//! Standalone Factorio buddy runtime.
//!
//! This is intentionally a thin host around the Rust MCP server: it watches the
//! mod's chat inbox, gives Claude only the Factorio MCP tools, and sends the
//! final response back to the mod. Gameplay policy remains in the model and
//! gameplay implementation remains in Rust/Lua; there is no second planner or
//! memory system here.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use factorioctl::client::{AgentId, FactorioClient};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, Instant, MissedTickBehavior};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_SYSTEM_PROMPT: &str = "You are an autonomous AI teammate inside a Factorio game. Use the Factorio MCP tools to observe and play the game through your own character. Act on player requests immediately. When idle, inspect the real game state and make concrete progress toward a functioning automated factory. Prioritize self-sustaining automation: build production chains that continuously gather, transport, process, and deliver resources without your character manually moving items. Use hand-crafting and manual item transfers only for bounded bootstrap or recovery, then replace them with automated production; never treat repeated hand-feeding as progress or completion. Treat planner output as an executable contract: when a plan returns exact mutation arguments, execute those exact arguments without substituting a search or approximate mutation. After a compound mutation, inspect the resulting state and correct or remove failed partial work before proceeding. Never claim an action succeeded unless a tool result confirms it. Keep final chat replies concise because they render in a small in-game panel.";

#[derive(Debug, Parser)]
#[command(about = "Run the autonomous Factorio buddy using the Rust MCP tool server")]
struct Args {
    #[arg(long, default_value = "default", env = "FACTORIO_AGENT_ID")]
    agent: String,

    #[arg(long)]
    label: Option<String>,

    #[arg(long, env = "MODEL")]
    model: Option<String>,

    #[arg(long, default_value = "localhost", env = "FACTORIO_RCON_HOST")]
    rcon_host: String,

    #[arg(long, default_value_t = 27015, env = "FACTORIO_RCON_PORT")]
    rcon_port: u16,

    #[arg(long, default_value_t = 34197, env = "FACTORIO_GAME_PORT")]
    game_port: u16,

    #[arg(long, default_value = "factorio", env = "FACTORIO_RCON_PASSWORD")]
    rcon_password: String,

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

    #[arg(long, default_value_t = 600, env = "BUDDY_TURN_TIMEOUT_SECONDS")]
    turn_timeout_seconds: u64,

    #[arg(long, default_value = DEFAULT_SYSTEM_PROMPT)]
    system_prompt: String,
}

#[derive(Clone, Debug, Deserialize)]
struct InputMessage {
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

struct Inbox {
    path: PathBuf,
    offset: u64,
}

struct LocalServer {
    child: Child,
    // Factorio exits when stdin reaches EOF, so retain the pipe for the life of
    // the server even though the buddy never writes console commands to it.
    _stdin: tokio::process::ChildStdin,
    output_task: JoinHandle<()>,
}

impl LocalServer {
    async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        let _ = self.output_task.await;
    }
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
    fn new(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Ok(Self { path, offset })
    }

    fn poll(&mut self) -> Result<Vec<InputMessage>> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return Ok(Vec::new());
        };
        if metadata.len() < self.offset {
            self.offset = 0;
        }
        if metadata.len() == self.offset {
            return Ok(Vec::new());
        }
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut chunk = String::new();
        file.read_to_string(&mut chunk)?;
        self.offset = metadata.len();
        Ok(parse_input(&chunk))
    }
}

