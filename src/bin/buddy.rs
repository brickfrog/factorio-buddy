//! Standalone Factorio buddy runtime.
//!
//! This is intentionally a thin host around the Rust MCP server: it watches the
//! mod's chat inbox, gives Claude only the Factorio MCP tools, and sends the
//! final response back to the mod. Gameplay policy remains in the model and
//! gameplay implementation remains in Rust/Lua; there is no second planner or
//! memory system here.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use clap::Parser;
use factorioctl::client::{AgentId, FactorioClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, Instant, MissedTickBehavior};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_SYSTEM_PROMPT: &str = "You are an autonomous AI teammate inside a Factorio game. Use the Factorio MCP tools to observe and play the game through your own character. Act on player requests immediately. When idle, inspect the real game state and make concrete progress toward a functioning automated factory. Prioritize self-sustaining automation: build production chains that continuously gather, transport, process, and deliver resources without your character manually moving items. Use hand-crafting and manual item transfers only for bounded bootstrap or recovery, then replace them with automated production; never treat repeated hand-feeding as progress or completion. Build belts as complete source-to-destination routes with route_belt or a higher-level automation controller; do not improvise disconnected one-tile belt fragments. Treat live resource patches as extraction reserves: put only compatible mining drills or pumpjacks on them, place processing, storage, power, and ordinary logistics outside them, and route new belts around or underground. Use execute_edge_miner so new extraction begins with a clear output tile; existing overlap is not permission to extend it. Prefer dedicated item belts or deliberate lane separation; never assume a branch is pure because one sampled tile currently shows one item. Before tapping any belt that may carry multiple products, inspect its exact lanes; configure the receiving inserter's whitelist when one consumer must accept only specific items, but do not mistake a filtered inserter for a pure upstream belt. Treat planner output as an executable contract: when a plan returns exact mutation arguments, execute those exact arguments without substituting a search or approximate mutation. After a compound mutation, inspect the resulting state and correct or remove failed partial work before proceeding. Never claim an action succeeded unless a tool result confirms it. Keep final chat replies concise because they render in a small in-game panel.";

const AUTONOMY_DIRECTIVE: &str = "Autonomy tick: re-evaluate the whole factory from the authoritative snapshot below before acting. The factory is a set of independent subsystems that keep running while you work elsewhere. Choose from the current evidence, not from conversational momentum or the previous turn's focus. If research or another subsystem is healthy and progressing, leave it running; do not wait for it, repeatedly poll it, or keep embellishing it. Select the highest-leverage stalled or underdeveloped subsystem shown by the current data, inspect the relevant location with tools, take concrete action toward durable automation, and verify the result. Do not merely describe a plan.";

#[derive(Clone, Debug, Parser)]
#[command(about = "Run the autonomous Factorio buddy using the Rust MCP tool server")]
struct Args {
    #[arg(long, default_value = "default", env = "FACTORIO_AGENT_ID")]
    agent: String,

    #[arg(long)]
    label: Option<String>,

    #[arg(long, env = "MODEL")]
    model: Option<String>,

    #[arg(
        long,
        default_value = "low",
        env = "BUDDY_EFFORT",
        value_parser = ["low", "medium", "high", "xhigh", "max"]
    )]
    effort: String,

    #[arg(long, default_value = "localhost", env = "FACTORIO_RCON_HOST")]
    rcon_host: String,

    #[arg(long, default_value_t = 27015, env = "FACTORIO_RCON_PORT")]
    rcon_port: u16,

    #[arg(long, default_value_t = 34197, env = "FACTORIO_GAME_PORT")]
    game_port: u16,

    #[arg(long, env = "FACTORIO_RCON_PASSWORD")]
    rcon_password: Option<String>,

    #[arg(long, env = "FACTORIO_SCRIPT_OUTPUT")]
    script_output: Option<PathBuf>,

    /// Start and own a local headless Factorio server before starting the NPC.
    #[arg(long)]
    start_server: bool,

    /// Recreate the local save before starting the server.
    #[arg(long, requires = "start_server")]
    fresh: bool,

    #[arg(long, env = "FACTORIO_BIN")]
    factorio_bin: Option<PathBuf>,

    #[arg(long, default_value = ".factorio-buddy", env = "FACTORIO_WRITE_DATA")]
    write_data: PathBuf,

    #[arg(long)]
    save: Option<PathBuf>,

    #[arg(long, env = "FACTORIOCTL_MCP")]
    mcp_bin: Option<PathBuf>,

    /// Seconds between autonomous turns. Set to 0 for chat-only operation.
    #[arg(long, default_value_t = 30, env = "BUDDY_HEARTBEAT_SECONDS")]
    heartbeat_seconds: u64,

    #[arg(long, default_value_t = true, env = "AUTONOMY_REQUIRES_PLAYER")]
    autonomy_requires_player: bool,

    /// Optional whole-turn timeout. Zero leaves a progressing turn uncapped;
    /// player input and shutdown can still cancel it immediately.
    #[arg(long, default_value_t = 0, env = "BUDDY_TURN_TIMEOUT_SECONDS")]
    turn_timeout_seconds: u64,

    #[arg(long, default_value = DEFAULT_SYSTEM_PROMPT)]
    system_prompt: String,
}

#[derive(Clone, Debug, Deserialize)]
struct InputMessage {
    #[serde(default)]
    id: Option<u64>,
    message: String,
    #[serde(default = "default_player_index")]
    player_index: u32,
    #[serde(default = "default_agent")]
    target_agent: String,
    response_to: Option<String>,
}

fn default_player_index() -> u32 {
    1
}
fn default_agent() -> String {
    "default".to_owned()
}

#[derive(Debug, Deserialize)]
struct ClaudeResult {
    #[serde(default)]
    result: String,
    session_id: Option<String>,
    #[serde(default)]
    is_error: bool,
}

struct ClaudeReply {
    text: String,
    already_delivered: bool,
}

struct Inbox {
    path: PathBuf,
    cursor_path: PathBuf,
    offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnKind {
    Human,
    Autonomy,
}

#[derive(Clone, Debug)]
struct TurnRequest {
    kind: TurnKind,
    prompt: Option<String>,
    player_index: u32,
    response_agent: String,
}

struct TurnCompletion {
    session_id: Option<String>,
    succeeded: bool,
}

struct ActiveTurn {
    kind: TurnKind,
    handle: JoinHandle<TurnCompletion>,
}

struct LifecycleClient {
    host: String,
    port: u16,
    password: String,
    agent: String,
    client: Mutex<Option<FactorioClient>>,
}

struct ControllerLease {
    file: File,
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SaveIdentity {
    size: u64,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SaveOwner {
    version: u32,
    primary_save: String,
    primary_identity: SaveIdentity,
    run_id: String,
    run_directory: String,
    clean_shutdown: bool,
}

struct LocalServer {
    child: Child,
    // Factorio exits when stdin reaches EOF, so retain the pipe for the life of
    // the server even though the buddy never writes console commands to it.
    _stdin: tokio::process::ChildStdin,
    output_task: JoinHandle<()>,
    save_owner_path: PathBuf,
    save_owner: SaveOwner,
}

impl LocalServer {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("failed to inspect Factorio server process")
    }

    async fn stop(mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                warn!(%status, "Factorio server had already exited");
                let _ = self.output_task.await;
                return;
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "failed to inspect Factorio server before shutdown"),
        }

