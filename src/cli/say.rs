//! Say command - broadcast thoughts via in-game display and/or TTS

use anyhow::Result;
use clap::Args;
use std::process::Stdio;
use tokio::process::Command;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::config::{Config, TtsConfig};

#[derive(Args, Debug)]
pub struct SayCommand {
    /// Message to broadcast
    pub message: String,

    /// Disable TTS for this message (override config)
    #[arg(long)]
    pub no_tts: bool,

    /// TTS only - skip in-game display
    #[arg(long)]
    pub tts_only: bool,
}

pub async fn execute(cmd: SayCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let broadcast = config.broadcast.unwrap_or_default();

    // Determine what to enable based on config + CLI overrides
    let do_console = !cmd.tts_only && broadcast.console;
    let do_flying_text = !cmd.tts_only && broadcast.flying_text;
    let do_tts = !cmd.no_tts && broadcast.tts.as_ref().map(|t| t.enabled).unwrap_or(false);

    // In-game display
    if do_console || do_flying_text {
        let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

        if do_console {
            display_console(&mut client, &cmd.message).await?;
        }

        if do_flying_text {
            display_flying_text(&mut client, &cmd.message).await?;
        }

        client.close().await?;
    }

    // TTS output
    if do_tts {
        let tts_config = broadcast.tts.unwrap_or_default();
        speak_message(&cmd.message, &tts_config).await?;
    }

    Ok(())
}

async fn display_console(client: &mut FactorioClient, message: &str) -> Result<()> {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let lua = format!(r#"game.print("[Agent] {}")"#, escaped);
    client.execute_lua(&lua).await?;
    Ok(())
}

async fn display_flying_text(client: &mut FactorioClient, message: &str) -> Result<()> {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let lua = format!(
        r#"
local player = game.players[1]
if player and player.character and player.character.valid then
    player.character.surface.create_entity{{
        name = "flying-text",
        position = {{ player.character.position.x, player.character.position.y - 2 }},
        text = "{}",
        color = {{ r = 0.8, g = 0.8, b = 1.0 }}
    }}
end
"#,
        escaped
    );
    client.execute_lua(&lua).await?;
    Ok(())
}

async fn speak_message(message: &str, config: &TtsConfig) -> Result<()> {
    match config.backend.as_str() {
        "say" => speak_macos_say(message, config).await,
        "openai" => speak_openai(message, config).await,
        _ => anyhow::bail!("Unknown TTS backend: {}", config.backend),
    }
}

async fn speak_macos_say(message: &str, config: &TtsConfig) -> Result<()> {
    let mut cmd = Command::new("say");

    if let Some(ref voice) = config.voice {
        cmd.arg("-v").arg(voice);
    }

    if let Some(rate) = config.rate {
        // macOS say rate is in words per minute (default ~175)
        let wpm = (175.0 * rate) as u32;
        cmd.arg("-r").arg(wpm.to_string());
    }

    cmd.arg(message);

    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("macOS say command failed");
    }

    Ok(())
}

async fn speak_openai(message: &str, config: &TtsConfig) -> Result<()> {
    // Get API key from config or environment
    let api_key = config
        .openai_api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .ok_or_else(|| anyhow::anyhow!("OpenAI API key required (set in config or OPENAI_API_KEY env var)"))?;

    let voice = config.voice.as_deref().unwrap_or("nova");
    let speed = config.rate.unwrap_or(1.0);

    // Build request body
    let body = serde_json::json!({
        "model": "tts-1",
        "input": message,
        "voice": voice,
        "speed": speed
    });

    // Use curl for simplicity (avoids adding reqwest dependency)
    let mut cmd = Command::new("curl");
    cmd.args([
        "-s",
        "-X",
        "POST",
        "https://api.openai.com/v1/audio/speech",
        "-H",
        &format!("Authorization: Bearer {}", api_key),
        "-H",
        "Content-Type: application/json",
        "-d",
        &body.to_string(),
        "--output",
        "-",
    ]);

    let output = cmd.output().await?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("OpenAI TTS request failed: {}", error);
    }

    // Pipe audio to afplay (macOS) for playback
    let mut play_cmd = Command::new("afplay");
    play_cmd.arg("-");
    play_cmd.stdin(Stdio::piped());

    let mut child = play_cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(&output.stdout).await?;
    }
    child.wait().await?;

    Ok(())
}