fn parse_input(chunk: &str) -> Vec<InputMessage> {
    chunk
        .lines()
        .filter_map(|line| {
            let message: InputMessage = serde_json::from_str(line).ok()?;
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

fn install_mod_into(mods_dir: &Path) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mod/claude-interface");
    let destination = mods_dir.join("claude-interface");
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    copy_tree(&source, &destination)
        .with_context(|| format!("failed to install Factorio mod from {}", source.display()))
}

fn install_mods(write_data: &Path) -> Result<()> {
    install_mod_into(&write_data.join("mods"))?;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let client_mods = home.join(".factorio/mods");
        if client_mods.is_dir() {
            install_mod_into(&client_mods)
                .context("failed to install Buddy mod for the Factorio client")?;
            info!(path = %client_mods.display(), "installed Factorio client mod");
        }
    }
    Ok(())
}

async fn start_local_server(args: &Args) -> Result<LocalServer> {
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
    let config = write_data.join("config.ini");
    std::fs::write(
        &config,
        format!(
            "[path]\nread-data={}\nwrite-data={}\n\n[other]\ncheck-updates=false\n",
            data_root.join("data").display(),
            write_data.display()
        ),
    )?;

    let save = args
        .save
        .clone()
        .unwrap_or_else(|| write_data.join("saves/buddy.zip"));
    if args.fresh && save.exists() {
        std::fs::remove_file(&save)
            .with_context(|| format!("failed to remove old save: {}", save.display()))?;
    }
    if !save.exists() {
        info!(save = %save.display(), "creating Factorio save");
        let status = Command::new(&factorio)
            .arg("--config")
            .arg(&config)
            .arg("--create")
            .arg(&save)
            .arg("--map-gen-settings")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/map-gen.json"))
            .status()
            .await?;
        if !status.success() {
            bail!("Factorio failed to create save: {status}");
        }
    }

    let mut child = Command::new(&factorio)
        .arg("--config")
        .arg(&config)
        .arg("--start-server")
        .arg(&save)
        .arg("--rcon-port")
        .arg(args.rcon_port.to_string())
        .arg("--rcon-password")
        .arg(&args.rcon_password)
        .arg("--port")
        .arg(args.game_port.to_string())
        .arg("--server-settings")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/server.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
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
        if FactorioClient::connect(&args.rcon_host, args.rcon_port, &args.rcon_password)
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
            "FACTORIO_RCON_PASSWORD": args.rcon_password,
            "FACTORIO_AGENT_ID": args.agent,
        }
    }}})
    .to_string()
}

async fn lifecycle(args: &Args, function: &str, values: &[Value]) -> Result<String> {
    let agent_id = AgentId::new(Some(&args.agent))?;
    let mut client = FactorioClient::connect(&args.rcon_host, args.rcon_port, &args.rcon_password)
        .await?
        .with_agent_id(agent_id);
    client.call_remote(function, values).await
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

async fn invoke_claude(
    args: &Args,
    config: &str,
    prompt: &str,
    session_id: &mut Option<String>,
) -> Result<String> {
    let started = Instant::now();
    info!(event = "turn_start", session = session_id.as_deref().unwrap_or("new"), prompt = %prompt, "Claude turn started");
    let mut command = Command::new("claude");
    command
        .arg("--print")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(config)
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .arg("--allowedTools")
        .arg("mcp__factorio__*")
        .arg("--tools")
        .arg("")
        .arg("--disable-slash-commands")
        .arg("--system-prompt")
        .arg(&args.system_prompt)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    if let Some(model) = &args.model {
        command.arg("--model").arg(model);
    }
    if let Some(id) = session_id.as_deref() {
        command.arg("--resume").arg(id);
    }
    command.arg(prompt);

    let mut child = command
        .spawn()
        .context("failed to start `claude`; install/authenticate Claude Code")?;
    let stdout = child.stdout.take().context("claude stdout unavailable")?;
    let stderr = child.stderr.take().context("claude stderr unavailable")?;

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            warn!(event = "model_stderr", message = %line, "Claude stderr");
        }
    });

    let stream = async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut final_result = None;
        let mut tool_names: HashMap<String, String> = HashMap::new();
        while let Some(line) = lines.next_line().await? {
            let event: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => {
                    warn!(event = "model_protocol_error", %error, raw = %line, "Invalid Claude event");
                    continue;
                }
            };
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
                                        block.get("input").cloned().unwrap_or_else(|| json!(null));
                                    tool_names.insert(id.to_owned(), tool.to_owned());
                                    info!(event = "tool_call", tool, tool_use_id = id, arguments = %arguments, "Tool call");
                                }
                                "text" => {
                                    info!(event = "model_text", text = %block.get("text").and_then(|value| value.as_str()).unwrap_or(""), "Claude text")
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
                                let result =
                                    block.get("content").cloned().unwrap_or_else(|| json!(null));
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
        Ok::<_, anyhow::Error>((status, final_result))
    };

    let (status, parsed) = timeout(Duration::from_secs(args.turn_timeout_seconds), stream)
        .await
        .context("claude turn exceeded the wall-clock timeout")??;
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
            return Ok(parsed.result);
        }
    }
    if !status.success() {
        bail!("claude exited {status} without a valid result");
    }
    bail!("claude returned no final result")
}

