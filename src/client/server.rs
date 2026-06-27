//! Server process management

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Manages Factorio server processes
pub struct ServerManager {
    factorio_binary: PathBuf,
    saves_dir: PathBuf,
    server_process: Option<Child>,
}

impl ServerManager {
    /// Create a new server manager
    pub fn new() -> Result<Self> {
        let factorio_binary = find_factorio_binary()?;
        let saves_dir = std::env::current_dir()?.join("saves");

        // Ensure saves directory exists
        std::fs::create_dir_all(&saves_dir)?;

        Ok(Self {
            factorio_binary,
            saves_dir,
            server_process: None,
        })
    }

    /// Create a new map/save file
    pub async fn create_map(
        &self,
        name: &str,
        peaceful: bool,
        seed: Option<u32>,
        map_gen_settings: Option<PathBuf>,
        map_settings: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let save_path = self.saves_dir.join(format!("{}.zip", name));

        let mut cmd = Command::new(&self.factorio_binary);
        cmd.arg("--create").arg(&save_path);

        if peaceful {
            // Use a map gen settings file with peaceful mode if available
            // Otherwise we'd need to create one
        }

        if let Some(seed) = seed {
            cmd.arg("--map-gen-seed").arg(seed.to_string());
        }

        if let Some(path) = map_gen_settings {
            cmd.arg("--map-gen-settings").arg(path);
        }

        if let Some(path) = map_settings {
            cmd.arg("--map-settings").arg(path);
        }

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
            .wait_with_output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create map: {}", stderr);
        }

        if !save_path.exists() {
            bail!("Save file was not created");
        }

        Ok(save_path)
    }

    /// Start a headless server
    pub async fn start_server(
        &mut self,
        save_path: &PathBuf,
        rcon_port: u16,
        rcon_password: &str,
        server_settings: Option<PathBuf>,
    ) -> Result<()> {
        if self.server_process.is_some() {
            bail!("Server already running");
        }
        if let Some(pid) = self.live_pid_from_file()? {
            bail!("Server already running (PID: {})", pid);
        }

        let mut cmd = Command::new(&self.factorio_binary);
        cmd.arg("--start-server")
            .arg(save_path)
            .arg("--rcon-port")
            .arg(rcon_port.to_string())
            .arg("--rcon-password")
            .arg(rcon_password);

        if let Some(path) = server_settings {
            cmd.arg("--server-settings").arg(path);
        }

        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to start server")?;

        if let Some(pid) = child.id() {
            self.write_pidfile(pid)?;
        }
        self.server_process = Some(child);

        // Wait a bit for server to start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        Ok(())
    }

    /// Stop the running server
    pub async fn stop_server(&mut self) -> Result<()> {
        if let Some(mut child) = self.server_process.take() {
            child.kill().await?;
            child.wait().await?;
            self.remove_pidfile()?;
            return Ok(());
        }

        if let Some(pid) = self.live_pid_from_file()? {
            stop_pid(pid).await?;
        }
        self.remove_pidfile()?;
        Ok(())
    }

    /// Get server status
    pub async fn status(&self) -> Result<String> {
        if let Some(ref child) = self.server_process {
            if let Some(id) = child.id() {
                if factorio_server_running(id) {
                    return Ok(format!("Running (PID: {})", id));
                }
            }
        }
        if let Some(pid) = self.live_pid_from_file()? {
            return Ok(format!("Running (PID: {})", pid));
        }
        Ok("Not running".to_string())
    }

    fn pidfile_path(&self) -> PathBuf {
        self.saves_dir.join(".factorioctl-server.pid")
    }

    fn write_pidfile(&self, pid: u32) -> Result<()> {
        fs::write(self.pidfile_path(), pid.to_string()).context("Failed to write server pidfile")
    }

    fn remove_pidfile(&self) -> Result<()> {
        let path = self.pidfile_path();
        if path.exists() {
            fs::remove_file(path).context("Failed to remove server pidfile")?;
        }
        Ok(())
    }

    fn live_pid_from_file(&self) -> Result<Option<u32>> {
        let path = self.pidfile_path();
        if !path.exists() {
            return Ok(None);
        }

        let raw = fs::read_to_string(&path).context("Failed to read server pidfile")?;
        let pid = match raw.trim().parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => {
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
        };

        if factorio_server_running(pid) {
            Ok(Some(pid))
        } else {
            let _ = fs::remove_file(&path);
            Ok(None)
        }
    }
}