        info!("requesting Factorio shutdown and final save");
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The server runs in its own process group, so terminal Ctrl-C only
            // reaches Buddy. Deliver one SIGINT here and then give Factorio time
            // to finish its normal save-before-exit path.
            let result = unsafe { libc::kill(pid as i32, libc::SIGINT) };
            if result != 0 {
                warn!(error = %std::io::Error::last_os_error(), pid, "failed to interrupt Factorio server");
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.start_kill();

        let clean_shutdown = match timeout(Duration::from_secs(60), self.child.wait()).await {
            Ok(Ok(status)) => {
                info!(%status, "Factorio server stopped after final save");
                status.success()
            }
            Ok(Err(error)) => {
                warn!(%error, "failed while waiting for Factorio server to stop");
                false
            }
            Err(_) => {
                warn!("Factorio did not stop within 60 seconds; forcing shutdown");
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
                false
            }
        };
        if clean_shutdown {
            self.save_owner.clean_shutdown = true;
            if let Err(error) = write_json_atomic(&self.save_owner_path, &self.save_owner, false) {
                warn!(%error, "failed to record clean Factorio shutdown");
            }
        }
        let _ = self.output_task.await;
    }
}

impl ControllerLease {
    fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open controller lease {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                bail!(
                    "another Buddy controller already owns agent lease {}: {error}",
                    path.display()
                );
            }
        }

        #[cfg(not(unix))]
        if file.metadata()?.len() != 0 {
            bail!(
                "another Buddy controller may already own agent lease {}",
                path.display()
            );
        }

        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(Self { file, path })
    }
}

impl Drop for ControllerLease {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(not(unix))]
        {
            let _ = self.file.set_len(0);
        }
        let _ = &self.path;
    }
}

fn atomic_write(path: &Path, contents: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("atomic write destination has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let staged = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    if staged.exists() {
        std::fs::remove_file(&staged)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(&staged)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<()> {
    let mut encoded = serde_json::to_vec_pretty(value)?;
    encoded.push(b'\n');
    atomic_write(path, &encoded, private)
}

fn password_path(write_data: &Path) -> PathBuf {
    write_data.join("rcon-password")
}

fn generate_password() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("failed to generate RCON password: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn configure_rcon_password(args: &mut Args) -> Result<()> {
    std::fs::create_dir_all(&args.write_data)?;
    let path = password_path(&args.write_data);

    if args.start_server {
        if !matches!(args.rcon_host.as_str(), "localhost" | "127.0.0.1" | "::1") {
            bail!(
                "an owned Factorio server must use loopback RCON, not {}",
                args.rcon_host
            );
        }
        args.rcon_host = "127.0.0.1".to_owned();
        let password = match args.rcon_password.take() {
            Some(password) if !password.trim().is_empty() => password,
            Some(_) => bail!("RCON password cannot be empty"),
            None => match std::fs::read_to_string(&path) {
                Ok(password) if !password.trim().is_empty() => password.trim().to_owned(),
                _ => generate_password()?,
            },
        };
        atomic_write(&path, format!("{password}\n").as_bytes(), true)?;
        args.rcon_password = Some(password);
        return Ok(());
    }

    if args
        .rcon_password
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        bail!("RCON password cannot be empty");
    }
    if args.rcon_password.is_none() {
        let password = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no RCON password supplied; set FACTORIO_RCON_PASSWORD or start the managed server once (missing {})",
                path.display()
            )
        })?;
        if password.trim().is_empty() {
            bail!("managed RCON password file is empty: {}", path.display());
        }
        args.rcon_password = Some(password.trim().to_owned());
    }
    Ok(())
}

fn rcon_password(args: &Args) -> Result<&str> {
    args.rcon_password
        .as_deref()
        .context("RCON password was not configured")
}

fn should_forward_factorio_output(line: &str) -> bool {
    !line.contains("New RCON connection")
}

async fn forward_factorio_output(reader: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if should_forward_factorio_output(&line) => println!("{line}"),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                warn!(%error, "failed to read Factorio server output");
                break;
            }
        }
    }
}

impl Inbox {
    fn new(path: PathBuf, cursor_path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file_len = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let offset = match std::fs::read_to_string(&cursor_path) {
            Ok(cursor) => {
                let stored = cursor
                    .trim()
                    .parse::<u64>()
                    .with_context(|| format!("invalid inbox cursor {}", cursor_path.display()))?;
                if stored > file_len {
                    0
                } else {
                    stored
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let inbox = Self {
            path,
            cursor_path,
            offset,
        };
        inbox.persist_cursor()?;
        Ok(inbox)
    }

    fn persist_cursor(&self) -> Result<()> {
        atomic_write(
            &self.cursor_path,
            format!("{}\n", self.offset).as_bytes(),
            false,
        )
    }

    fn poll(&mut self) -> Result<Vec<InputMessage>> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return Ok(Vec::new());
        };
        if metadata.len() < self.offset {
            self.offset = 0;
            self.persist_cursor()?;
        }
        if metadata.len() == self.offset {
            return Ok(Vec::new());
        }
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)?;
        let Some(last_newline) = chunk.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let complete = String::from_utf8_lossy(&chunk[..=last_newline]);
        self.offset += (last_newline + 1) as u64;
        self.persist_cursor()?;
        Ok(parse_input(&complete))
    }
}

fn parse_input(chunk: &str) -> Vec<InputMessage> {
    chunk
        .lines()
        .filter_map(|line| {
            let message: InputMessage = match serde_json::from_str(line) {
                Ok(message) => message,
                Err(error) => {
                    warn!(%error, line, "ignored malformed chat inbox record");
                    return None;
                }
            };
            (!message.message.trim().is_empty()).then_some(message)
        })
        .collect()
}

fn find_mcp(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .with_context(|| format!("MCP binary not found: {}", path.display()));
    }
    let current = std::env::current_exe()?;
    let sibling = current.with_file_name(if cfg!(windows) { "mcp.exe" } else { "mcp" });
    if sibling.is_file() {
        return Ok(sibling);
    }
    which::which("factorioctl-mcp")
        .or_else(|_| which::which("mcp"))
        .context("factorioctl MCP binary not found; build with `cargo build --release`")
}

fn default_script_output() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".factorio-buddy/script-output")
}

