//! Gather command - walk to resources and mine them

use anyhow::Result;
use clap::Args;

use super::ConnectionArgs;
use crate::client::FactorioClient;
use crate::output::{Output, OutputFormat};

#[derive(Args, Debug)]
pub struct GatherCommand {
    /// Resource type to gather (iron-ore, copper-ore, coal, stone, or entity name like huge-rock)
    pub resource: String,

    /// Amount to gather
    #[arg(long, default_value = "10")]
    pub amount: u32,

    /// Maximum distance to search for resources
    #[arg(long, default_value = "200")]
    pub radius: u32,
}

pub async fn execute(cmd: GatherCommand, conn: &ConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    let result = client.gather_resource(&cmd.resource, cmd.amount, cmd.radius).await?;

    if conn.output == OutputFormat::Json {
        Output::new(conn.output).print(&result)?;
    } else {
        if result.success {
            println!(
                "Gathered {} {} (walked {:.1} units)",
                result.gathered, result.resource_name, result.distance_walked
            );
            if !result.inventory.is_empty() {
                println!("Inventory now:");
                for item in &result.inventory {
                    println!("  {} x{}", item.name, item.count);
                }
            }
        } else {
            println!("Gathering failed: {}", result.error.unwrap_or_default());
            println!("Gathered {} before failing", result.gathered);
        }
    }

    client.close().await?;
    Ok(())
}
