//! Pathfinding for belt routing

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{Area, Direction, Position};

/// Underground belt configuration
#[derive(Debug, Clone)]
pub struct UndergroundConfig {
    /// Maximum distance between entry and exit
    pub max_distance: u32,
    /// Entity name for the underground belt
    pub entity_name: String,
    /// Technology name required to use this underground belt
    pub required_tech: String,
}

impl UndergroundConfig {
    /// Create underground config from belt type name
    pub fn from_belt_type(belt_type: &str) -> Option<Self> {
        match belt_type {
            "transport-belt" => Some(Self {
                max_distance: 4,
                entity_name: "underground-belt".into(),
                required_tech: "logistics".into(),
            }),
            "fast-transport-belt" => Some(Self {
                max_distance: 6,
                entity_name: "fast-underground-belt".into(),
                required_tech: "logistics-2".into(),
            }),
            "express-transport-belt" => Some(Self {
                max_distance: 8,
                entity_name: "express-underground-belt".into(),
                required_tech: "logistics-3".into(),
            }),
            _ => None,
        }
    }
}

/// Type of belt placement
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BeltKind {
    /// Normal surface belt
    #[default]
    Surface,
    /// Underground belt entry (goes underground)
    UndergroundEntry,
    /// Underground belt exit (comes up from underground)
    UndergroundExit,
}

/// Options for belt routing
#[derive(Debug, Clone)]
pub struct RoutingOptions {
    /// Allow using underground belts
    pub allow_underground: bool,
    /// Underground belt configuration (derived from belt_type)
    pub underground_config: Option<UndergroundConfig>,
    /// Cost for underground entry/exit (default: 0.5)
    pub underground_penalty: f64,
    /// Cost per tile when underground (default: 0.05)
    pub underground_skip_cost: f64,
}

impl Default for RoutingOptions {
    fn default() -> Self {
        Self {
            allow_underground: false,
            underground_config: None,
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        }
    }
}

/// How we arrived at a position during pathfinding
#[derive(Debug, Clone, Copy)]
enum MoveType {
    /// Moved one tile in a direction (surface belt)
    Surface(Direction),
    /// Jumped underground from entry to exit
    Underground { direction: Direction, distance: u32 },
}

/// Grid position for pathfinding (integer coordinates)
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct GridPos {
    pub x: i32,
    pub y: i32,
}

impl GridPos {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Convert from world Position (floors to tile containing the position)
    /// In Factorio, entities at (x.5, y.5) occupy tile (x, y)
    pub fn from_position(pos: &Position) -> Self {
        Self {
            x: pos.x.floor() as i32,
            y: pos.y.floor() as i32,
        }
    }

    /// Convert to world Position (tile center at x.5, y.5)
    pub fn to_position(self) -> Position {
        Position {
            x: self.x as f64 + 0.5,
            y: self.y as f64 + 0.5,
        }
    }

    /// Manhattan distance to another position
    pub fn manhattan_distance(&self, other: &GridPos) -> u32 {
        ((self.x - other.x).abs() + (self.y - other.y).abs()) as u32
    }

    /// Get neighbors in cardinal directions
    pub fn cardinal_neighbors(&self) -> [(GridPos, Direction); 4] {
        [
            (GridPos::new(self.x, self.y - 1), Direction::North),
            (GridPos::new(self.x + 1, self.y), Direction::East),
            (GridPos::new(self.x, self.y + 1), Direction::South),
            (GridPos::new(self.x - 1, self.y), Direction::West),
        ]
    }

    /// Offset position by direction and distance (cardinal directions only)
    pub fn offset(&self, direction: Direction, distance: i32) -> GridPos {
        match direction {
            Direction::North => GridPos::new(self.x, self.y - distance),
            Direction::East => GridPos::new(self.x + distance, self.y),
            Direction::South => GridPos::new(self.x, self.y + distance),
            Direction::West => GridPos::new(self.x - distance, self.y),
            // Diagonal directions - underground belts only support cardinal
            _ => *self,
        }
    }
}

/// Collision map for pathfinding
pub struct CollisionMap {
    /// Blocked tiles (positions that cannot be traversed)
    blocked: HashSet<GridPos>,
    /// Preferred tiles (lower movement cost)
    preferred: HashSet<GridPos>,
    /// Bounding box of the search area
    bounds: Area,
}

impl CollisionMap {
    /// Create a new collision map
    pub fn new(bounds: Area) -> Self {
        Self {
            blocked: HashSet::new(),
            preferred: HashSet::new(),
            bounds,
        }
    }

    /// Mark a position as blocked
    pub fn block(&mut self, pos: GridPos) {
        self.blocked.insert(pos);
    }

    /// Mark a position as preferred (lower routing cost)
    pub fn prefer(&mut self, pos: GridPos) {
        self.preferred.insert(pos);
    }

