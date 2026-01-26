//! Research/technology commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ResolvedConnectionArgs;
use crate::client::lua::LuaCommand;
use crate::client::FactorioClient;

#[derive(Args, Debug)]
pub struct ResearchCommand {
    #[command(subcommand)]
    pub command: ResearchSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum ResearchSubcommand {
    /// Show research status (completed and available)
    Status,

    /// List available technologies to research
    Available,

    /// Show current research progress
    Current,

    /// Start researching a technology
    Start {
        /// Technology name
        tech: String,
    },
}

pub async fn execute(cmd: ResearchCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    match cmd.command {
        ResearchSubcommand::Status => {
            let lua = LuaCommand::get_research_status();
            let response = client.execute_lua(&lua).await?;

            #[derive(serde::Deserialize)]
            struct Labs {
                count: u32,
                powered: u32,
                working: u32,
            }
            #[derive(serde::Deserialize)]
            struct SciencePack {
                name: String,
                count: u32,
            }
            #[derive(serde::Deserialize)]
            struct CurrentResearch {
                name: String,
                research_unit_count: u32,
            }
            #[derive(serde::Deserialize)]
            struct Status {
                researched_count: u32,
                total_count: u32,
                current_research: Option<CurrentResearch>,
                research_progress: f64,
                labs: Labs,
                science_packs_in_labs: Vec<SciencePack>,
                message: Option<String>,
            }
            let status: Status = serde_json::from_str(&response)?;

            println!("Research Status:");
            println!(
                "  Technologies: {}/{} researched",
                status.researched_count, status.total_count
            );

            if let Some(current) = &status.current_research {
                println!(
                    "  Current: {} ({:.1}% complete)",
                    current.name,
                    status.research_progress * 100.0
                );
            } else {
                println!("  Current: None");
            }

            println!(
                "\nLabs: {} total, {} powered, {} working",
                status.labs.count, status.labs.powered, status.labs.working
            );

            if !status.science_packs_in_labs.is_empty() {
                println!("Science packs in labs:");
                for pack in &status.science_packs_in_labs {
                    println!("  {}: {}", pack.name, pack.count);
                }
            }

            if let Some(msg) = status.message {
                println!("\n⚠️  {}", msg);
            }
        }

        ResearchSubcommand::Available => {
            let response = client
                .execute_lua(
                    r#"
local force = game.forces.player
local available = {}
for name, tech in pairs(force.technologies) do
    if not tech.researched and tech.enabled then
        local all_met = true
        for _, prereq in pairs(tech.prerequisites) do
            if not prereq.researched then
                all_met = false
                break
            end
        end
        if all_met then
            local packs = {}
            for _, ing in pairs(tech.research_unit_ingredients) do
                table.insert(packs, ing.name .. " x" .. ing.amount)
            end
            table.insert(available, {
                name = name,
                cost = tech.research_unit_count,
                packs = table.concat(packs, ", ")
            })
        end
    end
end
rcon.print(helpers.table_to_json(available))
"#,
                )
                .await?;

            #[derive(serde::Deserialize)]
            struct Tech {
                name: String,
                cost: u32,
                packs: String,
            }
            let techs: Vec<Tech> = serde_json::from_str(&response)?;

            if techs.is_empty() {
                println!("No technologies available to research");
            } else {
                println!("Available technologies:");
                for tech in &techs {
                    if tech.packs.is_empty() {
                        println!("  {} ({} units)", tech.name, tech.cost);
                    } else {
                        println!("  {} ({} units: {})", tech.name, tech.cost, tech.packs);
                    }
                }
            }
        }

        ResearchSubcommand::Current => {
            let response = client
                .execute_lua(
                    r#"
local force = game.forces.player
local current = force.current_research
if current then
    local progress = force.research_progress
    rcon.print(helpers.table_to_json({
        name = current.name,
        progress = progress,
        cost = current.research_unit_count
    }))
else
    rcon.print('{"name": null}')
end
"#,
                )
                .await?;

            #[derive(serde::Deserialize)]
            struct Current {
                name: Option<String>,
                progress: Option<f64>,
                cost: Option<u32>,
            }
            let current: Current = serde_json::from_str(&response)?;

            if let Some(name) = current.name {
                let progress = current.progress.unwrap_or(0.0);
                let cost = current.cost.unwrap_or(1);
                println!(
                    "Researching: {} ({:.1}% complete, {}/{})",
                    name,
                    progress * 100.0,
                    (progress * cost as f64) as u32,
                    cost
                );
            } else {
                println!("No research in progress");
            }
        }

        ResearchSubcommand::Start { tech } => {
            let lua = LuaCommand::start_research(&tech);
            let response = client.execute_lua(&lua).await?;

            #[derive(serde::Deserialize)]
            struct StartResult {
                success: bool,
                name: Option<String>,
                error: Option<String>,
                action_needed: Option<String>,
                hint: Option<String>,
                message: Option<String>,
            }
            let result: StartResult = serde_json::from_str(&response)?;

            if result.success {
                println!("Queued research: {}", result.name.unwrap_or(tech));
                if let Some(msg) = result.message {
                    println!("{}", msg);
                }
            } else {
                println!(
                    "Failed: {}",
                    result.error.unwrap_or_else(|| "unknown error".to_string())
                );
                if let Some(hint) = result.hint {
                    println!("Hint: {}", hint);
                }
                if let Some(action) = result.action_needed {
                    println!("Action needed: {}", action);
                }
            }
        }
    }

    client.close().await?;
    Ok(())
}
