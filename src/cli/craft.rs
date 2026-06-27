//! Crafting commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::output::Output;

#[derive(Args, Debug)]
pub struct CraftCommand {
    /// Recipe name to craft
    pub recipe: Option<String>,

    /// Number of items to craft
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Wait for crafting to complete
    #[arg(long)]
    pub wait: bool,
}

pub async fn execute(cmd: CraftCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    if let Some(recipe) = cmd.recipe {
        let result = client.craft(&recipe, cmd.count).await?;
        Output::new(conn.output).print(&result)?;

        if cmd.wait {
            client.wait_for_crafting().await?;
            println!("Crafting complete");
        }
    } else if cmd.wait {
        client.wait_for_crafting().await?;
        println!("Crafting complete");
    } else {
        anyhow::bail!("Recipe name required");
    }

    client.close().await?;
    Ok(())
}
