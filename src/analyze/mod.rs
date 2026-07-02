//! Algorithmic analysis tools for factory automation
//!
//! This module provides graph-based analysis of belt networks, inserter
//! configurations, and entity interactions.

mod belt_gaps;
mod belt_graph;
mod belt_network;
mod belt_reach;
mod belt_source_trace;
mod belt_sushi;
mod entity_reach;
mod inserter;
mod item_flow;

pub use belt_gaps::*;
pub use belt_graph::*;
pub use belt_network::*;
pub use belt_reach::*;
pub use belt_source_trace::*;
pub use belt_sushi::*;
pub use entity_reach::*;
pub use inserter::*;
pub use item_flow::*;

use crate::world::{Direction, TilePos};
use serde::{Deserialize, Serialize};

/// Reference to an entity for analysis results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRef {
    pub unit_number: Option<u32>,
    pub name: String,
    pub entity_type: String,
    pub position: TilePos,
}

/// Result of belt reachability analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltReachResult {
    /// Starting position for the analysis
    pub origin: TilePos,
    /// All belt positions upstream (feeding into origin)
    pub upstream: Vec<TilePos>,
    /// All belt positions downstream (fed by origin)
    pub downstream: Vec<TilePos>,
    /// Belt positions with no upstream (input endpoints)
    pub upstream_endpoints: Vec<TilePos>,
    /// Belt positions with no downstream (output endpoints)
    pub downstream_endpoints: Vec<TilePos>,
    /// Total number of connected belts
    pub total_belts: u32,
}

/// A connected belt network
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltNetwork {
    /// Network identifier
    pub id: u32,
    /// All belt positions in this network
    pub belts: Vec<TilePos>,
    /// Belt positions with no upstream (entry points)
    pub inputs: Vec<TilePos>,
    /// Belt positions with no downstream (exit points)
    pub outputs: Vec<TilePos>,
    /// Number of belts in this network
    pub belt_count: u32,
}

/// Result of belt network analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltNetworkResult {
    /// All detected belt networks
    pub networks: Vec<BeltNetwork>,
    /// Total number of separate networks
    pub total_networks: u32,
    /// Total number of belts across all networks
    pub total_belts: u32,
}

/// Type of gap in a belt line
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GapType {
    /// No entity at the expected position
    Missing,
    /// Belt exists but faces wrong direction
    Misaligned,
    /// Non-belt entity blocking the path
    Blocked,
}

/// A gap in the belt network
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltGap {
    /// Position of the belt pointing into the gap
    pub from: TilePos,
    /// Expected position of the next belt
    pub to: TilePos,
    /// Direction the source belt is facing
    pub from_direction: Direction,
    /// Type of gap
    pub gap_type: GapType,
    /// Name of blocking entity (if Blocked)
    pub blocker: Option<String>,
}

/// Result of belt gap analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltGapResult {
    /// All detected gaps
    pub gaps: Vec<BeltGap>,
    /// Number of gaps found
    pub gap_count: u32,
}

/// Result of inserter analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InserterAnalysis {
    /// Inserter unit number
    pub unit_number: u32,
    /// Inserter position (tile)
    pub position: TilePos,
    /// Direction the inserter faces
    pub direction: Direction,
    /// Type of inserter
    pub inserter_type: String,
    /// Position it picks up from
    pub pickup_position: TilePos,
    /// Position it drops to
    pub dropoff_position: TilePos,
    /// Entity at pickup position (if any)
    pub pickup_target: Option<EntityRef>,
    /// Entity at dropoff position (if any)
    pub dropoff_target: Option<EntityRef>,
}

/// Result of entity reach analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityReachResult {
    /// Center position of analysis
    pub origin: TilePos,
    /// Search radius used
    pub radius: u32,
    /// Belts within range
    pub belts: Vec<EntityRef>,
    /// Inserters that can interact with origin
    pub inserters: Vec<InserterAnalysis>,
    /// Other entities that can interact
    pub interacting_entities: Vec<EntityRef>,
}