    /// Block all tiles within an area
    pub fn block_area(&mut self, area: &Area) {
        for x in area.left_top.x.floor() as i32..area.right_bottom.x.ceil() as i32 {
            for y in area.left_top.y.floor() as i32..area.right_bottom.y.ceil() as i32 {
                self.block(GridPos::new(x, y));
            }
        }
    }

    /// Mark all tiles within an area as preferred (lower routing cost)
    pub fn prefer_area(&mut self, area: &Area) {
        for x in area.left_top.x.floor() as i32..area.right_bottom.x.ceil() as i32 {
            for y in area.left_top.y.floor() as i32..area.right_bottom.y.ceil() as i32 {
                self.prefer(GridPos::new(x, y));
            }
        }
    }

    /// Check if a position is walkable (not blocked and within bounds)
    pub fn is_walkable(&self, pos: &GridPos) -> bool {
        // Check tile coordinates against bounds (inclusive, matching Area::contains behavior)
        let in_bounds = pos.x >= self.bounds.left_top.x.floor() as i32
            && pos.x <= self.bounds.right_bottom.x.floor() as i32
            && pos.y >= self.bounds.left_top.y.floor() as i32
            && pos.y <= self.bounds.right_bottom.y.floor() as i32;
        !self.blocked.contains(pos) && in_bounds
    }

    /// Get movement cost for a tile (0.5 for preferred, 1.0 for normal)
    pub fn tile_cost(&self, pos: &GridPos) -> f64 {
        if self.preferred.contains(pos) {
            0.5
        } else {
            1.0
        }
    }

    /// Get the number of blocked tiles
    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }

    /// Mark a position as not blocked (remove from blocked set)
    pub fn unblock(&mut self, pos: GridPos) {
        self.blocked.remove(&pos);
    }
}

/// A single belt placement in the route
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltPlacement {
    pub position: Position,
    pub direction: Direction,
    /// Type of belt (surface, underground entry, or underground exit)
    #[serde(default)]
    pub kind: BeltKind,
}

/// One validated topology edge in a belt route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltTopologyStep {
    /// Zero-based sequence index.
    pub index: u32,
    /// Tile coordinate occupied by this belt placement.
    pub tile: GridPos,
    /// World-space placement position.
    pub position: Position,
    /// Belt direction at this placement.
    pub direction: Direction,
    /// Surface/underground placement kind.
    pub kind: BeltKind,
    /// Next connected tile in the route, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_tile: Option<GridPos>,
    /// Whether this step connects to the next step.
    pub connects_to_next: bool,
    /// Human-readable connection note.
    pub connection: String,
}

/// Machine-checkable connectivity summary for a planned belt route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltRouteTopology {
    /// Whether all adjacent route steps connect end-to-end.
    pub connected: bool,
    /// First route tile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_tile: Option<GridPos>,
    /// Last route tile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_tile: Option<GridPos>,
    /// Per-step topology evidence.
    pub steps: Vec<BeltTopologyStep>,
    /// Connectivity errors, if any.
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Result of belt routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    /// Whether a path was found
    pub success: bool,
    /// Belt placements from start to end
    pub belts: Vec<BeltPlacement>,
    /// Total belt count
    pub belt_count: u32,
    /// Number of turns in the path
    pub turn_count: u32,
    /// Number of underground belt pairs used
    #[serde(default)]
    pub underground_count: u32,
    /// Machine-checkable connected-route topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<BeltRouteTopology>,
    /// Error message if routing failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of walk pathfinding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkPathResult {
    /// Whether a path was found
    pub success: bool,
    /// Path waypoints from start to end
    pub path: Vec<GridPos>,
    /// Total path length in tiles
    pub path_length: u32,
    /// Error message if pathfinding failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Node in the A* search
#[derive(Clone)]
struct PathNode {
    pos: GridPos,
    g_cost: f64,
    f_cost: f64,
}

impl PartialEq for PathNode {
    fn eq(&self, other: &Self) -> bool {
        self.pos == other.pos
    }
}

impl Eq for PathNode {}

impl Ord for PathNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: lower f_cost is better
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for PathNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
struct BeltRouteState {
    pos: GridPos,
    arrival: Option<Direction>,
}

#[derive(Clone)]
struct BeltPathNode {
    state: BeltRouteState,
    g_cost: f64,
    f_cost: f64,
}