fn find_factorio(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .with_context(|| format!("Factorio binary not found: {}", path.display()));
    }
    if let Ok(path) = which::which("factorio") {
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    [
        PathBuf::from("/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio"),
        home.join(".local/share/Steam/steamapps/common/Factorio/bin/x64/factorio"),
        home.join(".steam/steam/steamapps/common/Factorio/bin/x64/factorio"),
        PathBuf::from("/opt/factorio/bin/x64/factorio"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .context("Factorio binary not found; set FACTORIO_BIN=/path/to/factorio")
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn trees_equal(left: &Path, right: &Path) -> Result<bool> {
    if !left.is_dir() || !right.is_dir() {
        return Ok(false);
    }
    let mut left_entries = std::fs::read_dir(left)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut right_entries = std::fs::read_dir(right)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    left_entries.sort();
    right_entries.sort();
    if left_entries != right_entries {
        return Ok(false);
    }
    for name in left_entries {
        let left_path = left.join(&name);
        let right_path = right.join(&name);
        let left_type = std::fs::symlink_metadata(&left_path)?.file_type();
        let right_type = std::fs::symlink_metadata(&right_path)?.file_type();
        if left_type.is_dir() != right_type.is_dir() || left_type.is_file() != right_type.is_file()
        {
            return Ok(false);
        }
        if left_type.is_dir() {
            if !trees_equal(&left_path, &right_path)? {
                return Ok(false);
            }
        } else if left_type.is_file() && std::fs::read(&left_path)? != std::fs::read(&right_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn install_mod_source(source: &Path, mods_dir: &Path) -> Result<bool> {
    let destination = mods_dir.join("claude-interface");
    if trees_equal(source, &destination)? {
        return Ok(false);
    }

    std::fs::create_dir_all(mods_dir)?;
    let staged = mods_dir.join(format!(
        ".claude-interface.installing-{}",
        std::process::id()
    ));
    let backup = mods_dir.join(format!(".claude-interface.backup-{}", std::process::id()));
    if staged.exists() {
        std::fs::remove_dir_all(&staged)?;
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    copy_tree(source, &staged)
        .with_context(|| format!("failed to stage Factorio mod from {}", source.display()))?;

    let had_destination = destination.exists();
    if had_destination {
        std::fs::rename(&destination, &backup).with_context(|| {
            format!("failed to preserve installed mod {}", destination.display())
        })?;
    }
    if let Err(error) = std::fs::rename(&staged, &destination) {
        if had_destination {
            let _ = std::fs::rename(&backup, &destination);
        }
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error)
            .with_context(|| format!("failed to activate Factorio mod {}", destination.display()));
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    Ok(true)
}

fn install_mod_into(mods_dir: &Path) -> Result<bool> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mod/claude-interface");
    install_mod_source(&source, mods_dir)
        .with_context(|| format!("failed to install Factorio mod from {}", source.display()))
}

#[cfg(target_os = "linux")]
fn factorio_client_running() -> bool {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };
    processes.filter_map(Result::ok).any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            return false;
        };
        let Ok(command_line) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let args = command_line
            .split(|byte| *byte == 0)
            .filter_map(|arg| std::str::from_utf8(arg).ok())
            .collect::<Vec<_>>();
        let is_factorio = args
            .first()
            .is_some_and(|program| program.rsplit('/').next() == Some("factorio"));
        is_factorio
            && !args
                .iter()
                .any(|arg| matches!(*arg, "--start-server" | "--create"))
    })
}

#[cfg(not(target_os = "linux"))]
fn factorio_client_running() -> bool {
    false
}

fn install_mods(write_data: &Path) -> Result<()> {
    if install_mod_into(&write_data.join("mods"))? {
        info!(path = %write_data.join("mods").display(), "installed Factorio server mod");
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let client_mods = home.join(".factorio/mods");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mod/claude-interface");
        let destination = client_mods.join("claude-interface");
        if !trees_equal(&source, &destination)? {
            if factorio_client_running() {
                bail!(
                    "the Factorio client is running with a different Buddy mod; close it and rerun the same command so the synchronized mod can be installed"
                );
            }
            install_mod_source(&source, &client_mods)
                .context("failed to install Buddy mod for the Factorio client")?;
            info!(path = %client_mods.display(), "installed Factorio client mod");
        }
    }
    Ok(())
}

fn newest_autosave(directory: &Path) -> Result<Option<(PathBuf, SystemTime)>> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(None);
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("_autosave")
            || path.extension().and_then(|ext| ext.to_str()) != Some("zip")
        {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if newest
            .as_ref()
            .is_none_or(|(newest_path, newest_modified)| {
                modified > *newest_modified || (modified == *newest_modified && path > *newest_path)
            })
        {
            newest = Some((path, modified));
        }
    }
    Ok(newest)
}

fn normalized_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", path.display()));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().context("path has no parent directory")?;
    let parent = if parent.exists() {
        parent.canonicalize()?
    } else {
        parent.to_owned()
    };
    Ok(parent.join(absolute.file_name().context("path has no file name")?))
}