/// Find the Factorio binary on the system
fn find_factorio_binary() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("FACTORIO_PATH") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }

    // Check common installation paths
    let candidates = vec![
        // macOS Steam
        PathBuf::from("/Users")
            .join(whoami::username())
            .join("Library/Application Support/Steam/steamapps/common/Factorio/factorio.app/Contents/MacOS/factorio"),
        // macOS standalone
        PathBuf::from("/Applications/factorio.app/Contents/MacOS/factorio"),
        // Linux Steam
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".steam/steam/steamapps/common/Factorio/bin/x64/factorio"),
        // Linux standalone
        PathBuf::from("/opt/factorio/bin/x64/factorio"),
        // Windows Steam (via WSL or native)
        PathBuf::from("C:/Program Files (x86)/Steam/steamapps/common/Factorio/bin/x64/factorio.exe"),
        PathBuf::from("C:/Program Files/Factorio/bin/x64/factorio.exe"),
    ];

    for path in candidates {
        if path.exists() {
            return Ok(path);
        }
    }

    // Check if factorio is in PATH
    if let Ok(path) = which::which("factorio") {
        return Ok(path);
    }

    bail!(
        "Could not find Factorio binary. Please ensure Factorio is installed or set FACTORIO_PATH environment variable."
    )
}

/// True only if `pid` is alive AND is actually a Factorio server process.
///
/// A bare `/proc/<pid>` existence check is unsafe: a stale pidfile plus PID
/// reuse would make us report an unrelated process as the server (and worse,
/// kill it on stop). We validate identity via the process command line so a
/// reused PID running something else is treated as stale.
fn factorio_server_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let cmdline = match fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        // /proc/<pid>/cmdline is NUL-separated argv; lossily scan it for the
        // factorio binary name.
        let cmd = String::from_utf8_lossy(&cmdline);
        cmd.contains("factorio")
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

async fn stop_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .await
            .context("Failed to stop server process")?;
        if !status.success() && factorio_server_running(pid) {
            bail!("Failed to stop server process {}", pid);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        bail!("Stopping a server from a pidfile is not supported on this platform");
    }

    Ok(())
}

impl Drop for ServerManager {
    fn drop(&mut self) {
        let _ = self.server_process.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn manager_for(saves_dir: PathBuf) -> ServerManager {
        ServerManager {
            factorio_binary: PathBuf::from("factorio"),
            saves_dir,
            server_process: None,
        }
    }

    #[tokio::test]
    async fn status_reads_a_live_factorio_pidfile_from_a_prior_manager() {
        // The test binary itself is `factorioctl-<hash>`, so its /proc cmdline
        // contains "factorio" — a valid stand-in for a live Factorio server.
        let dir = tempdir().unwrap();
        let saves_dir = dir.path().join("saves");
        std::fs::create_dir_all(&saves_dir).unwrap();
        let pidfile = saves_dir.join(".factorioctl-server.pid");
        std::fs::write(&pidfile, std::process::id().to_string()).unwrap();
        let manager = manager_for(saves_dir);

        let status = manager.status().await.unwrap();

        assert!(
            status.contains(&format!("PID: {}", std::process::id())),
            "status should report the live factorio-identified pid, got {status}"
        );
    }

    #[tokio::test]
    async fn unrelated_live_pid_is_treated_as_stale_not_killed() {
        // A reused PID running something that is NOT Factorio (here: `sleep`)
        // must be treated as a stale pidfile: status says Not running, the
        // pidfile is removed, and crucially the unrelated process is NOT killed.
        let dir = tempdir().unwrap();
        let saves_dir = dir.path().join("saves");
        std::fs::create_dir_all(&saves_dir).unwrap();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id().unwrap();
        let pidfile = saves_dir.join(".factorioctl-server.pid");
        std::fs::write(&pidfile, pid.to_string()).unwrap();
        let mut manager = manager_for(saves_dir);

        assert_eq!(manager.status().await.unwrap(), "Not running");
        assert!(!pidfile.exists(), "stale pidfile should be removed on read");

        // stop_server must not kill the unrelated process.
        manager.stop_server().await.unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "stop must not kill an unrelated (non-factorio) reused PID"
        );

        child.kill().await.unwrap();
        let _ = child.wait().await;
    }
}
