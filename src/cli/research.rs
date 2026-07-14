//! Research/technology commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ResolvedConnectionArgs;

fn empty_object_as_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<T>, _>>()
            .map_err(serde::de::Error::custom),
        serde_json::Value::Object(map) if map.is_empty() => Ok(Vec::new()),
        serde_json::Value::Null => Ok(Vec::new()),
        other => Err(serde::de::Error::custom(format!(
            "expected array or empty object, got {other}"
        ))),
    }
}

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

    /// Feed science packs from the agent inventory into a lab
    Feed {
        /// Target lab unit number
        #[arg(long)]
        lab_unit_number: u32,

        /// Science pack item name
        #[arg(long, default_value = "automation-science-pack")]
        science_pack: String,

        /// Number of packs to transfer
        #[arg(long, default_value = "1")]
        count: u32,

        /// Actually transfer packs. Without this flag the command only plans.
        #[arg(long)]
        execute: bool,
    },
}

pub async fn execute(cmd: ResearchCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        ResearchSubcommand::Status => {
            let agent_id = client.agent_id().as_str().to_string();
            let response = client
                .call_remote("get_research_status", &[serde_json::json!(agent_id)])
                .await?;

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
                #[serde(default, deserialize_with = "empty_object_as_vec")]
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
                .call_remote(
                    "get_available_research",
                    &[serde_json::json!(client.agent_id().as_str())],
                )
                .await?;

            #[derive(serde::Deserialize)]
            struct Ingredient {
                name: String,
                amount: u32,
                #[allow(dead_code)]
                available: Option<u32>,
            }
            #[derive(serde::Deserialize)]
            struct Tech {
                name: String,
                research_unit_count: u32,
                #[serde(default, deserialize_with = "empty_object_as_vec")]
                ingredients: Vec<Ingredient>,
                #[serde(default)]
                ready: String,
                #[serde(default, deserialize_with = "empty_object_as_vec")]
                blockers: Vec<String>,
            }
            #[derive(serde::Deserialize)]
            struct Available {
                #[serde(default, deserialize_with = "empty_object_as_vec")]
                technologies: Vec<Tech>,
                guidance: Option<String>,
            }
            let available: Available = serde_json::from_str(&response)?;

            if available.technologies.is_empty() {
                println!("No technologies available to research");
            } else {
                println!("Available technologies:");
                for tech in &available.technologies {
                    let packs = tech
                        .ingredients
                        .iter()
                        .map(|ing| format!("{} x{}", ing.name, ing.amount))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let state = if tech.ready.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", tech.ready)
                    };
                    if packs.is_empty() {
                        println!(
                            "  {} ({} units{})",
                            tech.name, tech.research_unit_count, state
                        );
                    } else {
                        println!(
                            "  {} ({} units{}: {})",
                            tech.name, tech.research_unit_count, state, packs
                        );
                    }
                    if !tech.blockers.is_empty() {
                        println!("    blocked by: {}", tech.blockers.join(", "));
                    }
                }
            }

            if let Some(guidance) = available.guidance {
                println!("\nHint: {}", guidance);
            }
        }

        ResearchSubcommand::Current => {
            let agent_id = client.agent_id().as_str().to_string();
            let response = client
                .call_remote("get_research_status", &[serde_json::json!(agent_id)])
                .await?;

            #[derive(serde::Deserialize)]
            struct CurrentResearch {
                name: String,
                research_unit_count: u32,
            }
            #[derive(serde::Deserialize)]
            struct Status {
                current_research: Option<CurrentResearch>,
                research_progress: f64,
            }
            let status: Status = serde_json::from_str(&response)?;

            if let Some(current) = status.current_research {
                let cost = current.research_unit_count;
                println!(
                    "Researching: {} ({:.1}% complete, {}/{})",
                    current.name,
                    status.research_progress * 100.0,
                    (status.research_progress * cost as f64) as u32,
                    cost
                );
            } else {
                println!("No research in progress");
            }
        }

        ResearchSubcommand::Start { tech } => {
            let agent_id = client.agent_id().as_str().to_string();
            let response = client
                .call_remote(
                    "start_research",
                    &[serde_json::json!(tech), serde_json::json!(agent_id)],
                )
                .await?;

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

        ResearchSubcommand::Feed {
            lab_unit_number,
            science_pack,
            count,
            execute,
        } => {
            let result = client
                .feed_lab_from_inventory(lab_unit_number, &science_pack, count, !execute)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    client.close().await?;
    Ok(())
}