fn save_identity(path: &Path) -> Result<SaveIdentity> {
    // A stable content identity is sufficient here: it prevents an ownership
    // sidecar left beside one save from authorizing recovery over replacement
    // content at the same path. Run isolation below is the stronger ownership
    // boundary for autosaves themselves.
    const FNV_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    let mut file = File::open(path)
        .with_context(|| format!("failed to open save identity source {}", path.display()))?;
    let mut digest = FNV_OFFSET_BASIS;
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("save is too large to identify")?;
        for byte in &buffer[..read] {
            digest ^= u128::from(*byte);
            digest = digest.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(SaveIdentity {
        size,
        digest: format!("fnv1a128:{digest:032x}"),
    })
}

fn save_owner_path(save: &Path) -> Result<PathBuf> {
    let parent = save.parent().context("save path has no parent directory")?;
    let mut name = save
        .file_name()
        .context("save path has no file name")?
        .to_os_string();
    name.push(".buddy-owner.json");
    Ok(parent.join(name))
}

fn valid_run_id(run_id: &str) -> bool {
    run_id.len() == 64 && run_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn promote_owned_autosave(
    save: &Path,
    owner_path: &Path,
    managed_runs_root: &Path,
) -> Result<Option<PathBuf>> {
    let owner: SaveOwner = match std::fs::read(owner_path) {
        Ok(encoded) => serde_json::from_slice(&encoded)
            .with_context(|| format!("invalid save ownership file {}", owner_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if owner.version != 2 || owner.clean_shutdown {
        return Ok(None);
    }
    let requested = normalized_path(save)?;
    if Path::new(&owner.primary_save) != requested {
        warn!(
            requested = %requested.display(),
            owner = %owner.primary_save,
            "refusing to promote an autosave owned by a different primary save"
        );
        return Ok(None);
    }

    if save.exists() {
        let current_identity = save_identity(save)?;
        if current_identity.size != owner.primary_identity.size
            || current_identity.digest != owner.primary_identity.digest
        {
            warn!(
                save = %save.display(),
                "refusing to promote an autosave over primary save content that changed outside the owned run"
            );
            return Ok(None);
        }
    }

    if !valid_run_id(&owner.run_id) {
        warn!(run_id = %owner.run_id, "refusing invalid autosave run identity");
        return Ok(None);
    }
    let expected_run_directory = normalized_path(&managed_runs_root.join(&owner.run_id))?;
    if Path::new(&owner.run_directory) != expected_run_directory {
        warn!(
            expected = %expected_run_directory.display(),
            owner = %owner.run_directory,
            "refusing autosaves outside the owned run directory"
        );
        return Ok(None);
    }

    let Some((autosave, autosave_modified)) =
        newest_autosave(&expected_run_directory.join("saves"))?
    else {
        return Ok(None);
    };
    if save.exists() {
        let save_modified = std::fs::metadata(save)?.modified()?;
        if save_modified >= autosave_modified {
            return Ok(None);
        }
    }

    let directory = save.parent().context("save path has no parent directory")?;
    let stem = save
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("save");
    let backup = directory.join(format!("{stem}.previous.zip"));
    let staged = directory.join(format!(".{stem}.recovering.zip"));

    if save.exists() {
        std::fs::copy(save, &backup).with_context(|| {
            format!(
                "failed to preserve stale save {} as {}",
                save.display(),
                backup.display()
            )
        })?;
    }
    if staged.exists() {
        std::fs::remove_file(&staged)?;
    }
    std::fs::copy(&autosave, &staged).with_context(|| {
        format!(
            "failed to stage autosave {} as {}",
            autosave.display(),
            staged.display()
        )
    })?;
    std::fs::File::open(&staged)?.sync_all()?;
    std::fs::rename(&staged, save).with_context(|| {
        format!(
            "failed to promote autosave {} to {}",
            autosave.display(),
            save.display()
        )
    })?;

    info!(
        source = %autosave.display(),
        save = %save.display(),
        backup = %backup.display(),
        "promoted newer autosave before resume"
    );
    Ok(Some(autosave))
}

async fn start_local_server(args: &mut Args) -> Result<LocalServer> {
    if tokio::net::TcpStream::connect((&*args.rcon_host, args.rcon_port))
        .await
        .is_ok()
    {
        bail!(
            "RCON port {} is already in use; stop the existing Factorio server before `just play`",
            args.rcon_port
        );
    }
    let factorio = find_factorio(args.factorio_bin.clone())?;
    let write_data = std::fs::canonicalize(&args.write_data).or_else(|_| {
        std::fs::create_dir_all(&args.write_data)?;
        std::fs::canonicalize(&args.write_data)
    })?;
    install_mods(&write_data)?;
    std::fs::create_dir_all(write_data.join("saves"))?;

    let data_root = factorio
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("cannot derive Factorio data directory from binary path")?;
    let save = args
        .save
        .clone()
        .unwrap_or_else(|| write_data.join("saves/buddy.zip"));
    if let Some(parent) = save.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let save = normalized_path(&save)?;
    let save_owner_path = save_owner_path(&save)?;
    let managed_runs_root = write_data.join("managed-runs");
    std::fs::create_dir_all(&managed_runs_root)?;
    if args.fresh && save.exists() {
        std::fs::remove_file(&save)
            .with_context(|| format!("failed to remove old save: {}", save.display()))?;
    }
    if !args.fresh {
        promote_owned_autosave(&save, &save_owner_path, &managed_runs_root)?;
    }

    let run_id = generate_password()?;
    let run_directory = managed_runs_root.join(&run_id);
    std::fs::create_dir_all(run_directory.join("saves"))?;
    let run_directory = normalized_path(&run_directory)?;
    let config = run_directory.join("config.ini");
    atomic_write(
        &config,
        format!(
            "[path]\nread-data={}\nwrite-data={}\n\n[other]\ncheck-updates=false\n",
            data_root.join("data").display(),
            run_directory.display()
        )
        .as_bytes(),
        true,
    )?;
    if !save.exists() {
        info!(save = %save.display(), "creating Factorio save");
        let status = Command::new(&factorio)
            .arg("--config")
            .arg(&config)
            .arg("--mod-directory")
            .arg(write_data.join("mods"))
            .arg("--create")
            .arg(&save)
            .arg("--map-gen-settings")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/map-gen.json"))
            .status()
            .await?;
        if !status.success() {
            if !save.is_file() {
                bail!("Factorio failed to create save: {status}");
            }
            warn!(%status, "Factorio created the save but reported a non-zero create status");
        }
    }

    let save_owner = SaveOwner {
        version: 2,
        primary_save: normalized_path(&save)?.to_string_lossy().into_owned(),
        primary_identity: save_identity(&save)?,
        run_id,
        run_directory: run_directory.to_string_lossy().into_owned(),
        clean_shutdown: false,
    };
    write_json_atomic(&save_owner_path, &save_owner, false)?;
    args.script_output = Some(run_directory.join("script-output"));

    let mut server_command = Command::new(&factorio);
    server_command
        .arg("--config")
        .arg(&config)
        .arg("--mod-directory")
        .arg(write_data.join("mods"))
        .arg("--start-server")
        .arg(&save)
        .arg("--rcon-bind")
        .arg(format!("127.0.0.1:{}", args.rcon_port))
        .arg("--rcon-password")
        .arg(rcon_password(args)?)
        .arg("--port")
        .arg(args.game_port.to_string())
        .arg("--server-settings")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/server.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    server_command.process_group(0);
    let mut child = server_command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .context("Factorio stdin pipe unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("Factorio stdout pipe unavailable")?;
    let output_task = tokio::spawn(forward_factorio_output(stdout));

    for _ in 0..60 {
        if let Some(status) = child.try_wait()? {
            bail!("Factorio server exited during startup: {status}");
        }
        if FactorioClient::connect(&args.rcon_host, args.rcon_port, rcon_password(args)?)
            .await
            .is_ok()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(status) = child.try_wait()? {
                bail!("Factorio server exited after opening RCON: {status}");
            }
            info!(save = %save.display(), "Factorio server ready");
            return Ok(LocalServer {
                child,
                _stdin: stdin,
                output_task,
                save_owner_path,
                save_owner,
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill().await;
    bail!("Factorio server did not open RCON within 30 seconds")
}

fn mcp_config(args: &Args, mcp: &Path) -> String {
    json!({"mcpServers": {"factorio": {
        "command": mcp,
        "env": {
            "FACTORIO_RCON_HOST": args.rcon_host,
            "FACTORIO_RCON_PORT": args.rcon_port.to_string(),
            "FACTORIO_RCON_PASSWORD": rcon_password(args).expect("password configured"),
            "FACTORIO_AGENT_ID": args.agent,
            "FACTORIO_BUDDY_PROJECT_ROOT": env!("CARGO_MANIFEST_DIR"),
        }
    }}})
    .to_string()
}

fn write_mcp_config(args: &Args, mcp: &Path) -> Result<PathBuf> {
    let path = args.write_data.join(format!("mcp-{}.json", args.agent));
    atomic_write(&path, mcp_config(args, mcp).as_bytes(), true)?;
    Ok(path)
}

fn validate_lifecycle_response(function: &str, response: &str) -> Result<()> {
    let trimmed = response.trim();
    if function == "ping" {
        if trimmed != "pong" {
            bail!("lifecycle ping returned {trimmed:?}, expected \"pong\"");
        }
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(());
    };
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        let kind = value
            .get("error_kind")
            .and_then(Value::as_str)
            .unwrap_or("remote_error");
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("remote lifecycle call failed");
        bail!("{function} failed ({kind}): {message}");
    }
    if let Some(kind) = value.get("error_kind").and_then(Value::as_str) {
        bail!("{function} failed ({kind})");
    }
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        if !message.is_empty() {
            bail!("{function} failed: {message}");
        }
    }
    if function == "pre_place_character_result" {
        match value.get("status").and_then(Value::as_str) {
            Some("created" | "already_placed") => {}
            Some(status) => bail!("pre_place_character_result failed with status {status}"),
            None => bail!("pre_place_character_result returned no status"),
        }
    }
    Ok(())
}

fn lifecycle_is_read_only(function: &str) -> bool {
    matches!(
        function,
        "ping" | "connected_player_count_result" | "autonomy_snapshot"
    )
}

impl LifecycleClient {
    fn new(args: &Args) -> Result<Self> {
        AgentId::new(Some(&args.agent))?;
        Ok(Self {
            host: args.rcon_host.clone(),
            port: args.rcon_port,
            password: rcon_password(args)?.to_owned(),
            agent: args.agent.clone(),
            client: Mutex::new(None),
        })
    }

    async fn call(&self, function: &str, values: &[Value]) -> Result<String> {
        let attempts = if lifecycle_is_read_only(function) {
            2
        } else {
            1
        };
        let mut last_error = None;
        for attempt in 0..attempts {
            let mut guard = self.client.lock().await;
            let mut client = match guard.take() {
                Some(client) => client,
                None => {
                    let agent_id = AgentId::new(Some(&self.agent))?;
                    match FactorioClient::connect(&self.host, self.port, &self.password).await {
                        Ok(client) => client.with_agent_id(agent_id),
                        Err(error) => {
                            last_error = Some(error);
                            if attempt + 1 < attempts {
                                warn!(function, "lifecycle RCON connection failed; retrying once");
                                continue;
                            }
                            break;
                        }
                    }
                }
            };
            // Keep the slot empty while an RCON operation is in flight. If the
            // task is cancelled, the local client is dropped instead of
            // returning a potentially half-read protocol stream to the pool.
            let response = client.call_remote(function, values).await;
            match response {
                Ok(response) => {
                    *guard = Some(client);
                    validate_lifecycle_response(function, &response)?;
                    return Ok(response);
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < attempts {
                        warn!(function, "lifecycle RCON disconnected; reconnecting once");
                    }
                }
            }
        }
        let error = last_error.context("lifecycle call failed without a transport error")?;
        Err(error).with_context(|| format!("lifecycle call {function} failed"))
    }
}

fn connected_player_count(value: &str) -> u64 {
    let Ok(value) = serde_json::from_str::<Value>(value) else {
        return 0;
    };
    value
        .as_u64()
        .or_else(|| value.get("count").and_then(Value::as_u64))
        .or_else(|| value.get("connected_players").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn stream_event_session_id(event: &Value) -> Option<&str> {
    event.get("session_id").and_then(Value::as_str)
}

fn autonomy_prompt(snapshot: &str) -> String {
    let formatted_snapshot = serde_json::from_str::<Value>(snapshot)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| snapshot.to_owned());
    format!("{AUTONOMY_DIRECTIVE}\n\nAuthoritative current factory snapshot:\n{formatted_snapshot}")
}

async fn collect_autonomy_prompt(args: &Args, lifecycle: &LifecycleClient) -> String {
    match lifecycle
        .call("autonomy_snapshot", &[json!(args.agent)])
        .await
    {
        Ok(snapshot) => autonomy_prompt(&snapshot),
        Err(error) => {
            warn!(%error, "failed to collect autonomy snapshot");
            format!("{AUTONOMY_DIRECTIVE}\n\nThe automatic snapshot failed. Inspect the whole factory with read-only tools before choosing what to work on.")
        }
    }
}

fn claude_arguments(
    args: &Args,
    config: &str,
    prompt: &str,
    session_id: Option<&str>,
) -> Vec<OsString> {
    let mut arguments = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        config.into(),
        "--permission-mode".into(),
        "bypassPermissions".into(),
        "--allowedTools".into(),
        "mcp__factorio__*".into(),
        "--disallowedTools".into(),
        "mcp__factorio__execute_lua".into(),
        "--tools".into(),
        "".into(),
        "--setting-sources".into(),
        "".into(),
        "--disable-slash-commands".into(),
        "--effort".into(),
        args.effort.clone().into(),
        "--system-prompt".into(),
        args.system_prompt.clone().into(),
    ];
    if let Some(model) = &args.model {
        arguments.push("--model".into());
        arguments.push(model.into());
    }
    if let Some(session_id) = session_id {
        arguments.push("--resume".into());
        arguments.push(session_id.into());
    }
    arguments.push("--".into());
    arguments.push(prompt.into());
    arguments
}

async fn invoke_claude(
    args: &Args,
    config: &str,
    lifecycle: &LifecycleClient,
    prompt: &str,
    player_index: u32,
    response_agent: &str,
    session_id: &mut Option<String>,
) -> Result<ClaudeReply> {
    let started = Instant::now();
    info!(event = "turn_start", session = session_id.as_deref().unwrap_or("new"), prompt = %prompt, "Claude turn started");
    let mut command = Command::new("claude");
    command
        .args(claude_arguments(
            args,
            config,
            prompt,
            session_id.as_deref(),
        ))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("failed to start `claude`; install/authenticate Claude Code")?;
    let stdout = child.stdout.take().context("claude stdout unavailable")?;
    let stderr = child.stderr.take().context("claude stderr unavailable")?;
    let observed_session_id = Arc::new(StdMutex::new(session_id.clone()));
    let streamed_session_id = Arc::clone(&observed_session_id);

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            warn!(event = "model_stderr", message = %line, "Claude stderr");
        }
    });

    let stream = async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut final_result = None;
        let mut delivered_text = Vec::new();
        let mut tool_names: HashMap<String, String> = HashMap::new();
        while let Some(line) = lines.next_line().await? {
            let event: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => {
                    warn!(event = "model_protocol_error", %error, raw = %line, "Invalid Claude event");
                    continue;
                }
            };
            if let Some(id) = stream_event_session_id(&event) {
                if let Ok(mut observed) = streamed_session_id.lock() {
                    *observed = Some(id.to_owned());
                }
            }
            match event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "assistant" => {
                    if let Some(blocks) =
                        event.pointer("/message/content").and_then(Value::as_array)
                    {
                        for block in blocks {
                            match block
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                            {
                                "tool_use" => {
                                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                    let tool = block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown");
                                    let arguments =
                                        block.get("input").cloned().unwrap_or(Value::Null);
                                    tool_names.insert(id.to_owned(), tool.to_owned());
                                    info!(event = "tool_call", tool, tool_use_id = id, arguments = %arguments, "Tool call");
                                }
                                "text" => {
                                    let text = block
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .trim();
                                    info!(event = "model_text", text, "Claude text");
                                    if !text.is_empty() {
                                        match lifecycle
                                            .call(
                                                "receive_response",
                                                &[
                                                    json!(player_index),
                                                    json!(response_agent),
                                                    json!(text),
                                                ],
                                            )
                                            .await
                                        {
                                            Ok(_) => delivered_text.push(text.to_owned()),
                                            Err(error) => warn!(
                                                %error,
                                                "failed to stream Claude text to Factorio"
                                            ),
                                        }
                                    }
                                }
                                "thinking" => {
                                    info!(event = "model_thinking", thinking = %block.get("thinking").and_then(|value| value.as_str()).unwrap_or(""), "Claude thinking")
                                }
                                kind => {
                                    info!(event = "model_content", content_type = kind, payload = %block, "Claude content")
                                }
                            }
                        }
                    }
                }
                "user" => {
                    if let Some(blocks) =
                        event.pointer("/message/content").and_then(Value::as_array)
                    {
                        for block in blocks {
                            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                                let id = block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                let is_error = block
                                    .get("is_error")
                                    .and_then(|value| value.as_bool())
                                    .unwrap_or(false);
                                let result = block.get("content").cloned().unwrap_or(Value::Null);
                                info!(event = "tool_result", tool = tool_names.get(id).map(String::as_str).unwrap_or("unknown"), tool_use_id = id, is_error, result = %result, "Tool result");
                            }
                        }
                    }
                }
                "result" => {
                    info!(event = "model_result", payload = %event, "Claude result");
                    final_result = serde_json::from_value::<ClaudeResult>(event).ok();
                }
                kind => {
                    info!(event = "model_event", event_type = kind, payload = %event, "Claude event")
                }
            }
        }
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status, final_result, delivered_text))
    };

    let outcome = if args.turn_timeout_seconds == 0 {
        stream.await
    } else {
        match timeout(Duration::from_secs(args.turn_timeout_seconds), stream).await {
            Ok(result) => result,
            Err(error) => {
                stderr_task.abort();
                let _ = stderr_task.await;
                return Err(error).context("claude turn exceeded the wall-clock timeout");
            }
        }
    };
    if let Ok(observed) = observed_session_id.lock() {
        if let Some(id) = observed.as_deref() {
            if session_id.as_deref() != Some(id) {
                info!(event = "session", session = id, "Claude session observed");
                *session_id = Some(id.to_owned());
            }
        }
    }
    let (status, parsed, delivered_text) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            stderr_task.abort();
            let _ = stderr_task.await;
            return Err(error);
        }
    };
    let _ = stderr_task.await;
    info!(event = "turn_exit", exit_status = %status, duration_ms = started.elapsed().as_millis(), "Claude process exited");

    if let Some(parsed) = parsed {
        if let Some(id) = parsed.session_id {
            info!(event = "session", session = %id, "Claude session active");
            *session_id = Some(id);
        }
        if parsed.is_error {
            bail!("claude returned an error: {}", parsed.result);
        }
        if !parsed.result.trim().is_empty() {
            info!(event = "turn_complete", response = %parsed.result, "Claude turn completed");
            let already_delivered = delivered_text
                .iter()
                .any(|text| text.trim() == parsed.result.trim());
            return Ok(ClaudeReply {
                text: parsed.result,
                already_delivered,
            });
        }
    }
    if !status.success() {
        bail!("claude exited {status} without a valid result");
    }
    bail!("claude returned no final result")
}

