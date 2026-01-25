//! CLI command definitions and handlers

use clap::{Parser, Subcommand};

pub mod build;
pub mod character;
pub mod craft;
pub mod gather;
pub mod get;
pub mod insert;
pub mod mine;
pub mod place;
pub mod remove;
pub mod server;
pub mod set_recipe;
pub mod tick;
pub mod walk_to;

// Re-export OutputFormat from output module
pub use crate::output::OutputFormat;

/// CLI tool for controlling Factorio headless servers via RCON
#[derive(Parser, Debug)]
#[command(name = "factorioctl")]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// RCON connection settings
    #[command(flatten)]
    pub connection: ConnectionArgs,
}

/// Global connection arguments
#[derive(Parser, Debug, Clone)]
pub struct ConnectionArgs {
    /// RCON host
    #[arg(long, default_value = "localhost", global = true)]
    pub host: String,

    /// RCON port
    #[arg(long, default_value = "27015", global = true)]
    pub port: u16,

    /// RCON password
    #[arg(long, default_value = "", global = true)]
    pub password: String,

    /// Output format
    #[arg(long, default_value = "human", global = true)]
    pub output: OutputFormat,
}

/// Top-level commands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Server management commands
    Server(server::ServerCommand),

    /// Query game state (tick, entities, resources, tiles)
    Get(get::GetCommand),

    /// Character control (init, teleport, walk, status, inventory)
    Character(character::CharacterCommand),

    /// Walk to a position (smooth navigation)
    WalkTo(walk_to::WalkToCommand),

    /// Gather resources (walk to and mine)
    Gather(gather::GatherCommand),

    /// Build structures (drills, smelters, from plan)
    Build(build::BuildCommand),

    /// Mine entities (low-level)
    Mine(mine::MineCommand),

    /// Craft items
    Craft(craft::CraftCommand),

    /// Place entities from inventory
    Place(place::PlaceCommand),

    /// Remove entities
    Remove(remove::RemoveCommand),

    /// Insert items into entities
    Insert(insert::InsertCommand),

    /// Set recipe on assembling machines
    SetRecipe(set_recipe::SetRecipeCommand),

    /// Tick control (pause, resume, speed)
    Tick(tick::TickCommand),
}
