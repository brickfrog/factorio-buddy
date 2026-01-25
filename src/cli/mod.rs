//! CLI command definitions and handlers

use clap::{Parser, Subcommand};

pub mod belt;
pub mod blueprint;
pub mod build;
pub mod character;
pub mod clipboard;
pub mod config;
pub mod craft;
pub mod exec;
pub mod gather;
pub mod get;
pub mod insert;
pub mod map;
pub mod mine;
pub mod place;
pub mod power;
pub mod remove;
pub mod research;
pub mod route;
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
    #[arg(long, global = true, env = "FACTORIO_RCON_HOST")]
    pub host: Option<String>,

    /// RCON port
    #[arg(long, global = true, env = "FACTORIO_RCON_PORT")]
    pub port: Option<u16>,

    /// RCON password
    #[arg(long, global = true, env = "FACTORIO_RCON_PASSWORD")]
    pub password: Option<String>,

    /// Output format
    #[arg(long, default_value = "human", global = true)]
    pub output: OutputFormat,
}

impl ConnectionArgs {
    /// Resolve connection args with config file fallbacks
    pub fn resolve(&self) -> ResolvedConnectionArgs {
        let config = config::Config::load().unwrap_or_default();
        ResolvedConnectionArgs {
            host: self.host.clone()
                .or(config.host)
                .unwrap_or_else(|| "localhost".to_string()),
            port: self.port
                .or(config.port)
                .unwrap_or(27015),
            password: self.password.clone()
                .or(config.password)
                .unwrap_or_default(),
            output: self.output,
        }
    }
}

/// Resolved connection arguments with defaults applied
#[derive(Debug, Clone)]
pub struct ResolvedConnectionArgs {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub output: OutputFormat,
}

/// Top-level commands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Declarative blueprint placement
    Blueprint(blueprint::BlueprintCommand),

    /// Configure connection settings
    Config(config::ConfigCommand),

    /// Copy entities from an area to clipboard
    Copy(clipboard::CopyCommand),

    /// Paste entities from clipboard to a location
    Paste(clipboard::PasteCommand),

    /// Manage clipboard
    Clipboard(clipboard::ClipboardCommand),

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

    /// Execute raw Lua command
    Exec(exec::ExecCommand),

    /// Render ASCII map of an area
    Map(map::MapCommand),

    /// Route entities (pathfinding for belts)
    Route(route::RouteCommand),

    /// Research/technology commands
    Research(research::ResearchCommand),

    /// Power infrastructure commands
    Power(power::PowerCommand),

    /// Belt infrastructure commands
    Belt(belt::BeltCommand),
}