async fn handle_turn(
    args: Arc<Args>,
    config: Arc<str>,
    lifecycle: Arc<LifecycleClient>,
    request: TurnRequest,
    mut session_id: Option<String>,
) -> TurnCompletion {
    let prompt = request
        .prompt
        .unwrap_or_else(|| String::from(AUTONOMY_DIRECTIVE));
    let _ = lifecycle
        .call(
            "set_status",
            &[
                json!(request.player_index),
                json!("[color=0.8,0.7,0.2]Thinking...[/color]"),
            ],
        )
        .await;

    let mut result = invoke_claude(
        &args,
        &config,
        &lifecycle,
        &prompt,
        request.player_index,
        &request.response_agent,
        &mut session_id,
    )
    .await;
    if result
        .as_ref()
        .err()
        .is_some_and(|error| session_id.is_some() && is_invalid_session_error(error))
    {
        warn!("Claude session was unavailable; retrying this turn without --resume");
        session_id = None;
        result = invoke_claude(
            &args,
            &config,
            &lifecycle,
            &prompt,
            request.player_index,
            &request.response_agent,
            &mut session_id,
        )
        .await;
    }

    let succeeded = match result {
        Ok(reply) => {
            if !reply.already_delivered {
                if let Err(error) = lifecycle
                    .call(
                        "receive_response",
                        &[
                            json!(request.player_index),
                            json!(request.response_agent),
                            json!(reply.text),
                        ],
                    )
                    .await
                {
                    warn!(%error, "failed to send response to Factorio");
                }
            }
            true
        }
        Err(error) => {
            warn!(%error, "agent turn failed");
            let _ = lifecycle
                .call(
                    "receive_response",
                    &[
                        json!(request.player_index),
                        json!(request.response_agent),
                        json!(format!("Agent error: {error}")),
                    ],
                )
                .await;
            false
        }
    };
    let _ = lifecycle
        .call(
            "set_status",
            &[
                json!(request.player_index),
                json!("[color=0.4,0.8,0.4]Ready[/color]"),
            ],
        )
        .await;
    TurnCompletion {
        session_id,
        succeeded,
    }
}

