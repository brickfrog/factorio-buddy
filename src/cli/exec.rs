//! Execute raw Lua commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;

#[derive(Args, Debug)]
pub struct ExecCommand {
    /// Lua code to execute
    pub lua: String,
}

pub async fn execute(cmd: ExecCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    let response = client.execute_lua(&cmd.lua).await?;
    println!("{}", response);

    client.close().await?;
    Ok(())
}
