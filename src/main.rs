//! factorioctl - CLI tool for controlling Factorio headless servers via RCON

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod client;
mod output;
mod world;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let conn = cli.connection.resolve();

    match cli.command {
        Commands::Config(cmd) => cli::config::execute(cmd).await,
        Commands::Server(cmd) => cli::server::execute(cmd).await,
        Commands::Get(cmd) => cli::get::execute(cmd, &conn).await,
        Commands::Character(cmd) => cli::character::execute(cmd, &conn).await,
        Commands::WalkTo(cmd) => cli::walk_to::execute(cmd, &conn).await,
        Commands::Gather(cmd) => cli::gather::execute(cmd, &conn).await,
        Commands::Build(cmd) => cli::build::execute(cmd, &conn).await,
        Commands::Mine(cmd) => cli::mine::execute(cmd, &conn).await,
        Commands::Craft(cmd) => cli::craft::execute(cmd, &conn).await,
        Commands::Place(cmd) => cli::place::execute(cmd, &conn).await,
        Commands::Remove(cmd) => cli::remove::execute(cmd, &conn).await,
        Commands::Insert(cmd) => cli::insert::execute(cmd, &conn).await,
        Commands::SetRecipe(cmd) => cli::set_recipe::execute(cmd, &conn).await,
        Commands::Tick(cmd) => cli::tick::execute(cmd, &conn).await,
        Commands::Exec(cmd) => cli::exec::execute(cmd, &conn).await,
    }
}
