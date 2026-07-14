//! Sushi belt detection - identify belts with mixed items
//!
//! Sushi belts are belts that carry multiple item types, either intentionally
//! (for compact designs) or accidentally (mixing in the wrong items).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::{BeltAnalysisScope, BeltGraph};
use crate::world::{BeltLaneContentsResult, TilePos};

/// Classification of how items are mixed on a belt
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BeltMixType {
    /// Single item type on belt (or only on one lane)
    Pure,
    /// Different items on left vs right lane (intentional separation)
    LaneSeparated,
    /// Multiple items mixed on the same lane (sushi)
    Sushi,
    /// No items on belt
    Empty,
}

/// Analysis of item mixing on a single belt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltMixAnalysis {
    /// Belt position (tile coordinates)
    pub position: TilePos,
    /// Belt unit number
    pub unit_number: Option<u32>,
    /// Type of item mixing detected
    pub mix_type: BeltMixType,
    /// Item types on left lane
    pub left_lane_items: Vec<String>,
    /// Item types on right lane
    pub right_lane_items: Vec<String>,
}

/// Result of sushi belt detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SushiDetectionResult {
    /// Exact-model coverage for loop detection.
    pub analysis_scope: BeltAnalysisScope,
    /// Belts with sushi (multiple items on same lane)
    pub sushi_belts: Vec<BeltMixAnalysis>,
    /// Belts with lane separation (different items per lane)
    pub lane_separated_belts: Vec<BeltMixAnalysis>,
    /// Belts with pure single-item content
    pub pure_belts: Vec<BeltMixAnalysis>,
    /// Count of sushi belts
    pub sushi_belt_count: u32,
    /// Count of lane-separated belts
    pub lane_separated_count: u32,
    /// Count of pure belts
    pub pure_belt_count: u32,
    /// Count of empty belts
    pub empty_belt_count: u32,
    /// Networks that form loops (circular paths)
    pub looping_networks: Vec<Vec<TilePos>>,
}

/// Detect sushi belts from lane contents data
pub fn detect_sushi_belts(
    lane_contents: &BeltLaneContentsResult,
    graph: &BeltGraph,
) -> SushiDetectionResult {
    let mut sushi_belts = Vec::new();
    let mut lane_separated_belts = Vec::new();
    let mut pure_belts = Vec::new();
    let mut empty_count = 0u32;

    for belt in &lane_contents.belts {
        let left_items: Vec<String> = belt
            .left_lane
            .items
            .iter()
            .map(|i| i.name.clone())
            .collect();
        let right_items: Vec<String> = belt
            .right_lane
            .items
            .iter()
            .map(|i| i.name.clone())
            .collect();

        let mix_type = classify_belt_mix(&left_items, &right_items);

        let analysis = BeltMixAnalysis {
            position: belt.position,
            unit_number: Some(belt.unit_number),
            mix_type: mix_type.clone(),
            left_lane_items: left_items,
            right_lane_items: right_items,
        };

        match mix_type {
            BeltMixType::Sushi => sushi_belts.push(analysis),
            BeltMixType::LaneSeparated => lane_separated_belts.push(analysis),
            BeltMixType::Pure => pure_belts.push(analysis),
            BeltMixType::Empty => empty_count += 1,
        }
    }

    // Detect looping networks
    let looping_networks = detect_loops(graph);

    SushiDetectionResult {
        analysis_scope: graph.analysis_scope().clone(),
        sushi_belt_count: sushi_belts.len() as u32,
        lane_separated_count: lane_separated_belts.len() as u32,
        pure_belt_count: pure_belts.len() as u32,
        empty_belt_count: empty_count,
        sushi_belts,
        lane_separated_belts,
        pure_belts,
        looping_networks,
    }
}

/// Classify how items are mixed on a belt based on lane contents
fn classify_belt_mix(left_items: &[String], right_items: &[String]) -> BeltMixType {
    let left_empty = left_items.is_empty();
    let right_empty = right_items.is_empty();

    if left_empty && right_empty {
        return BeltMixType::Empty;
    }

    let left_unique: HashSet<_> = left_items.iter().collect();
    let right_unique: HashSet<_> = right_items.iter().collect();

    // Check if either lane has multiple item types (sushi)
    if left_unique.len() > 1 || right_unique.len() > 1 {
        return BeltMixType::Sushi;
    }

    // Both lanes have at most 1 item type each
    if left_empty || right_empty {
        // Only one lane has items - pure
        return BeltMixType::Pure;
    }

    // Both lanes have exactly 1 item type each
    if left_unique == right_unique {
        // Same item on both lanes - pure
        BeltMixType::Pure
    } else {
        // Different items on each lane - lane separated
        BeltMixType::LaneSeparated
    }
}

/// Detect loops in the belt graph using DFS
fn detect_loops(graph: &BeltGraph) -> Vec<Vec<TilePos>> {
    let mut visited_global: HashSet<TilePos> = HashSet::new();
    let mut loops = Vec::new();

    for start_pos in graph.all_positions() {
        if visited_global.contains(start_pos) {
            continue;
        }

        let mut path = Vec::new();
        let mut visited_path: HashSet<TilePos> = HashSet::new();

        if let Some(loop_path) = find_loop_from(graph, *start_pos, &mut visited_path, &mut path) {
            loops.push(loop_path);
        }

        // Mark all visited positions as globally visited
        visited_global.extend(visited_path);
    }

    loops
}

/// DFS to find a loop starting from a position
fn find_loop_from(
    graph: &BeltGraph,
    pos: TilePos,
    visited: &mut HashSet<TilePos>,
    path: &mut Vec<TilePos>,
) -> Option<Vec<TilePos>> {
    if visited.contains(&pos) {
        // Found a loop - extract the cycle
        if let Some(loop_start_idx) = path.iter().position(|p| *p == pos) {
            return Some(path[loop_start_idx..].to_vec());
        }
        return None;
    }

    visited.insert(pos);
    path.push(pos);

    // Follow downstream edges
    for downstream_pos in graph.downstream_of(&pos) {
        if let Some(loop_path) = find_loop_from(graph, *downstream_pos, visited, path) {
            return Some(loop_path);
        }
    }

    path.pop();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_belt_mix(&[], &[]), BeltMixType::Empty);
    }

    #[test]
    fn test_classify_pure_single_lane() {
        assert_eq!(
            classify_belt_mix(&["iron-ore".to_string()], &[]),
            BeltMixType::Pure
        );
        assert_eq!(
            classify_belt_mix(&[], &["copper-ore".to_string()]),
            BeltMixType::Pure
        );
    }

    #[test]
    fn test_classify_pure_both_lanes() {
        assert_eq!(
            classify_belt_mix(&["iron-ore".to_string()], &["iron-ore".to_string()]),
            BeltMixType::Pure
        );
    }

    #[test]
    fn test_classify_lane_separated() {
        assert_eq!(
            classify_belt_mix(&["iron-ore".to_string()], &["copper-ore".to_string()]),
            BeltMixType::LaneSeparated
        );
    }

    #[test]
    fn test_classify_sushi() {
        assert_eq!(
            classify_belt_mix(&["iron-ore".to_string(), "copper-ore".to_string()], &[]),
            BeltMixType::Sushi
        );
        assert_eq!(
            classify_belt_mix(
                &["iron-ore".to_string()],
                &["copper-ore".to_string(), "coal".to_string()]
            ),
            BeltMixType::Sushi
        );
    }
}
