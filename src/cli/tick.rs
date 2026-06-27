//! Tick control commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ResolvedConnectionArgs;

#[derive(Args, Debug)]
pub struct TickCommand {
    #[command(subcommand)]
    pub command: TickSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum TickSubcommand {
    /// Pause the game
    Pause,

    /// Resume the game
    Resume,

    /// Set game speed multiplier
    Speed {
        /// Speed multiplier (1.0 = normal)
        multiplier: f64,
    },

    /// Wait for N ticks
    Wait {
        /// Number of ticks to wait
        ticks: u32,
    },
}

pub async fn execute(cmd: TickCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        TickSubcommand::Pause => {
            client.pause_game().await?;
            println!("Game paused");
        }
        TickSubcommand::Resume => {
            client.resume_game().await?;
            println!("Game resumed");
        }
        TickSubcommand::Speed { multiplier } => {
            client.set_game_speed(multiplier).await?;
            println!("Game speed set to {}x", multiplier);
        }
        TickSubcommand::Wait { ticks } => {
            let start = client.get_tick().await?;
            client.wait_ticks(ticks).await?;
            let end = client.get_tick().await?;
            println!(
                "Waited {} ticks ({} -> {})",
                end.tick - start.tick,
                start.tick,
                end.tick
            );
        }
    }

    client.close().await?;
    Ok(())
}
