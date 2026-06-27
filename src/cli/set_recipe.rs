//! Recipe setting commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;

#[derive(Args, Debug)]
pub struct SetRecipeCommand {
    /// Entity unit number
    pub unit_number: u32,

    /// Recipe name
    pub recipe: String,
}

pub async fn execute(cmd: SetRecipeCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    // Check proximity before setting recipe
    client
        .ensure_proximity_to_entity(cmd.unit_number, crate::client::PROXIMITY_RANGE_INTERACT)
        .await?;

    client.set_recipe(cmd.unit_number, &cmd.recipe).await?;
    println!("Set recipe '{}' on entity #{}", cmd.recipe, cmd.unit_number);

    client.close().await?;
    Ok(())
}
