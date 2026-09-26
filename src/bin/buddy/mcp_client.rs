//! Minimal runtime MCP client used by the maintenance controller.
//!
//! It spawns the same Factorio MCP server binary Claude uses, with the same
//! agent/RCON/issue-root environment, and speaks rmcp's JSON-RPC client. It
//! never calls gameplay remotes directly.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{bail, Context, Result};
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use tokio::process::Command;

pub struct McpClient {
    service: Option<RunningService<RoleClient, ()>>,
    pgid: Option<i32>,
}

/// Semantic tool result: MCP `isError` flag and the concatenated text content.
pub struct ToolReply {
    pub is_error: bool,
    pub text: String,
}

impl McpClient {
    /// Spawn the MCP binary in its own process group. `env` must be the same
    /// variables Buddy writes into Claude's MCP configuration.
    pub async fn spawn(mcp: &Path, env: &[(&str, String)]) -> Result<Self> {
        let mut command = Command::new(mcp);
        for (key, value) in env {
            command.env(key, value);
        }
        #[cfg(unix)]
        command.process_group(0);
        let transport = TokioChildProcess::new(command)
            .with_context(|| format!("failed to spawn MCP server {}", mcp.display()))?;
        let pgid = transport.id().map(|pid| pid as i32);
        let service = ().serve(transport).await.context("MCP initialization handshake failed")?;
        Ok(Self {
            service: Some(service),
            pgid,
        })
    }

    pub fn process_group(&self) -> Option<i32> {
        self.pgid
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolReply> {
        let Some(service) = self.service.as_ref() else {
            bail!("MCP client already closed");
        };
        let Value::Object(arguments) = arguments else {
            bail!("MCP tool arguments must be a JSON object");
        };
        let result = service
            .call_tool(CallToolRequestParams {
                meta: None,
                name: Cow::Owned(name.to_owned()),
                arguments: Some(arguments),
                task: None,
            })
            .await
            .with_context(|| format!("MCP call to {name} failed"))?;
        let mut text = String::new();
        for content in &result.content {
            if let RawContent::Text(block) = &content.raw {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&block.text);
            }
        }
        if text.is_empty() {
            if let Some(structured) = &result.structured_content {
                text = structured.to_string();
            }
        }
        Ok(ToolReply {
            is_error: result.is_error.unwrap_or(false),
            text,
        })
    }

    /// Close the JSON-RPC session. The caller owns the process group (see
    /// `process_group`) and reaps it so cancellation and normal exit share one
    /// cleanup path.
    pub async fn close(mut self) {
        if let Some(mut service) = self.service.take() {
            let _ = service
                .close_with_timeout(std::time::Duration::from_secs(3))
                .await;
        }
    }
}
