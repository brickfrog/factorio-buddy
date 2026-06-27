//! Analysis commands for belt networks and entity interactions

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::analyze::{
    analyze_belt_reach, analyze_entity_reach, analyze_inserters, detect_sushi_belts,
    find_belt_gaps, find_belt_networks, trace_belt_sources, BeltGraph,
};
use crate::output::Output;
use crate::world::{Area, TilePos};

use super::ResolvedConnectionArgs;

/// Analyze belt networks and entity interactions
#[derive(Parser, Debug)]
pub struct AnalyzeCommand {
    #[command(subcommand)]
    pub subcommand: AnalyzeSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum AnalyzeSubcommand {
    /// Analyze belt reachability from a position
    BeltReach(BeltReachArgs),

    /// Find all connected belt networks
    BeltNetworks(BeltNetworksArgs),

    /// Find gaps in belt lines
    BeltGaps(BeltGapsArgs),

    /// Analyze inserters in an area
    Inserters(InsertersArgs),

    /// Analyze what entities can interact with a position
    EntityReach(EntityReachArgs),

    /// Get belt contents with lane separation
    BeltContents(BeltContentsArgs),

    /// Detect sushi belts (mixed items on same lane)
    SushiDetect(SushiDetectArgs),

    /// Trace upstream sources for a belt
    BeltSources(BeltSourcesArgs),
}

/// Arguments for belt reachability analysis
#[derive(Parser, Debug)]
pub struct BeltReachArgs {
    /// X coordinate of starting belt
    #[arg(short, long)]
    pub x: i32,

    /// Y coordinate of starting belt
    #[arg(short, long)]
    pub y: i32,

    /// Radius to search for belts (default: 100)
    #[arg(short, long, default_value = "100")]
    pub radius: u32,
}

/// Arguments for belt network analysis
#[derive(Parser, Debug)]
pub struct BeltNetworksArgs {
    /// Center X coordinate
    #[arg(short, long, default_value = "0")]
    pub x: i32,

    /// Center Y coordinate
    #[arg(short, long, default_value = "0")]
    pub y: i32,

    /// Radius to search for belts
    #[arg(short, long, default_value = "100")]
    pub radius: u32,
}

/// Arguments for belt gap analysis
#[derive(Parser, Debug)]
pub struct BeltGapsArgs {
    /// Center X coordinate
    #[arg(short, long, default_value = "0")]
    pub x: i32,

    /// Center Y coordinate
    #[arg(short, long, default_value = "0")]
    pub y: i32,

    /// Radius to search
    #[arg(short, long, default_value = "100")]
    pub radius: u32,
}

/// Arguments for inserter analysis
#[derive(Parser, Debug)]
pub struct InsertersArgs {
    /// Center X coordinate
    #[arg(short, long, default_value = "0")]
    pub x: i32,

    /// Center Y coordinate
    #[arg(short, long, default_value = "0")]
    pub y: i32,

    /// Radius to search
    #[arg(short, long, default_value = "50")]
    pub radius: u32,
}

/// Arguments for entity reach analysis
#[derive(Parser, Debug)]
pub struct EntityReachArgs {
    /// X coordinate of target position
    #[arg(short, long)]
    pub x: i32,

    /// Y coordinate of target position
    #[arg(short, long)]
    pub y: i32,

    /// Search radius
    #[arg(short, long, default_value = "10")]
    pub radius: u32,
}

/// Arguments for belt contents with lane separation
#[derive(Parser, Debug)]
pub struct BeltContentsArgs {
    /// Center X coordinate
    #[arg(short, long, default_value = "0")]
    pub x: i32,

    /// Center Y coordinate
    #[arg(short, long, default_value = "0")]
    pub y: i32,

    /// Radius to search for belts
    #[arg(short, long, default_value = "30")]
    pub radius: u32,
}

/// Arguments for sushi belt detection
#[derive(Parser, Debug)]
pub struct SushiDetectArgs {
    /// Center X coordinate
    #[arg(short, long, default_value = "0")]
    pub x: i32,

    /// Center Y coordinate
    #[arg(short, long, default_value = "0")]
    pub y: i32,

    /// Radius to search
    #[arg(short, long, default_value = "100")]
    pub radius: u32,
}

/// Arguments for belt source tracing
#[derive(Parser, Debug)]
pub struct BeltSourcesArgs {
    /// X coordinate of belt to trace
    #[arg(short, long)]
    pub x: i32,