impl PartialEq for BeltPathNode {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

impl Eq for BeltPathNode {}

impl Ord for BeltPathNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for BeltPathNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A* pathfinding for belt routing (without underground support)
pub fn find_belt_route(start: GridPos, goal: GridPos, collision_map: &CollisionMap) -> RouteResult {
    find_belt_route_with_options(start, goal, collision_map, &RoutingOptions::default())
}

/// A* pathfinding for belt routing with configurable options including underground belts
pub fn find_belt_route_with_options(
    start: GridPos,
    goal: GridPos,
    collision_map: &CollisionMap,
    options: &RoutingOptions,
) -> RouteResult {
    // Check start and goal are valid
    if !collision_map.is_walkable(&start) {
        return RouteResult {
            success: false,
            belts: vec![],
            belt_count: 0,
            turn_count: 0,
            underground_count: 0,
            topology: None,
            error: Some("Start position is blocked".to_string()),
        };
    }
    if !collision_map.is_walkable(&goal) {
        return RouteResult {
            success: false,
            belts: vec![],
            belt_count: 0,
            turn_count: 0,
            underground_count: 0,
            topology: None,
            error: Some("Goal position is blocked".to_string()),
        };
    }

    // A* data structures
    let mut open_set = BinaryHeap::new();
    let mut came_from: HashMap<BeltRouteState, (BeltRouteState, MoveType)> = HashMap::new();
    let mut g_score: HashMap<BeltRouteState, f64> = HashMap::new();
    let mut closed_set: HashSet<BeltRouteState> = HashSet::new();

    // Initialize with start node
    let start_state = BeltRouteState {
        pos: start,
        arrival: None,
    };
    g_score.insert(start_state, 0.0);
    open_set.push(BeltPathNode {
        state: start_state,
        g_cost: 0.0,
        f_cost: start.manhattan_distance(&goal) as f64,
    });

    while let Some(current) = open_set.pop() {
        if current.state.pos == goal {
            // Reconstruct path with move types
            let path_with_moves =
                reconstruct_path_with_moves(&came_from, current.state, start_state);
            let belts = path_to_belt_placements_with_moves(&path_with_moves);
            let turn_count = count_turns(&belts);
            let underground_count = belts
                .iter()
                .filter(|b| b.kind == BeltKind::UndergroundEntry)
                .count() as u32;
            let topology = build_belt_route_topology(&belts);

            return RouteResult {
                success: true,
                belt_count: belts.len() as u32,
                belts,
                turn_count,
                underground_count,
                topology: Some(topology),
                error: None,
            };
        }

        // Skip if already processed
        if closed_set.contains(&current.state) {
            continue;
        }
        closed_set.insert(current.state);

        // Explore surface neighbors
        for (neighbor, move_direction) in current.state.pos.cardinal_neighbors() {
            let neighbor_state = BeltRouteState {
                pos: neighbor,
                arrival: Some(move_direction),
            };
            if !collision_map.is_walkable(&neighbor) || closed_set.contains(&neighbor_state) {
                continue;
            }

            let turn_cost = match current.state.arrival {
                Some(arrival) if arrival != move_direction => 0.1,
                _ => 0.0,
            };
            let tile_cost = collision_map.tile_cost(&neighbor);
            let tentative_g = current.g_cost + tile_cost + turn_cost;

            let current_g = g_score
                .get(&neighbor_state)
                .copied()
                .unwrap_or(f64::INFINITY);
            if tentative_g < current_g {
                // This path is better
                came_from.insert(
                    neighbor_state,
                    (current.state, MoveType::Surface(move_direction)),
                );
                g_score.insert(neighbor_state, tentative_g);

                let h = neighbor.manhattan_distance(&goal) as f64;
                open_set.push(BeltPathNode {
                    state: neighbor_state,
                    g_cost: tentative_g,
                    f_cost: tentative_g + h,
                });
            }
        }

        // Explore underground jumps if enabled
        if let Some(ref ug_config) = options.underground_config {
            if options.allow_underground {
                for direction in [
                    Direction::North,
                    Direction::East,
                    Direction::South,
                    Direction::West,
                ] {
                    // Try underground jumps of length 2 to max_distance
                    for distance in 2..=ug_config.max_distance {
                        let exit_pos = current.state.pos.offset(direction, distance as i32);
                        let exit_state = BeltRouteState {
                            pos: exit_pos,
                            arrival: Some(direction),
                        };

                        // Check exit is walkable (entry is current pos, already validated)
                        if !collision_map.is_walkable(&exit_pos) {
                            continue;
                        }

                        // Skip if already in closed set
                        if closed_set.contains(&exit_state) {
                            continue;
                        }

                        // Calculate underground cost
                        // entry + exit penalty + cost per skipped tile
                        let skipped = distance.saturating_sub(2); // entry and exit don't count as skipped
                        let ug_cost = options.underground_penalty * 2.0
                            + (skipped as f64 * options.underground_skip_cost);

                        let turn_cost = match current.state.arrival {
                            Some(arrival) if arrival != direction => 0.1,
                            _ => 0.0,
                        };
                        let tentative_g = current.g_cost + ug_cost + turn_cost;

                        let current_g = g_score.get(&exit_state).copied().unwrap_or(f64::INFINITY);
                        if tentative_g < current_g {
                            // This underground path is better
                            came_from.insert(
                                exit_state,
                                (
                                    current.state,
                                    MoveType::Underground {
                                        direction,
                                        distance,
                                    },
                                ),
                            );
                            g_score.insert(exit_state, tentative_g);

                            let h = exit_pos.manhattan_distance(&goal) as f64;
                            open_set.push(BeltPathNode {
                                state: exit_state,
                                g_cost: tentative_g,
                                f_cost: tentative_g + h,
                            });
                        }
                    }
                }
            }
        }
    }

    // No path found
    RouteResult {
        success: false,
        belts: vec![],
        belt_count: 0,
        turn_count: 0,
        underground_count: 0,
        topology: None,
        error: Some("No path found - obstacles may be blocking the route".to_string()),
    }
}

/// Build a machine-checkable topology summary for route placements.
pub fn build_belt_route_topology(belts: &[BeltPlacement]) -> BeltRouteTopology {
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    let start_tile = belts
        .first()
        .map(|belt| GridPos::from_position(&belt.position));
    let goal_tile = belts
        .last()
        .map(|belt| GridPos::from_position(&belt.position));

    for (index, belt) in belts.iter().enumerate() {
        let tile = GridPos::from_position(&belt.position);
        let next = belts
            .get(index + 1)
            .map(|next_belt| GridPos::from_position(&next_belt.position));
        let (connects_to_next, connection) = match next {
            Some(next_tile) => {
                let connected = belt_step_connects(belt, tile, next_tile);
                if !connected {
                    errors.push(format!(
                        "belt step {} at ({},{}) does not connect to next tile ({},{})",
                        index, tile.x, tile.y, next_tile.x, next_tile.y
                    ));
                }
                (connected, belt_connection_label(belt, tile, next_tile))
            }
            None => (true, "terminal".to_string()),
        };
        steps.push(BeltTopologyStep {
            index: index as u32,
            tile,
            position: belt.position,
            direction: belt.direction,
            kind: belt.kind,
            next_tile: next,
            connects_to_next,
            connection,
        });
    }

    BeltRouteTopology {
        connected: errors.is_empty(),
        start_tile,
        goal_tile,
        steps,
        errors,
    }
}

fn belt_step_connects(belt: &BeltPlacement, tile: GridPos, next: GridPos) -> bool {
    match belt.kind {
        BeltKind::Surface => tile.offset(belt.direction, 1) == next,
        BeltKind::UndergroundEntry => {
            let dx = next.x - tile.x;
            let dy = next.y - tile.y;
            match belt.direction {
                Direction::North => dx == 0 && dy < -1,
                Direction::East => dy == 0 && dx > 1,
                Direction::South => dx == 0 && dy > 1,
                Direction::West => dy == 0 && dx < -1,
                _ => false,
            }
        }
        BeltKind::UndergroundExit => tile.offset(belt.direction, 1) == next,
    }
}

fn belt_connection_label(belt: &BeltPlacement, tile: GridPos, next: GridPos) -> String {
    match belt.kind {
        BeltKind::Surface => format!("surface ({},{}) -> ({},{})", tile.x, tile.y, next.x, next.y),
        BeltKind::UndergroundEntry => {
            format!(
                "underground ({},{}) -> ({},{})",
                tile.x, tile.y, next.x, next.y
            )
        }
        BeltKind::UndergroundExit => {
            format!(
                "underground-exit ({},{}) -> ({},{})",
                tile.x, tile.y, next.x, next.y
            )
        }
    }
}

/// Reconstruct path from came_from map (legacy, for backward compatibility)
fn reconstruct_path(
    came_from: &HashMap<GridPos, (GridPos, Direction)>,
    goal: GridPos,
    start: GridPos,
) -> Vec<GridPos> {
    let mut path = vec![goal];
    let mut current = goal;

    while current != start {
        if let Some((parent, _)) = came_from.get(&current) {
            path.push(*parent);
            current = *parent;
        } else {
            break;
        }
    }

    path.reverse();
    path
}

/// Reconstruct path from came_from map with move types (for underground support)
fn reconstruct_path_with_moves(
    came_from: &HashMap<BeltRouteState, (BeltRouteState, MoveType)>,
    goal: BeltRouteState,
    start: BeltRouteState,
) -> Vec<(GridPos, Option<MoveType>)> {
    let mut path = vec![(goal.pos, None)];
    let mut current = goal;

    while current != start {
        if let Some((parent, move_type)) = came_from.get(&current) {
            path.push((parent.pos, Some(*move_type)));
            current = *parent;
        } else {
            break;
        }
    }

    path.reverse();
    path
}

/// Convert a path of positions into belt placements with correct directions (legacy)
fn path_to_belt_placements(path: &[GridPos]) -> Vec<BeltPlacement> {
    if path.len() < 2 {
        return vec![];
    }

    let mut placements = Vec::with_capacity(path.len());

    for i in 0..path.len() {
        let current = &path[i];

        // Direction is determined by the NEXT position (where items flow TO)
        let direction = if i + 1 < path.len() {
            let next = &path[i + 1];
            direction_from_delta(next.x - current.x, next.y - current.y)
        } else {
            // Last belt: continue in same direction as previous
            if i > 0 {
                let prev = &path[i - 1];
                direction_from_delta(current.x - prev.x, current.y - prev.y)
            } else {
                Direction::North // Fallback
            }
        };

        placements.push(BeltPlacement {
            position: current.to_position(),
            direction,
            kind: BeltKind::Surface,
        });
    }

    placements
}

/// Convert a path with move types into belt placements (with underground support)
fn path_to_belt_placements_with_moves(path: &[(GridPos, Option<MoveType>)]) -> Vec<BeltPlacement> {
    if path.len() < 2 {
        return vec![];
    }

    let mut placements = Vec::new();
    let mut skip_next = false;

    for i in 0..path.len() {
        if skip_next {
            skip_next = false;
            continue;
        }

        let (current_pos, current_move) = &path[i];

        // The move type tells us how to get from current_pos to the next position
        match current_move {
            Some(MoveType::Surface(direction)) => {
                // Normal surface belt pointing in direction of travel
                placements.push(BeltPlacement {
                    position: current_pos.to_position(),
                    direction: *direction,
                    kind: BeltKind::Surface,
                });
            }
            Some(MoveType::Underground { direction, .. }) => {
                // Underground entry at current position
                placements.push(BeltPlacement {
                    position: current_pos.to_position(),
                    direction: *direction,
                    kind: BeltKind::UndergroundEntry,
                });
                // Underground exit at next position
                if i + 1 < path.len() {
                    let (next_pos, _) = &path[i + 1];
                    placements.push(BeltPlacement {
                        position: next_pos.to_position(),
                        direction: *direction,
                        kind: BeltKind::UndergroundExit,
                    });
                    skip_next = true; // Skip the next position since we just placed its belt
                }
            }
            None => {
                // Last position in path - determine direction from movement into this position
                if i > 0 {
                    let (prev_pos, prev_move) = &path[i - 1];
                    // Use the direction from the previous move, or calculate from position delta
                    let direction = match prev_move {
                        Some(MoveType::Surface(d))
                        | Some(MoveType::Underground { direction: d, .. }) => *d,
                        None => direction_from_delta(
                            current_pos.x - prev_pos.x,
                            current_pos.y - prev_pos.y,
                        ),
                    };
                    placements.push(BeltPlacement {
                        position: current_pos.to_position(),
                        direction,
                        kind: BeltKind::Surface,
                    });
                }
            }
        }
    }

    placements
}

/// Get direction from delta (dx, dy)
fn direction_from_delta(dx: i32, dy: i32) -> Direction {
    match (dx, dy) {
        (0, -1) => Direction::North, // Moving up (negative Y)
        (1, 0) => Direction::East,   // Moving right
        (0, 1) => Direction::South,  // Moving down (positive Y)
        (-1, 0) => Direction::West,  // Moving left
        _ => Direction::North,       // Should not happen
    }
}

/// Count turns in a belt route
fn count_turns(belts: &[BeltPlacement]) -> u32 {
    if belts.len() < 2 {
        return 0;
    }

    let mut turns = 0;
    for i in 1..belts.len() {
        if belts[i].direction != belts[i - 1].direction {
            turns += 1;
        }
    }
    turns
}

/// A* pathfinding for walking (returns simplified waypoints)
pub fn find_walk_path(
    start: GridPos,
    goal: GridPos,
    collision_map: &CollisionMap,
) -> WalkPathResult {
    // Check start and goal are valid
    if !collision_map.is_walkable(&start) {
        return WalkPathResult {
            success: false,
            path: vec![],
            path_length: 0,
            error: Some("Start position is blocked".to_string()),
        };
    }
    if !collision_map.is_walkable(&goal) {
        return WalkPathResult {
            success: false,
            path: vec![],
            path_length: 0,
            error: Some("Goal position is blocked".to_string()),
        };
    }

    // A* data structures
    let mut open_set = BinaryHeap::new();
    let mut came_from: HashMap<GridPos, (GridPos, Direction)> = HashMap::new();
    let mut g_score: HashMap<GridPos, f64> = HashMap::new();
    let mut closed_set: HashSet<GridPos> = HashSet::new();

    // Initialize with start node
    g_score.insert(start, 0.0);
    open_set.push(PathNode {
        pos: start,
        g_cost: 0.0,
        f_cost: start.manhattan_distance(&goal) as f64,
    });

    while let Some(current) = open_set.pop() {
        if current.pos == goal {
            // Reconstruct and simplify path
            let full_path = reconstruct_path(&came_from, goal, start);
            let simplified = simplify_walk_path(&full_path);
            return WalkPathResult {
                success: true,
                path_length: full_path.len() as u32,
                path: simplified,
                error: None,
            };
        }

        if closed_set.contains(&current.pos) {
            continue;
        }
        closed_set.insert(current.pos);

        // Explore neighbors (including diagonals for walking)
        for (neighbor, move_direction) in current.pos.cardinal_neighbors() {
            if !collision_map.is_walkable(&neighbor) || closed_set.contains(&neighbor) {
                continue;
            }

            let tentative_g = current.g_cost + 1.0;

            let current_g = g_score.get(&neighbor).copied().unwrap_or(f64::INFINITY);
            if tentative_g < current_g {
                came_from.insert(neighbor, (current.pos, move_direction));
                g_score.insert(neighbor, tentative_g);

                let h = neighbor.manhattan_distance(&goal) as f64;
                open_set.push(PathNode {
                    pos: neighbor,
                    g_cost: tentative_g,
                    f_cost: tentative_g + h,
                });
            }
        }
    }

    // No path found
    WalkPathResult {
        success: false,
        path: vec![],
        path_length: 0,
        error: Some("No path found - obstacles may be blocking the route".to_string()),
    }
}

/// Simplify a walk path by removing intermediate points on straight lines
/// Only keep waypoints where direction changes or every N tiles
fn simplify_walk_path(path: &[GridPos]) -> Vec<GridPos> {
    if path.len() <= 2 {
        return path.to_vec();
    }

    let mut simplified = vec![path[0]];
    let mut last_direction: Option<(i32, i32)> = None;
    let mut steps_since_waypoint = 0;
    const MAX_STEPS_BETWEEN_WAYPOINTS: i32 = 10;

    for i in 1..path.len() {
        let dx = path[i].x - path[i - 1].x;
        let dy = path[i].y - path[i - 1].y;
        let current_direction = (dx, dy);

        let is_turn = last_direction
            .map(|d| d != current_direction)
            .unwrap_or(false);
        steps_since_waypoint += 1;

        // Add waypoint on direction change or after max steps
        if is_turn || steps_since_waypoint >= MAX_STEPS_BETWEEN_WAYPOINTS {
            simplified.push(path[i - 1]);
            steps_since_waypoint = 0;
        }

        last_direction = Some(current_direction);
    }

    // Always add the final destination
    simplified.push(path[path.len() - 1]);

    simplified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn belt_route_state_distinguishes_arrival_direction_at_same_tile() {
        let east = BeltRouteState {
            pos: GridPos::new(4, 7),
            arrival: Some(Direction::East),
        };
        let north = BeltRouteState {
            pos: GridPos::new(4, 7),
            arrival: Some(Direction::North),
        };
        let mut states = HashSet::new();
        states.insert(east);
        states.insert(north);

        assert_ne!(east, north);
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn test_grid_pos_manhattan_distance() {
        let a = GridPos::new(0, 0);
        let b = GridPos::new(3, 4);
        assert_eq!(a.manhattan_distance(&b), 7);
    }

    #[test]
    fn test_simple_path() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let collision_map = CollisionMap::new(bounds);

        let result = find_belt_route(GridPos::new(0, 0), GridPos::new(3, 0), &collision_map);

        assert!(result.success);
        assert_eq!(result.belt_count, 4); // 0,0 -> 1,0 -> 2,0 -> 3,0
        assert_eq!(result.turn_count, 0);
    }

    #[test]
    fn test_simple_path_includes_connected_topology() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let collision_map = CollisionMap::new(bounds);

        let result = find_belt_route(GridPos::new(0, 0), GridPos::new(3, 0), &collision_map);

        let topology = result
            .topology
            .expect("successful route should include topology");
        assert!(topology.connected);
        assert_eq!(topology.start_tile, Some(GridPos::new(0, 0)));
        assert_eq!(topology.goal_tile, Some(GridPos::new(3, 0)));
        assert_eq!(topology.steps.len(), 4);
        assert!(topology.steps.iter().all(|step| step.connects_to_next));
        assert_eq!(topology.errors, Vec::<String>::new());
    }

