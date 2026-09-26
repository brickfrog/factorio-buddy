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

    let result = if cmd.recipe.is_empty() {
        client.clear_recipe(cmd.unit_number).await?
    } else {
        client.set_recipe(cmd.unit_number, &cmd.recipe).await?
    };
    if cmd.recipe.is_empty() {
        println!("Cleared recipe on entity #{}", cmd.unit_number);
    } else {
        println!("Set recipe '{}' on entity #{}", cmd.recipe, cmd.unit_number);
    }
    // Changing a recipe unloads the machine; report where every item went.
    for item in result
        .get("returned_items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
    {
        let count = |name: &str| item.get(name).and_then(|value| value.as_u64()).unwrap_or(0);
        println!(
            "  returned {} x{} ({}): {} to inventory, {} spilled at the machine",
            item.get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            count("count"),
            item.get("quality")
                .and_then(|value| value.as_str())
                .unwrap_or("normal"),
            count("inserted"),
            count("spilled")
        );
    }

    client.close().await?;
    Ok(())
}