fn is_invalid_session_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    (message.contains("session") || message.contains("conversation"))
        && (message.contains("not found")
            || message.contains("invalid")
            || message.contains("unavailable"))
}

fn start_turn(
    args: Arc<Args>,
    config: Arc<str>,
    lifecycle: Arc<LifecycleClient>,
    mut request: TurnRequest,
    session_id: Option<String>,
) -> ActiveTurn {
    let kind = request.kind;
    let handle = tokio::spawn(async move {
        if kind == TurnKind::Autonomy {
            request.prompt = Some(collect_autonomy_prompt(&args, &lifecycle).await);
        }
        handle_turn(args, config, lifecycle, request, session_id).await
    });
    ActiveTurn { kind, handle }
}

async fn cancel_active_turn(active: &mut Option<ActiveTurn>, reason: &str) {
    let Some(turn) = active.take() else {
        return;
    };
    info!(kind = ?turn.kind, reason, "cancelling active Claude turn");
    turn.handle.abort();
    let _ = turn.handle.await;
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_buddy(args: Arc<Args>, local_server: &mut Option<LocalServer>) -> Result<()> {
    let mcp = find_mcp(args.mcp_bin.clone())?;
    let config_path = write_mcp_config(&args, &mcp)?;
    let config: Arc<str> = config_path.to_string_lossy().into_owned().into();
    let input = args
        .script_output
        .clone()
        .unwrap_or_else(default_script_output)
        .join("claude-chat/input.jsonl");
    let cursor = args.write_data.join(format!("inbox-{}.cursor", args.agent));
    let mut inbox = Inbox::new(input.clone(), cursor)?;
    let label = args.label.clone().unwrap_or_else(|| args.agent.clone());
    let lifecycle = Arc::new(LifecycleClient::new(&args)?);

    lifecycle
        .call("ping", &[])
        .await
        .context("Factorio Buddy mod is not reachable")?;
    lifecycle
        .call("register_agent", &[json!(args.agent), json!(label)])
        .await?;
    lifecycle
        .call(
            "pre_place_character_result",
            &[json!(args.agent), json!("nauvis"), json!(0)],
        )
        .await?;

    info!(
        agent = %args.agent,
        input = %input.display(),
        mcp = %mcp.display(),
        model = args.model.as_deref().unwrap_or("default"),
        effort = %args.effort,
        heartbeat_seconds = args.heartbeat_seconds,
        turn_timeout_seconds = args.turn_timeout_seconds,
        autonomy_requires_player = args.autonomy_requires_player,
        "Factorio buddy online"
    );
    let mut timer = interval(Duration::from_millis(500));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer.tick().await;
    let mut next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds.max(1));
    let mut session_id = None;
    let mut pending: VecDeque<TurnRequest> = VecDeque::new();
    let mut active: Option<ActiveTurn> = None;
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        if active.is_none() {
            if let Some(request) = pending.pop_front() {
                info!(kind = ?request.kind, "starting queued Claude turn");
                active = Some(start_turn(
                    Arc::clone(&args),
                    Arc::clone(&config),
                    Arc::clone(&lifecycle),
                    request,
                    session_id.clone(),
                ));
            }
        }

        tokio::select! {
            _ = &mut shutdown => {
                cancel_active_turn(&mut active, "shutdown").await;
                break;
            }
            completion = async {
                let handle = &mut active
                    .as_mut()
                    .expect("active turn guarded by select condition")
                    .handle;
                handle.await
            }, if active.is_some() => {
                let kind = active.take().expect("completed active turn").kind;
                match completion {
                    Ok(completion) => {
                        session_id = completion.session_id;
                        info!(?kind, succeeded = completion.succeeded, "Claude turn finished");
                    }
                    Err(error) if error.is_cancelled() => {
                        info!(?kind, "Claude turn cancelled");
                    }
                    Err(error) => warn!(?kind, %error, "Claude turn task failed"),
                }
                next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds.max(1));
            }
            _ = timer.tick() => {
                if let Some(server) = local_server.as_mut() {
                    if let Some(status) = server.try_wait()? {
                        cancel_active_turn(&mut active, "Factorio server exited").await;
                        bail!("owned Factorio server exited unexpectedly: {status}");
                    }
                }

                let messages = inbox.poll().unwrap_or_else(|error| {
                    warn!(%error, "failed to read Factorio chat inbox");
                    Vec::new()
                });
                let mut received_human_message = false;
                for message in messages {
                    if message.target_agent != args.agent && message.target_agent != "all" {
                        continue;
                    }
                    received_human_message = true;
                    let target = message
                        .response_to
                        .as_deref()
                        .unwrap_or(&args.agent)
                        .to_owned();
                    info!(
                        message_id = message.id,
                        player_index = message.player_index,
                        target_agent = %message.target_agent,
                        "received player message"
                    );
                    pending.push_back(TurnRequest {
                        kind: TurnKind::Human,
                        prompt: Some(message.message),
                        player_index: message.player_index,
                        response_agent: target,
                    });
                }
                if received_human_message {
                    pending.retain(|request| request.kind == TurnKind::Human);
                    cancel_active_turn(&mut active, "player message").await;
                    next_autonomy = Instant::now()
                        + Duration::from_secs(args.heartbeat_seconds.max(1));
                    continue;
                }

                if active.is_some()
                    || !pending.is_empty()
                    || args.heartbeat_seconds == 0
                    || Instant::now() < next_autonomy
                {
                    continue;
                }
                if args.autonomy_requires_player {
                    let count = lifecycle
                        .call("connected_player_count_result", &[])
                        .await
                        .map(|value| connected_player_count(&value))
                        .unwrap_or_else(|error| {
                            warn!(%error, "failed to check connected player count");
                            0
                        });
                    if count == 0 {
                        next_autonomy = Instant::now()
                            + Duration::from_secs(args.heartbeat_seconds.max(1));
                        continue;
                    }
                }
                pending.push_back(TurnRequest {
                    kind: TurnKind::Autonomy,
                    prompt: None,
                    player_index: 0,
                    response_agent: args.agent.clone(),
                });
                next_autonomy = Instant::now()
                    + Duration::from_secs(args.heartbeat_seconds.max(1));
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();
    let mut args = Args::parse();
    AgentId::new(Some(&args.agent)).context("invalid --agent")?;
    if args.start_server && args.script_output.is_none() {
        args.script_output = Some(args.write_data.join("script-output"));
    }
    let _lease =
        ControllerLease::acquire(args.write_data.join(format!("buddy-{}.lock", args.agent)))?;
    let _server_lease = if args.start_server {
        Some(ControllerLease::acquire(
            args.write_data.join("buddy-server.lock"),
        )?)
    } else {
        None
    };
    configure_rcon_password(&mut args)?;
    let mut local_server = if args.start_server {
        Some(start_local_server(&mut args).await?)
    } else {
        None
    };
    let result = run_buddy(Arc::new(args), &mut local_server).await;
    if let Some(server) = local_server.take() {
        info!("stopping Factorio server");
        server.stop().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const RUN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn write_owner(
        path: &Path,
        save: &Path,
        managed_runs_root: &Path,
        run_id: &str,
        clean_shutdown: bool,
    ) -> PathBuf {
        let run_directory = managed_runs_root.join(run_id);
        std::fs::create_dir_all(run_directory.join("saves")).unwrap();
        let owner = SaveOwner {
            version: 2,
            primary_save: normalized_path(save)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            primary_identity: save_identity(save).unwrap(),
            run_id: run_id.to_owned(),
            run_directory: normalized_path(&run_directory)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            clean_shutdown,
        };
        write_json_atomic(path, &owner, false).unwrap();
        run_directory
    }

    fn append(path: &Path, contents: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(contents).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn parses_valid_jsonl_and_ignores_noise() {
        let messages = parse_input(
            "noise\n{\"message\":\"mine coal\",\"player_index\":4}\n{\"message\":\"\"}\n",
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "mine coal");
        assert_eq!(messages[0].player_index, 4);
    }

    #[test]
    fn parses_connected_player_shapes() {
        assert_eq!(connected_player_count("2"), 2);
        assert_eq!(connected_player_count(r#"{"count":3}"#), 3);
        assert_eq!(connected_player_count("garbage"), 0);
    }

    #[test]
    fn reads_session_id_from_stream_events() {
        let event = json!({"type": "system", "subtype": "init", "session_id": "abc-123"});
        assert_eq!(stream_event_session_id(&event), Some("abc-123"));
        assert_eq!(stream_event_session_id(&json!({"type": "assistant"})), None);
    }

    #[test]
    fn hides_only_routine_rcon_connection_lines() {
        assert!(!should_forward_factorio_output(
            "Info RemoteCommandProcessor.cpp:245: New RCON connection from 127.0.0.1"
        ));
        assert!(should_forward_factorio_output(
            "Error RemoteCommandProcessor.cpp: RCON authentication failed"
        ));
        assert!(should_forward_factorio_output("Joining game"));
    }

    #[test]
    fn normal_factorio_chat_wakes_buddy_and_responses_do_not_require_the_gui() {
        let control = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/mod/claude-interface/control.lua"
        ));
        let chat_start = control
            .find("script.on_event(defines.events.on_console_chat")
            .expect("console chat handler");
        let chat_end = control[chat_start..]
            .find("-- Hotkey toggle")
            .map(|offset| chat_start + offset)
            .expect("end of console chat handler");
        assert!(control[chat_start..chat_end].contains("write_bridge_message("));

        let display_start = control
            .find("local function add_chat_message")
            .expect("chat display helper");
        let display_end = control[display_start..]
            .find("local function set_status")
            .map(|offset| display_start + offset)
            .expect("end of chat display helper");
        let display = &control[display_start..display_end];
        let print = display.find("player.print(").expect("console delivery");
        let gui_guard = display
            .find("player.gui.screen[GUI_FRAME]")
            .expect("optional GUI delivery");
        assert!(
            print < gui_guard,
            "console delivery must precede the GUI guard"
        );
    }

    #[test]
    fn default_prompt_requires_complete_belt_routes() {
        assert!(DEFAULT_SYSTEM_PROMPT
            .contains("Build belts as complete source-to-destination routes with route_belt"));
        assert!(
            DEFAULT_SYSTEM_PROMPT.contains("do not improvise disconnected one-tile belt fragments")
        );
    }

    #[test]
    fn default_prompt_requires_resource_preservation_and_explicit_belt_contents() {
        for required in [
            "Treat live resource patches as extraction reserves",
            "route new belts around or underground",
            "existing overlap is not permission to extend it",
            "Prefer dedicated item belts or deliberate lane separation",
            "never assume a branch is pure",
            "configure the receiving inserter's whitelist",
            "do not mistake a filtered inserter for a pure upstream belt",
        ] {
            assert!(
                DEFAULT_SYSTEM_PROMPT.contains(required),
                "default gameplay prompt should include {required:?}"
            );
        }
    }

    #[test]
    fn default_new_game_map_uses_peaceful_mode() {
        let settings: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/configs/map-gen.json"
        )))
        .expect("default map generation settings should be valid JSON");
        assert_eq!(
            settings
                .get("peaceful_mode")
                .and_then(|value| value.as_bool()),
            Some(true),
            "just play must create peaceful default maps"
        );
    }

    #[test]
    fn autonomy_prompt_requires_global_reprioritization_and_includes_snapshot() {
        let prompt = autonomy_prompt(r#"{"research":{"research_progress":0.5}}"#);
        assert!(prompt.contains("re-evaluate the whole factory"));
        assert!(prompt.contains("leave it running"));
        assert!(prompt.contains("\"research_progress\": 0.5"));
    }

    #[test]
    fn missing_inbox_cursor_replays_queued_complete_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        std::fs::write(
            &path,
            b"{\"id\":1,\"message\":\"first\"}\n{\"id\":2,\"message\":\"second\"}\n",
        )
        .unwrap();

        let mut inbox = Inbox::new(path, cursor).unwrap();
        let messages = inbox.poll().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, Some(1));
        assert_eq!(messages[1].message, "second");
    }

    #[test]
    fn inbox_does_not_consume_partial_jsonl_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        let mut inbox = Inbox::new(path.clone(), cursor).unwrap();

        append(&path, b"{\"id\":7,\"message\":\"still writing\"");
        assert!(inbox.poll().unwrap().is_empty());
        assert_eq!(inbox.offset, 0);
        append(&path, b"}\n");
        let messages = inbox.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, Some(7));
        assert_eq!(messages[0].message, "still writing");
    }

    #[test]
    fn inbox_resumes_from_durable_cursor_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        append(&path, b"{\"message\":\"first\"}\n");
        let mut first = Inbox::new(path.clone(), cursor.clone()).unwrap();
        assert_eq!(first.poll().unwrap().len(), 1);
        append(&path, b"{\"message\":\"second\"}\n");

        let mut resumed = Inbox::new(path, cursor).unwrap();
        let messages = resumed.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "second");
    }

    #[test]
    fn inbox_replays_a_recreated_shorter_file_instead_of_clamping_to_its_end() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        append(
            &path,
            b"{\"message\":\"a deliberately long message in the original file\"}\n",
        );
        let mut first = Inbox::new(path.clone(), cursor.clone()).unwrap();
        assert_eq!(first.poll().unwrap().len(), 1);
        std::fs::write(&path, b"{\"message\":\"new\"}\n").unwrap();

        let mut recreated = Inbox::new(path, cursor).unwrap();
        let messages = recreated.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "new");
    }

    #[test]
    fn claude_prompt_is_separated_from_options_and_settings_are_isolated() {
        let args = Args::try_parse_from(["buddy"]).unwrap();
        let arguments = claude_arguments(&args, "{}", "--help", Some("session-id"))
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&arguments[arguments.len() - 2..], ["--", "--help"]);
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--setting-sources", ""]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--disallowedTools", "mcp__factorio__execute_lua"]));
    }

    #[test]
    fn companion_autonomy_requires_a_connected_player_by_default() {
        let args = Args::try_parse_from(["buddy"]).unwrap();
        assert!(args.autonomy_requires_player);
        assert_eq!(args.turn_timeout_seconds, 0);
    }

    #[test]
    fn lifecycle_rejects_structured_failure_and_bad_character_status() {
        assert!(validate_lifecycle_response("ping", "not-pong").is_err());
        assert!(validate_lifecycle_response(
            "register_agent",
            r#"{"success":false,"error_kind":"unknown_function","error":"old mod"}"#
        )
        .is_err());
        assert!(validate_lifecycle_response(
            "pre_place_character_result",
            r#"{"status":"creation_failed"}"#
        )
        .is_err());
        assert!(validate_lifecycle_response(
            "pre_place_character_result",
            r#"{"status":"teleported"}"#
        )
        .is_err());
        validate_lifecycle_response("pre_place_character_result", r#"{"status":"created"}"#)
            .unwrap();
        validate_lifecycle_response("receive_response", "").unwrap();
    }

    #[test]
    fn controller_lease_excludes_a_second_controller() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("buddy.lock");
        let first = ControllerLease::acquire(path.clone()).unwrap();
        assert!(ControllerLease::acquire(path.clone()).is_err());
        drop(first);
        ControllerLease::acquire(path).unwrap();
    }

    #[tokio::test]
    async fn active_turn_cancellation_aborts_the_running_task() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let task_flag = Arc::clone(&dropped);
        let handle = tokio::spawn(async move {
            let _guard = Dropped(task_flag);
            std::future::pending::<()>().await;
            unreachable!()
        });
        tokio::task::yield_now().await;
        let mut active = Some(ActiveTurn {
            kind: TurnKind::Autonomy,
            handle,
        });
        cancel_active_turn(&mut active, "test player message").await;
        assert!(active.is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn owned_server_password_is_generated_persisted_and_private() {
        let directory = tempfile::tempdir().unwrap();
        let mut args = Args::try_parse_from([
            "buddy",
            "--start-server",
            "--write-data",
            directory.path().to_str().unwrap(),
        ])
        .unwrap();
        configure_rcon_password(&mut args).unwrap();
        let password = args.rcon_password.as_deref().unwrap();
        let mut resumed = args.clone();
        resumed.rcon_password = None;
        configure_rcon_password(&mut resumed).unwrap();
        assert_eq!(resumed.rcon_password.as_deref(), Some(password));
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            std::fs::read_to_string(password_path(directory.path()))
                .unwrap()
                .trim(),
            password
        );
        assert_eq!(args.rcon_host, "127.0.0.1");
        let config_path = write_mcp_config(&args, Path::new("/tmp/factorio-mcp")).unwrap();
        let arguments = claude_arguments(&args, &config_path.to_string_lossy(), "play", None);
        assert!(arguments
            .iter()
            .all(|argument| !argument.to_string_lossy().contains(password)));
        assert!(std::fs::read_to_string(&config_path)
            .unwrap()
            .contains(password));
        assert!(std::fs::read_to_string(&config_path)
            .unwrap()
            .contains(env!("CARGO_MANIFEST_DIR")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(password_path(directory.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(config_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn mod_install_replaces_complete_tree_and_skips_identical_content() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let mods = directory.path().join("mods");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("info.json"), b"v1").unwrap();
        std::fs::write(source.join("nested/control.lua"), b"return 1").unwrap();

        assert!(install_mod_source(&source, &mods).unwrap());
        assert!(trees_equal(&source, &mods.join("claude-interface")).unwrap());
        assert!(!install_mod_source(&source, &mods).unwrap());
        std::fs::write(source.join("info.json"), b"v2").unwrap();
        assert!(install_mod_source(&source, &mods).unwrap());
        assert_eq!(
            std::fs::read(mods.join("claude-interface/info.json")).unwrap(),
            b"v2"
        );
        assert!(std::fs::read_dir(&mods).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));
    }

    #[test]
    fn resume_promotes_only_owned_newer_autosave_and_preserves_stale_primary() {
        let directory = tempfile::tempdir().expect("tempdir");
        let save = directory.path().join("buddy.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"stale").expect("write primary");
        let run = write_owner(&owner, &save, &managed_runs, RUN_A, false);
        let autosave = run.join("saves/_autosave2.zip");
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(&autosave, b"newer").expect("write autosave");

        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            Some(autosave.clone())
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"newer");
        assert_eq!(
            std::fs::read(directory.path().join("buddy.previous.zip")).unwrap(),
            b"stale"
        );
    }

    #[test]
    fn resume_refuses_clean_or_different_save_autosaves() {
        let directory = tempfile::tempdir().expect("tempdir");
        let save = directory.path().join("buddy.zip");
        let other = directory.path().join("other.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"current").unwrap();
        std::fs::write(&other, b"other").unwrap();

        let run = write_owner(&owner, &save, &managed_runs, RUN_A, true);
        let autosave = run.join("saves/_autosave1.zip");
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(&autosave, b"new autosave").unwrap();

        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            None
        );
        write_owner(&owner, &other, &managed_runs, RUN_B, false);
        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            None
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"current");
        assert!(!directory.path().join("buddy.previous.zip").exists());
    }

    #[test]
    fn resume_refuses_recovery_over_replaced_primary_content() {
        let directory = tempfile::tempdir().unwrap();
        let save = directory.path().join("buddy.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner_path = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"current").unwrap();
        let run = write_owner(&owner_path, &save, &managed_runs, RUN_A, false);
        std::fs::write(&save, b"replacement save").unwrap();
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(run.join("saves/_autosave1.zip"), b"owned autosave").unwrap();

        assert_eq!(
            promote_owned_autosave(&save, &owner_path, &managed_runs).unwrap(),
            None
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"replacement save");
    }

    #[test]
    fn per_primary_sidecars_and_run_namespaces_prevent_competing_save_contamination() {
        let directory = tempfile::tempdir().unwrap();
        let managed_runs = directory.path().join("managed-runs");
        let save_a = directory.path().join("alpha.zip");
        let save_b = directory.path().join("beta.zip");
        std::fs::write(&save_a, b"alpha primary").unwrap();
        std::fs::write(&save_b, b"beta primary").unwrap();
        let owner_a = save_owner_path(&save_a).unwrap();
        let owner_b = save_owner_path(&save_b).unwrap();
        assert_ne!(owner_a, owner_b);

        let run_a = write_owner(&owner_a, &save_a, &managed_runs, RUN_A, false);
        let run_b = write_owner(&owner_b, &save_b, &managed_runs, RUN_B, false);
        std::thread::sleep(Duration::from_millis(10));
        let autosave_a = run_a.join("saves/_autosave1.zip");
        std::fs::write(&autosave_a, b"alpha autosave").unwrap();
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(run_b.join("saves/_autosave3.zip"), b"newer beta autosave").unwrap();
        std::fs::write(
            directory.path().join("_autosave9.zip"),
            b"shared contamination",
        )
        .unwrap();

        assert_eq!(
            promote_owned_autosave(&save_a, &owner_a, &managed_runs).unwrap(),
            Some(autosave_a)
        );
        assert_eq!(std::fs::read(&save_a).unwrap(), b"alpha autosave");
        assert_eq!(std::fs::read(&save_b).unwrap(), b"beta primary");
    }
}
