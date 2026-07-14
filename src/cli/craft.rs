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

fn completed_message(completion: &serde_json::Value) -> Result<String> {
    if completion
        .get("completed")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        anyhow::bail!(
            "crafting is not complete: status={}, error_kind={}, error={}",
            completion
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            completion
                .get("error_kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            completion
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error"),
        );
    }
    let evidence = completion
        .get("evidence")
        .unwrap_or(&serde_json::Value::Null);
    Ok(format!(
        "Crafting complete (operation {}, output and flow verified after {} polls, {} ms)",
        completion
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
        evidence
            .get("polls")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        evidence
            .get("elapsed_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    ))
}

pub async fn execute(cmd: CraftCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    if let Some(recipe) = cmd.recipe {
        let result = client.craft(&recipe, cmd.count).await?;
        Output::new(conn.output).print(&result)?;

        if cmd.wait {
            if !result.success {
                anyhow::bail!(
                    "crafting request was not accepted: {}",
                    result.error.as_deref().unwrap_or("unknown error")
                );
            }
            let completion = client.complete_craft_admission().await?;
            println!("{}", completed_message(&completion)?);
        }
    } else if cmd.wait {
        let completion = client.complete_craft_admission().await?;
        println!("{}", completed_message(&completion)?);
    } else {
        anyhow::bail!("Recipe name required");
    }

    client.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_message_requires_verified_transaction() {
        let message = completed_message(&serde_json::json!({
            "completed": true,
            "operation_id": "craft-10-1",
            "evidence": {"polls": 3, "elapsed_ms": 500},
        }))
        .unwrap();
        assert!(message.starts_with("Crafting complete"));
        assert!(message.contains("output and flow verified"));
        assert!(message.contains("craft-10-1"));

        for completion in [
            serde_json::json!({"completed": false, "status": "pending"}),
            serde_json::json!({"completed": false, "status": "timed_out", "error_kind": "crafting_timeout"}),
            serde_json::json!({"status": "completed"}),
        ] {
            assert!(completed_message(&completion).is_err());
        }
    }
}