async fn handle_turn(
    args: &Args,
    config: &str,
    prompt: String,
    player_index: u32,
    response_agent: &str,
    session_id: &mut Option<String>,
) {
    let _ = lifecycle(
        args,
        "set_status",
        &[
            json!(player_index),
            json!("[color=0.8,0.7,0.2]Thinking...[/color]"),
        ],
    )
    .await;
    match invoke_claude(args, config, &prompt, session_id).await {
        Ok(reply) => {
            if let Err(error) = lifecycle(
                args,
                "receive_response",
                &[json!(player_index), json!(response_agent), json!(reply)],
            )
            .await
            {
                warn!(%error, "failed to send response to Factorio");
            }
        }
        Err(error) => {
            warn!(%error, "agent turn failed");
            let _ = lifecycle(
                args,
                "receive_response",
                &[
                    json!(player_index),
                    json!(response_agent),
                    json!(format!("Agent error: {error}")),
                ],
            )
            .await;
        }
    }
    let _ = lifecycle(
        args,
        "set_status",
        &[
            json!(player_index),
            json!("[color=0.4,0.8,0.4]Ready[/color]"),
        ],
    )
    .await;
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();
    let mut args = Args::parse();
    if args.start_server && args.script_output.is_none() {
        args.script_output = Some(args.write_data.join("script-output"));
    }
    let local_server = if args.start_server {
        Some(start_local_server(&args).await?)
    } else {
        None
    };
    AgentId::new(Some(&args.agent)).context("invalid --agent")?;
    let mcp = find_mcp(args.mcp_bin.clone())?;
    let config = mcp_config(&args, &mcp);
    let input = args
        .script_output
        .clone()
        .unwrap_or_else(default_script_output)
        .join("claude-chat/input.jsonl");
    let mut inbox = Inbox::new(input.clone())?;
    let label = args.label.clone().unwrap_or_else(|| args.agent.clone());

    lifecycle(&args, "ping", &[])
        .await
        .context("Factorio Buddy mod is not reachable")?;
    lifecycle(&args, "register_agent", &[json!(args.agent), json!(label)]).await?;
    lifecycle(
        &args,
        "pre_place_character_result",
        &[json!(args.agent), json!("nauvis"), json!(0)],
    )
    .await?;

    info!(agent = %args.agent, input = %input.display(), mcp = %mcp.display(), "Factorio buddy online");
    let mut timer = interval(Duration::from_millis(500));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer.tick().await;
    let mut next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds.max(1));
    let mut session_id = None;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = timer.tick() => {}
        }
        let messages = inbox.poll().unwrap_or_else(|error| {
            warn!(%error, "failed to read Factorio chat inbox");
            Vec::new()
        });
        let mut handled = false;
        for message in messages {
            if message.target_agent != args.agent && message.target_agent != "all" {
                continue;
            }
            handled = true;
            let target = message
                .response_to
                .as_deref()
                .unwrap_or(&args.agent)
                .to_owned();
            handle_turn(
                &args,
                &config,
                message.message,
                message.player_index,
                &target,
                &mut session_id,
            )
            .await;
        }
        if handled {
            next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds.max(1));
            continue;
        }
        if args.heartbeat_seconds == 0 || Instant::now() < next_autonomy {
            continue;
        }
        if args.autonomy_requires_player {
            let count = lifecycle(&args, "connected_player_count_result", &[])
                .await
                .map(|value| connected_player_count(&value))
                .unwrap_or(0);
            if count == 0 {
                next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds.max(1));
                continue;
            }
        }
        handle_turn(
            &args,
            &config,
            "Autonomy tick: inspect the current game state and take the next useful concrete action toward a functioning automated factory. Use tools; do not merely describe a plan.".to_owned(),
            0,
            &args.agent,
            &mut session_id,
        )
        .await;
        next_autonomy = Instant::now() + Duration::from_secs(args.heartbeat_seconds);
    }

    if let Some(server) = local_server {
        info!("stopping Factorio server");
        server.stop().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hides_only_routine_rcon_connection_lines() {
        assert!(!should_forward_factorio_output(
            "Info RemoteCommandProcessor.cpp:245: New RCON connection from 127.0.0.1"
        ));
        assert!(should_forward_factorio_output(
            "Error RemoteCommandProcessor.cpp: RCON authentication failed"
        ));
        assert!(should_forward_factorio_output("Joining game"));
    }
}
