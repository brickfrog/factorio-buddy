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

use crate::world::{entity_occupied_tiles, Direction, Entity, TilePos};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Index entities by every tile in their authoritative occupied footprint.
///
/// Factorio permits resources and transport entities to share a tile. The
/// lookup must therefore use semantic precedence rather than input order: a
/// belt on ore is the belt, not an arbitrary resource returned first by the
/// remote query. Stable identity ordering makes all remaining overlaps
/// deterministic as well.
pub(crate) fn build_entity_occupancy_lookup(entities: &[Entity]) -> HashMap<TilePos, &Entity> {
    let mut entities_by_tile: HashMap<TilePos, &Entity> = HashMap::new();

    for entity in entities {
        for tile in entity_occupied_tiles(entity) {
            match entities_by_tile.get(&tile) {
                Some(existing) if !entity_precedes(entity, existing) => {}
                _ => {
                    entities_by_tile.insert(tile, entity);
                }
            }
        }
    }

    entities_by_tile
}

fn entity_precedes(candidate: &Entity, existing: &Entity) -> bool {
    match entity_lookup_priority(candidate).cmp(&entity_lookup_priority(existing)) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => stable_entity_order(candidate, existing) == Ordering::Less,
    }
}

fn entity_lookup_priority(entity: &Entity) -> u8 {
    if is_transport_entity(entity) {
        return 3;
    }

    match entity.entity_type.as_deref() {
        Some("resource") | Some("item-entity") => 0,
        Some("tree") | Some("simple-entity") => 1,
        _ => 2,
    }
}

fn stable_entity_order(left: &Entity, right: &Entity) -> Ordering {
    left.name
        .cmp(&right.name)
        .then_with(|| left.entity_type.cmp(&right.entity_type))
        .then_with(|| left.unit_number.cmp(&right.unit_number))
        .then_with(|| left.position.x.total_cmp(&right.position.x))
        .then_with(|| left.position.y.total_cmp(&right.position.y))
        .then_with(|| left.direction.cmp(&right.direction))
}

/// Why a transport entity is outside the exact static belt model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedTransportReason {
    /// Splitters need two-lane input/output priority and filtering semantics.
    SplitterSemanticsNotModeled,
    /// Underground belts need endpoint mode and paired-endpoint information.
    UndergroundPairingNotModeled,
    /// Loaders have machine-side transfer semantics not represented by a belt edge.
    LoaderSemanticsNotModeled,
    /// Linked belts need their paired entity, which is not present in `Entity`.
    LinkedBeltPairingNotModeled,
    /// A modded or malformed transport prototype cannot be proven equivalent.
    UnsupportedTransportPrototype,
}

/// Explicit evidence for a transport entity excluded from exact analysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsupportedTransport {
    pub unit_number: Option<u32>,
    pub name: String,
    pub entity_type: String,
    pub position: TilePos,
    /// All occupied tiles known from the live bounding box or fallback footprint.
    pub occupied_tiles: Vec<TilePos>,
    pub reason: UnsupportedTransportReason,
}

/// Scope and completeness of a static belt analysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeltAnalysisScope {
    /// True only when every transport entity in the analyzed input is modeled.
    pub connectivity_model_complete: bool,
    /// Number of exact one-tile surface belts included in the graph.
    pub modeled_surface_belts: u32,
    /// Transport entities deliberately excluded instead of guessed geometrically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_transports: Vec<UnsupportedTransport>,
}

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
    /// Whether all transport entity kinds in the analyzed input were modeled.
    pub analysis_scope: BeltAnalysisScope,
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
    /// Whether totals and connected components cover every transport entity.
    pub analysis_scope: BeltAnalysisScope,
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
    /// Whether a gap-free conclusion can cover every transport entity.
    pub analysis_scope: BeltAnalysisScope,
    /// True only when there are no gaps and the static model is complete.
    pub certified_gap_free: bool,
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
    /// Inserter center in Factorio world coordinates.
    pub position: crate::world::Position,
    /// Direction the inserter faces
    pub direction: Direction,
    /// Type of inserter
    pub inserter_type: String,
    /// Authoritative Factorio pickup point.
    pub pickup_position: crate::world::Position,
    /// Authoritative Factorio drop point.
    pub dropoff_position: crate::world::Position,
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