    /// Y coordinate of belt to trace
    #[arg(short, long)]
    pub y: i32,

    /// Radius to search for connected belts and entities
    #[arg(short, long, default_value = "100")]
    pub radius: u32,
}

pub async fn execute(cmd: AnalyzeCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    match cmd.subcommand {
        AnalyzeSubcommand::BeltReach(args) => execute_belt_reach(args, conn).await,
        AnalyzeSubcommand::BeltNetworks(args) => execute_belt_networks(args, conn).await,
        AnalyzeSubcommand::BeltGaps(args) => execute_belt_gaps(args, conn).await,
        AnalyzeSubcommand::Inserters(args) => execute_inserters(args, conn).await,
        AnalyzeSubcommand::EntityReach(args) => execute_entity_reach(args, conn).await,
        AnalyzeSubcommand::BeltContents(args) => execute_belt_contents(args, conn).await,
        AnalyzeSubcommand::SushiDetect(args) => execute_sushi_detect(args, conn).await,
        AnalyzeSubcommand::BeltSources(args) => execute_belt_sources(args, conn).await,
    }
}

/// Create an Area from center coordinates and radius
fn area_from_center(x: i32, y: i32, radius: u32) -> Area {
    let r = radius as f64;
    Area {
        left_top: crate::world::Position::new(x as f64 - r, y as f64 - r),
        right_bottom: crate::world::Position::new(x as f64 + r, y as f64 + r),
    }
}

async fn execute_belt_reach(args: BeltReachArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let entities = client.find_entities(area, None, None).await?;

    let graph = BeltGraph::from_entities(&entities);
    let start = TilePos::new(args.x, args.y);

    match analyze_belt_reach(&graph, start) {
        Some(result) => Output::new(conn.output).print(&result),
        None => {
            eprintln!("No belt found at position ({}, {})", args.x, args.y);
            Ok(())
        }
    }
}

async fn execute_belt_networks(
    args: BeltNetworksArgs,
    conn: &ResolvedConnectionArgs,
) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let entities = client.find_entities(area, None, None).await?;

    let graph = BeltGraph::from_entities(&entities);
    let result = find_belt_networks(&graph);

    Output::new(conn.output).print(&result)
}

async fn execute_belt_gaps(args: BeltGapsArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let entities = client.find_entities(area, None, None).await?;

    let graph = BeltGraph::from_entities(&entities);
    let result = find_belt_gaps(&graph, &entities);

    Output::new(conn.output).print(&result)
}

async fn execute_inserters(args: InsertersArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let entities = client.find_entities(area, None, None).await?;

    let results = analyze_inserters(&entities);

    Output::new(conn.output).print(&results)
}

async fn execute_entity_reach(args: EntityReachArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    // Search in a larger radius to find inserters that might reach our target
    let search_radius = args.radius + 5; // Account for long inserters
    let area = area_from_center(args.x, args.y, search_radius);
    let entities = client.find_entities(area, None, None).await?;

    let target = TilePos::new(args.x, args.y);
    let result = analyze_entity_reach(&entities, target, args.radius);

    Output::new(conn.output).print(&result)
}

async fn execute_belt_contents(
    args: BeltContentsArgs,
    conn: &ResolvedConnectionArgs,
) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let result = client.get_belt_lane_contents(area).await?;

    Output::new(conn.output).print(&result)
}

async fn execute_sushi_detect(args: SushiDetectArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);

    // Get belt lane contents
    let lane_contents = client.get_belt_lane_contents(area).await?;

    // Get entities for belt graph
    let entities = client.find_entities(area, None, None).await?;
    let graph = BeltGraph::from_entities(&entities);

    // Detect sushi belts
    let result = detect_sushi_belts(&lane_contents, &graph);

    Output::new(conn.output).print(&result)
}

async fn execute_belt_sources(args: BeltSourcesArgs, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = area_from_center(args.x, args.y, args.radius);
    let entities = client.find_entities(area, None, None).await?;

    let graph = BeltGraph::from_entities(&entities);
    let origin = TilePos::new(args.x, args.y);

    match trace_belt_sources(origin, &graph, &entities) {
        Some(result) => Output::new(conn.output).print(&result),
        None => {
            eprintln!("No belt found at position ({}, {})", args.x, args.y);
            Ok(())
        }
    }
}
