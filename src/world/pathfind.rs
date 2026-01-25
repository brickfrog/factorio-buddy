//! Pathfinding for belt routing

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{Area, Direction, Position};

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

    /// Convert from world Position (rounds to nearest tile)
    pub fn from_position(pos: &Position) -> Self {
        Self {
            x: pos.x.round() as i32,
            y: pos.y.round() as i32,
        }
    }

    /// Convert to world Position (tile center)
    pub fn to_position(&self) -> Position {
        Position {
            x: self.x as f64,
            y: self.y as f64,
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
}

/// Collision map for pathfinding
pub struct CollisionMap {
    /// Blocked tiles (positions that cannot be traversed)
    blocked: HashSet<GridPos>,
    /// Bounding box of the search area
    bounds: Area,
}

impl CollisionMap {
    /// Create a new collision map
    pub fn new(bounds: Area) -> Self {
        Self {
            blocked: HashSet::new(),
            bounds,
        }
    }

    /// Mark a position as blocked
    pub fn block(&mut self, pos: GridPos) {
        self.blocked.insert(pos);
    }

    /// Check if a position is walkable (not blocked and within bounds)
    pub fn is_walkable(&self, pos: &GridPos) -> bool {
        !self.blocked.contains(pos) && self.bounds.contains(&pos.to_position())
    }

    /// Get the number of blocked tiles
    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }
}

/// A single belt placement in the route
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltPlacement {
    pub position: Position,
    pub direction: Direction,
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
    direction: Direction,
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

/// A* pathfinding for belt routing
pub fn find_belt_route(start: GridPos, goal: GridPos, collision_map: &CollisionMap) -> RouteResult {
    // Check start and goal are valid
    if !collision_map.is_walkable(&start) {
        return RouteResult {
            success: false,
            belts: vec![],
            belt_count: 0,
            turn_count: 0,
            error: Some("Start position is blocked".to_string()),
        };
    }
    if !collision_map.is_walkable(&goal) {
        return RouteResult {
            success: false,
            belts: vec![],
            belt_count: 0,
            turn_count: 0,
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
        direction: Direction::North, // Placeholder, will be determined by movement
        g_cost: 0.0,
        f_cost: start.manhattan_distance(&goal) as f64,
    });

    while let Some(current) = open_set.pop() {
        if current.pos == goal {
            // Reconstruct path
            let path = reconstruct_path(&came_from, goal, start);
            let belts = path_to_belt_placements(&path);
            let turn_count = count_turns(&belts);

            return RouteResult {
                success: true,
                belt_count: belts.len() as u32,
                belts,
                turn_count,
                error: None,
            };
        }

        // Skip if already processed
        if closed_set.contains(&current.pos) {
            continue;
        }
        closed_set.insert(current.pos);

        // Get the direction we arrived from (for turn cost calculation)
        let arrival_direction = came_from
            .get(&current.pos)
            .map(|(_, d)| *d)
            .unwrap_or(Direction::North);

        // Explore neighbors
        for (neighbor, move_direction) in current.pos.cardinal_neighbors() {
            if !collision_map.is_walkable(&neighbor) || closed_set.contains(&neighbor) {
                continue;
            }

            // Calculate movement cost (with turn penalty)
            let is_turn = current.pos != start && arrival_direction != move_direction;
            let turn_cost = if is_turn { 0.1 } else { 0.0 };
            let tentative_g = current.g_cost + 1.0 + turn_cost;

            let current_g = g_score.get(&neighbor).copied().unwrap_or(f64::INFINITY);
            if tentative_g < current_g {
                // This path is better
                came_from.insert(neighbor, (current.pos, move_direction));
                g_score.insert(neighbor, tentative_g);

                let h = neighbor.manhattan_distance(&goal) as f64;
                open_set.push(PathNode {
                    pos: neighbor,
                    direction: move_direction,
                    g_cost: tentative_g,
                    f_cost: tentative_g + h,
                });
            }
        }
    }

    // No path found
    RouteResult {
        success: false,
        belts: vec![],
        belt_count: 0,
        turn_count: 0,
        error: Some("No path found - obstacles may be blocking the route".to_string()),
    }
}

/// Reconstruct path from came_from map
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

/// Convert a path of positions into belt placements with correct directions
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
        });
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
pub fn find_walk_path(start: GridPos, goal: GridPos, collision_map: &CollisionMap) -> WalkPathResult {
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
        direction: Direction::North,
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

        let arrival_direction = came_from
            .get(&current.pos)
            .map(|(_, d)| *d)
            .unwrap_or(Direction::North);

        // Explore neighbors (including diagonals for walking)
        for (neighbor, move_direction) in current.pos.cardinal_neighbors() {
            if !collision_map.is_walkable(&neighbor) || closed_set.contains(&neighbor) {
                continue;
            }

            let is_turn = current.pos != start && arrival_direction != move_direction;
            let turn_cost = if is_turn { 0.05 } else { 0.0 };
            let tentative_g = current.g_cost + 1.0 + turn_cost;

            let current_g = g_score.get(&neighbor).copied().unwrap_or(f64::INFINITY);
            if tentative_g < current_g {
                came_from.insert(neighbor, (current.pos, move_direction));
                g_score.insert(neighbor, tentative_g);

                let h = neighbor.manhattan_distance(&goal) as f64;
                open_set.push(PathNode {
                    pos: neighbor,
                    direction: move_direction,
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

        let is_turn = last_direction.map(|d| d != current_direction).unwrap_or(false);
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
}
