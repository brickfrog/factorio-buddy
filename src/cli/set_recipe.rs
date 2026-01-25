//! Recipe setting commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;

#[derive(Args, Debug)]
pub struct SetRecipeCommand {
    /// Entity unit number
    pub unit_number: u32,

    /// Recipe name
    pub recipe: String,
}

pub async fn execute(cmd: SetRecipeCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    client.set_recipe(cmd.unit_number, &cmd.recipe).await?;
    println!("Set recipe '{}' on entity #{}", cmd.recipe, cmd.unit_number);

    client.close().await?;
    Ok(())
}
