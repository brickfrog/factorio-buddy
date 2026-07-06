//! Execute raw Lua commands

use anyhow::{bail, Result};
use clap::Args;

use super::ResolvedConnectionArgs;

#[derive(Args, Debug)]
pub struct ExecCommand {
    /// Lua code to execute
    pub lua: String,
}

fn raw_lua_enabled(env_value: Option<&str>) -> bool {
    matches!(
        env_value.map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on")
    )
}

pub async fn execute(cmd: ExecCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    if !raw_lua_enabled(std::env::var("FACTORIOCTL_ALLOW_RAW_LUA").ok().as_deref()) {
        bail!(
            "raw Lua CLI exec is disabled by default. Set FACTORIOCTL_ALLOW_RAW_LUA=1 for trusted operator/debug use."
        );
    }

    let mut client = conn.connect_client().await?;

    let response = client.execute_lua(&cmd.lua).await?;
    println!("{}", response);

    client.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::raw_lua_enabled;

    #[test]
    fn raw_lua_cli_exec_uses_explicit_operator_opt_in() {
        for value in [
            Some("1"),
            Some("true"),
            Some(" TRUE "),
            Some("yes"),
            Some("on"),
        ] {
            assert!(raw_lua_enabled(value), "{value:?}");
        }
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
        ] {
            assert!(!raw_lua_enabled(value), "{value:?}");
        }
    }
}
