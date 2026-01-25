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

    match cli.command {
        Commands::Server(cmd) => cli::server::execute(cmd).await,
        Commands::Get(cmd) => cli::get::execute(cmd, &cli.connection).await,
        Commands::Character(cmd) => cli::character::execute(cmd, &cli.connection).await,
        Commands::WalkTo(cmd) => cli::walk_to::execute(cmd, &cli.connection).await,
        Commands::Gather(cmd) => cli::gather::execute(cmd, &cli.connection).await,
        Commands::Build(cmd) => cli::build::execute(cmd, &cli.connection).await,
        Commands::Mine(cmd) => cli::mine::execute(cmd, &cli.connection).await,
        Commands::Craft(cmd) => cli::craft::execute(cmd, &cli.connection).await,
        Commands::Place(cmd) => cli::place::execute(cmd, &cli.connection).await,
        Commands::Remove(cmd) => cli::remove::execute(cmd, &cli.connection).await,
        Commands::Insert(cmd) => cli::insert::execute(cmd, &cli.connection).await,
        Commands::SetRecipe(cmd) => cli::set_recipe::execute(cmd, &cli.connection).await,
        Commands::Tick(cmd) => cli::tick::execute(cmd, &cli.connection).await,
    }
}
