//! factorioctl - CLI tool for controlling Factorio headless servers via RCON

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod analyze;
mod cli;
mod client;
mod config;
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
    let conn = cli.connection.resolve()?;

    match cli.command {
        Commands::Analyze(cmd) => cli::analyze::execute(cmd, &conn).await,
        Commands::Blueprint(cmd) => cli::blueprint::execute(cmd, &conn).await,
        Commands::Config(cmd) => cli::config::execute(cmd).await,
        Commands::Copy(cmd) => cli::clipboard::execute_copy(cmd, &conn).await,
        Commands::Paste(cmd) => cli::clipboard::execute_paste(cmd, &conn).await,
        Commands::Clipboard(cmd) => cli::clipboard::execute_clipboard(cmd, &conn).await,
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
        Commands::Extract(cmd) => cli::extract::execute(cmd, &conn).await,
        Commands::SetRecipe(cmd) => cli::set_recipe::execute(cmd, &conn).await,
        Commands::Tick(cmd) => cli::tick::execute(cmd, &conn).await,
        Commands::Exec(cmd) => cli::exec::execute(cmd, &conn).await,
        Commands::Map(cmd) => cli::map::execute(cmd, &conn).await,
        Commands::Route(cmd) => cli::route::execute(cmd, &conn).await,
        Commands::Research(cmd) => cli::research::execute(cmd, &conn).await,
        Commands::Power(cmd) => cli::power::execute(cmd, &conn).await,
        Commands::Belt(cmd) => cli::belt::execute(cmd, &conn).await,
        Commands::Say(cmd) => cli::say::execute(cmd, &conn).await,
    }
}
