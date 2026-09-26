//! Item extraction commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;

#[derive(Args, Debug)]
pub struct ExtractCommand {
    /// Item name to extract
    pub item: String,

    /// Entity unit number to extract from
    #[arg(long)]
    pub from: u32,

    /// Number of items to extract
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Inventory type (fuel, input, output, chest, furnace_source, furnace_result)
    #[arg(long, default_value = "chest")]
    pub inventory: String,
}

pub async fn execute(cmd: ExtractCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let result = client
        .extract_items(cmd.from, &cmd.item, cmd.count, &cmd.inventory)
        .await?;
    let field = |name: &str| {
        result
            .get(name)
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    };
    println!(
        "Extracted {} {} from entity #{} ({}); restored {}, spilled {}",
        field("extracted"),
        cmd.item,
        cmd.from,
        cmd.inventory,
        field("restored"),
        field("spilled")
    );

    client.close().await?;
    Ok(())
}
