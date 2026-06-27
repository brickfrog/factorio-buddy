//! Mining commands

use anyhow::Result;
use clap::Args;

use super::parsing::parse_position;
use super::ResolvedConnectionArgs;
use crate::output::Output;

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
    let mut client = conn.connect_client().await?;

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
