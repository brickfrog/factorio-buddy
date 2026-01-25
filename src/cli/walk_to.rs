//! Walk-to command - smooth navigation to a position

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::world::Position;

#[derive(Args, Debug)]
pub struct WalkToCommand {
    /// Target position (x,y)
    #[arg(allow_hyphen_values = true)]
    pub position: String,

    /// Run instead of walk (faster)
    #[arg(long, short)]
    pub run: bool,
}

pub async fn execute(cmd: WalkToCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let parts: Vec<f64> = cmd
        .position
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()?;
    if parts.len() != 2 {
        anyhow::bail!("Position must be x,y");
    }
    let target = Position {
        x: parts[0],
        y: parts[1],
    };

    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    let start = client.get_character_position().await?;
    println!("Walking from ({:.1}, {:.1}) to ({:.1}, {:.1})...", start.x, start.y, target.x, target.y);

    let result = client.walk_to(target, cmd.run).await?;

    if result.arrived {
        println!("Arrived at ({:.1}, {:.1})", result.final_position.x, result.final_position.y);
    } else {
        println!(
            "Stopped at ({:.1}, {:.1}) - {}",
            result.final_position.x, result.final_position.y,
            result.reason.unwrap_or_else(|| "unknown".to_string())
        );
    }
    println!("Distance walked: {:.1}", result.distance_walked);

    client.close().await?;
    Ok(())
}