    #[test]
    fn test_path_with_obstacle() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);
        // Block position (1, 0)
        collision_map.block(GridPos::new(1, 0));

        let result = find_belt_route(GridPos::new(0, 0), GridPos::new(2, 0), &collision_map);

        assert!(result.success);
        // Should go around: 0,0 -> 0,1 -> 1,1 -> 2,1 -> 2,0 or similar
        assert!(result.belt_count > 3);
        assert!(result.turn_count > 0);
    }

    #[test]
    fn test_direction_from_delta() {
        assert_eq!(direction_from_delta(0, -1), Direction::North);
        assert_eq!(direction_from_delta(1, 0), Direction::East);
        assert_eq!(direction_from_delta(0, 1), Direction::South);
        assert_eq!(direction_from_delta(-1, 0), Direction::West);
    }

    #[test]
    fn test_tile_cost() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Normal tile should have cost 1.0
        assert_eq!(collision_map.tile_cost(&GridPos::new(0, 0)), 1.0);

        // Preferred tile should have cost 0.5
        collision_map.prefer(GridPos::new(1, 1));
        assert_eq!(collision_map.tile_cost(&GridPos::new(1, 1)), 0.5);

        // Non-preferred tile still has cost 1.0
        assert_eq!(collision_map.tile_cost(&GridPos::new(2, 2)), 1.0);
    }

    #[test]
    fn test_block_area() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Block an area
        let blocked_area = Area {
            left_top: Position { x: 2.0, y: 2.0 },
            right_bottom: Position { x: 5.0, y: 5.0 },
        };
        collision_map.block_area(&blocked_area);

        // Tiles inside the area should be blocked
        assert!(!collision_map.is_walkable(&GridPos::new(2, 2)));
        assert!(!collision_map.is_walkable(&GridPos::new(3, 3)));
        assert!(!collision_map.is_walkable(&GridPos::new(4, 4)));

        // Tiles outside the area should be walkable
        assert!(collision_map.is_walkable(&GridPos::new(0, 0)));
        assert!(collision_map.is_walkable(&GridPos::new(5, 5)));
        assert!(collision_map.is_walkable(&GridPos::new(6, 6)));
    }

    #[test]
    fn test_prefer_area() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Mark an area as preferred
        let preferred_area = Area {
            left_top: Position { x: 2.0, y: 2.0 },
            right_bottom: Position { x: 5.0, y: 5.0 },
        };
        collision_map.prefer_area(&preferred_area);

        // Tiles inside the preferred area should have lower cost
        assert_eq!(collision_map.tile_cost(&GridPos::new(2, 2)), 0.5);
        assert_eq!(collision_map.tile_cost(&GridPos::new(3, 3)), 0.5);
        assert_eq!(collision_map.tile_cost(&GridPos::new(4, 4)), 0.5);

        // Tiles outside the preferred area should have normal cost
        assert_eq!(collision_map.tile_cost(&GridPos::new(0, 0)), 1.0);
        assert_eq!(collision_map.tile_cost(&GridPos::new(5, 5)), 1.0);
    }

    #[test]
    fn test_path_prefers_preferred_tiles() {
        // Create a map where going through preferred tiles is better
        // even if it means a slightly longer path
        let bounds = Area {
            left_top: Position { x: -5.0, y: -5.0 },
            right_bottom: Position { x: 15.0, y: 15.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Create a "corridor" of preferred tiles that goes south then east
        // Path from (0,0) to (10,0):
        // - Direct path: 10 tiles east (cost = 10.0)
        // - Preferred path: 2 south + 10 east + 2 north through preferred = (2 + 10 + 2) * 0.5 = 7.0
        // But turn costs would add up, so let's simplify

        // Prefer a horizontal strip at y=2
        for x in 0..=10 {
            collision_map.prefer(GridPos::new(x, 2));
        }

        let result = find_belt_route(GridPos::new(0, 2), GridPos::new(10, 2), &collision_map);
        assert!(result.success);
        // Path along preferred tiles should be found
        assert_eq!(result.belt_count, 11); // 0,2 to 10,2 = 11 tiles
    }

    // === Underground Belt Tests ===

    #[test]
    fn test_underground_config_from_belt_type() {
        // Test transport-belt -> underground-belt
        let config = UndergroundConfig::from_belt_type("transport-belt");
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.max_distance, 4);
        assert_eq!(config.entity_name, "underground-belt");
        assert_eq!(config.required_tech, "logistics");

        // Test fast-transport-belt -> fast-underground-belt
        let config = UndergroundConfig::from_belt_type("fast-transport-belt");
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.max_distance, 6);
        assert_eq!(config.entity_name, "fast-underground-belt");
        assert_eq!(config.required_tech, "logistics-2");

        // Test express-transport-belt -> express-underground-belt
        let config = UndergroundConfig::from_belt_type("express-transport-belt");
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.max_distance, 8);
        assert_eq!(config.entity_name, "express-underground-belt");
        assert_eq!(config.required_tech, "logistics-3");

        // Test unknown belt type returns None
        let config = UndergroundConfig::from_belt_type("unknown-belt");
        assert!(config.is_none());
    }

    #[test]
    fn test_underground_skips_obstacle() {
        let bounds = Area {
            left_top: Position { x: -10.0, y: -10.0 },
            right_bottom: Position { x: 10.0, y: 10.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Block middle tiles (1,0), (2,0)
        collision_map.block(GridPos::new(1, 0));
        collision_map.block(GridPos::new(2, 0));

        // Without underground: must go around (more belts, turns)
        let result_no_underground =
            find_belt_route(GridPos::new(0, 0), GridPos::new(3, 0), &collision_map);
        assert!(result_no_underground.success);
        assert!(result_no_underground.belt_count > 4); // Must go around
        assert!(result_no_underground.turn_count >= 2); // At least 2 turns

        // With underground: can go straight through
        let options = RoutingOptions {
            allow_underground: true,
            underground_config: Some(UndergroundConfig {
                max_distance: 4,
                entity_name: "underground-belt".into(),
                required_tech: "logistics".into(),
            }),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };
        let result_with_underground = find_belt_route_with_options(
            GridPos::new(0, 0),
            GridPos::new(3, 0),
            &collision_map,
            &options,
        );
        assert!(result_with_underground.success);
        // Underground path: entry at 0,0 + exit at 3,0 = 2 placements
        assert_eq!(result_with_underground.belt_count, 2);
        assert_eq!(result_with_underground.underground_count, 1);
        assert_eq!(result_with_underground.turn_count, 0);
        let topology = result_with_underground
            .topology
            .expect("underground route should include topology");
        assert!(topology.connected);
        assert!(topology
            .steps
            .iter()
            .any(|step| step.connection.starts_with("underground ")));
    }

    #[test]
    fn test_underground_respects_max_distance() {
        let bounds = Area {
            left_top: Position { x: -5.0, y: -5.0 },
            right_bottom: Position { x: 15.0, y: 15.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);

        // Block tiles 1-8 (gap of 9 tiles)
        for x in 1..9 {
            collision_map.block(GridPos::new(x, 0));
        }

        // With transport-belt max_distance=4, cannot cross this gap underground
        let options = RoutingOptions {
            allow_underground: true,
            underground_config: Some(UndergroundConfig {
                max_distance: 4,
                entity_name: "underground-belt".into(),
                required_tech: "logistics".into(),
            }),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };

        let result = find_belt_route_with_options(
            GridPos::new(0, 0),
            GridPos::new(9, 0),
            &collision_map,
            &options,
        );

        // Should still find a path, but must go around since gap > max_distance
        assert!(result.success);
        // Either goes around, or uses multiple underground segments
        // Either way, should have more than just entry+exit
        assert!(result.belt_count > 2);
    }

    #[test]
    fn test_underground_cost_calculation() {
        // Test that underground is preferred when it saves tiles
        let bounds = Area {
            left_top: Position { x: -5.0, y: -5.0 },
            right_bottom: Position { x: 15.0, y: 15.0 },
        };
        let collision_map = CollisionMap::new(bounds);

        // With underground enabled, for a straight path of 5 tiles:
        // Surface: 5 belts × 1.0 = 5.0
        // Underground: 0.5 + (3 × 0.05) + 0.5 = 1.15
        // Underground should be preferred

        let options = RoutingOptions {
            allow_underground: true,
            underground_config: Some(UndergroundConfig {
                max_distance: 6,
                entity_name: "underground-belt".into(),
                required_tech: "logistics".into(),
            }),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };

        let result = find_belt_route_with_options(
            GridPos::new(0, 0),
            GridPos::new(5, 0),
            &collision_map,
            &options,
        );

        assert!(result.success);
        // Underground should be used since it's cheaper
        // 2 placements: entry + exit
        assert_eq!(result.belt_count, 2);
        assert_eq!(result.underground_count, 1);
    }

    #[test]
    fn test_underground_belt_kinds() {
        let bounds = Area {
            left_top: Position { x: -5.0, y: -5.0 },
            right_bottom: Position { x: 15.0, y: 15.0 },
        };
        let mut collision_map = CollisionMap::new(bounds);
        // Block middle to force underground
        collision_map.block(GridPos::new(1, 0));

        let options = RoutingOptions {
            allow_underground: true,
            underground_config: Some(UndergroundConfig {
                max_distance: 4,
                entity_name: "underground-belt".into(),
                required_tech: "logistics".into(),
            }),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };

        let result = find_belt_route_with_options(
            GridPos::new(0, 0),
            GridPos::new(2, 0),
            &collision_map,
            &options,
        );

        assert!(result.success);
        assert_eq!(result.belts.len(), 2);

        // First belt should be underground entry
        assert_eq!(result.belts[0].kind, BeltKind::UndergroundEntry);
        assert_eq!(result.belts[0].direction, Direction::East);

        // Second belt should be underground exit
        assert_eq!(result.belts[1].kind, BeltKind::UndergroundExit);
        assert_eq!(result.belts[1].direction, Direction::East);
    }

    #[test]
    fn test_grid_pos_offset() {
        let pos = GridPos::new(5, 5);

        assert_eq!(pos.offset(Direction::North, 3), GridPos::new(5, 2));
        assert_eq!(pos.offset(Direction::South, 3), GridPos::new(5, 8));
        assert_eq!(pos.offset(Direction::East, 3), GridPos::new(8, 5));
        assert_eq!(pos.offset(Direction::West, 3), GridPos::new(2, 5));
    }
}
