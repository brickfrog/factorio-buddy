//! Mining commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::output::Output;
use crate::world::Position;

#[derive(Args, Debug)]
pub struct MineCommand {
    /// Mine at specific position
    #[arg(long, allow_hyphen_values = true)]
    pub at: Option<String>,

    /// Mine nearest entity of type
    #[arg(long)]
    pub nearest: Option<String>,

    /// Number of items to mine
    #[arg(long, default_value = "1")]
    pub count: u32,
}

pub async fn execute(cmd: MineCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    if let Some(pos_str) = cmd.at {
        let pos = parse_position(&pos_str)?;
        let result = client.mine_at(pos, cmd.count).await?;
        Output::new(conn.output).print(&result)?;
    } else if let Some(entity_type) = cmd.nearest {
        let result = client.mine_nearest(&entity_type, cmd.count).await?;
        Output::new(conn.output).print(&result)?;
    } else {
        anyhow::bail!("Either --at or --nearest must be specified");
    }

    client.close().await?;
    Ok(())
}

fn parse_position(s: &str) -> Result<Position> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()?;
    if parts.len() != 2 {
        anyhow::bail!("Position must be x,y");
    }
    Ok(Position {
        x: parts[0],
        y: parts[1],
    })
}
