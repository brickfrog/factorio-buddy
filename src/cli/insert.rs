//! Item insertion commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;

#[derive(Args, Debug)]
pub struct InsertCommand {
    /// Item name to insert
    pub item: String,

    /// Entity unit number to insert into
    #[arg(long)]
    pub into: u32,

    /// Number of items
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Inventory type (fuel, input, output)
    #[arg(long, default_value = "fuel")]
    pub inventory: String,
}

pub async fn execute(cmd: InsertCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    client
        .insert_items(cmd.into, &cmd.item, cmd.count, &cmd.inventory)
        .await?;
    println!(
        "Inserted {} {} into entity #{} ({})",
        cmd.count, cmd.item, cmd.into, cmd.inventory
    );

    client.close().await?;
    Ok(())
}
