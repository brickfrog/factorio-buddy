//! World model types for Factorio entities, resources, and terrain

mod blueprint;
mod entity;
mod inventory;
mod pathfind;
mod production;
mod prototype;
mod recipe;
mod resource;
mod results;
mod situation;
mod surface;
mod tile;

pub use blueprint::*;
pub use entity::*;
pub use inventory::*;
pub use pathfind::*;
pub use production::*;
pub use prototype::*;
pub use recipe::*;
pub use resource::*;
pub use results::*;
pub use situation::*;
pub use surface::*;
pub use tile::*;

use serde::{Deserialize, Serialize};

pub(crate) fn deserialize_lua_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) if map.is_empty() => Ok(Vec::new()),
        serde_json::Value::Null => Ok(Vec::new()),
        other => serde_json::from_value(other).map_err(serde::de::Error::custom),
    }
}

/// Integer tile coordinates (primary coordinate system for CLI)
/// In Factorio, tiles are 1x1 squares. Entities are placed at their center.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct TilePos {
    pub x: i32,
    pub y: i32,
}

impl TilePos {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Convert to world position for a 1x1 entity (center of tile)
    pub fn to_world_1x1(self) -> Position {
        Position {
            x: self.x as f64 + 0.5,
            y: self.y as f64 + 0.5,
        }
    }

    /// Convert to world position for a 2x2 entity (center of 4 tiles)
    pub fn to_world_2x2(self) -> Position {
        Position {
            x: self.x as f64 + 1.0,
            y: self.y as f64 + 1.0,
        }
    }

    /// Convert to world position for a 3x3 entity
    pub fn to_world_3x3(self) -> Position {
        Position {
            x: self.x as f64 + 1.5,
            y: self.y as f64 + 1.5,
        }
    }

    /// Convert to world position for entity of given size (width, height)
    pub fn to_world(self, width: u32, height: u32) -> Position {
        Position {
            x: self.x as f64 + width as f64 / 2.0,
            y: self.y as f64 + height as f64 / 2.0,
        }
    }

    /// Manhattan distance to another tile
    pub fn manhattan_distance(&self, other: &TilePos) -> u32 {
        ((self.x - other.x).abs() + (self.y - other.y).abs()) as u32
    }

    /// Offset position by 1 tile in the given direction
    pub fn offset_in_direction(&self, dir: Direction) -> TilePos {
        self.offset_in_direction_by(dir, 1)
    }

    /// Offset position by N tiles in the given direction
    pub fn offset_in_direction_by(&self, dir: Direction, distance: i32) -> TilePos {
        match dir {
            Direction::North => TilePos::new(self.x, self.y - distance),
            Direction::NorthEast => TilePos::new(self.x + distance, self.y - distance),
            Direction::East => TilePos::new(self.x + distance, self.y),
            Direction::SouthEast => TilePos::new(self.x + distance, self.y + distance),
            Direction::South => TilePos::new(self.x, self.y + distance),
            Direction::SouthWest => TilePos::new(self.x - distance, self.y + distance),
            Direction::West => TilePos::new(self.x - distance, self.y),
            Direction::NorthWest => TilePos::new(self.x - distance, self.y - distance),
        }
    }
}

impl std::fmt::Display for TilePos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// A position in the game world (float coordinates for Factorio API)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Convert to tile position (floor to get the tile this position is in)
    pub fn to_tile(self) -> TilePos {
        TilePos {
            x: self.x.floor() as i32,
            y: self.y.floor() as i32,
        }
    }

    /// Calculate squared distance to another position
    pub fn distance_squared(&self, other: &Position) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    /// Calculate distance to another position
    pub fn distance(&self, other: &Position) -> f64 {
        self.distance_squared(other).sqrt()
    }
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({:.1}, {:.1})", self.x, self.y)
    }
}

/// A rectangular area
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Area {
    pub left_top: Position,
    pub right_bottom: Position,
}

impl Area {
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            left_top: Position::new(x1.min(x2), y1.min(y2)),
            right_bottom: Position::new(x1.max(x2), y1.max(y2)),
        }
    }

    /// Get the center of the area
    pub fn center(&self) -> Position {
        Position {
            x: (self.left_top.x + self.right_bottom.x) / 2.0,
            y: (self.left_top.y + self.right_bottom.y) / 2.0,
        }
    }

    /// Get the width of the area
    pub fn width(&self) -> f64 {
        self.right_bottom.x - self.left_top.x
    }

    /// Get the height of the area
    pub fn height(&self) -> f64 {
        self.right_bottom.y - self.left_top.y
    }

    /// Check if a position is within this area
    pub fn contains(&self, pos: &Position) -> bool {
        pos.x >= self.left_top.x
            && pos.x <= self.right_bottom.x
            && pos.y >= self.left_top.y
            && pos.y <= self.right_bottom.y
    }
}

/// Area defined by integer tile corners (inclusive)
#[derive(Debug, Clone, Copy, Default)]
pub struct TileArea {
    pub min: TilePos,
    pub max: TilePos,
}

impl TileArea {
    pub fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Self {
        Self {
            min: TilePos::new(x1.min(x2), y1.min(y2)),
            max: TilePos::new(x1.max(x2), y1.max(y2)),
        }
    }

    /// Convert to world area for Factorio queries
    /// The max position is exclusive in world coordinates (adds 1)
    pub fn to_world(self) -> Area {
        Area {
            left_top: Position::new(self.min.x as f64, self.min.y as f64),
            right_bottom: Position::new((self.max.x + 1) as f64, (self.max.y + 1) as f64),
        }
    }

    /// Check if a tile is within this area (inclusive)
    pub fn contains(&self, tile: &TilePos) -> bool {
        tile.x >= self.min.x && tile.x <= self.max.x && tile.y >= self.min.y && tile.y <= self.max.y
    }
}

/// Get entity size (width, height) for common entities
/// This is used to convert tile positions to world positions for placement
pub fn entity_size(name: &str) -> (u32, u32) {
    match name {
        // 1x1 entities
        n if n.contains("belt") && !n.contains("splitter") => (1, 1),
        n if n.contains("inserter") => (1, 1),
        n if n.contains("pole") => (1, 1),
        n if n.contains("pipe") && !n.contains("pump") => (1, 1),
        n if n.contains("chest") => (1, 1),
        "lamp" | "small-lamp" => (1, 1),

        // Shoreline entity; the collision box extends into water but the
        // prototype tile footprint is 1x1.
        "offshore-pump" => (1, 1),
        n if n.contains("pump") && !n.contains("offshore") => (1, 2),

        // 2x1 entities
        n if n.contains("splitter") => (2, 1),

        // 2x2 entities
        "stone-furnace" | "steel-furnace" => (2, 2),
        "burner-mining-drill" => (2, 2),
        "boiler" => (3, 2),
        "steam-engine" => (3, 5),
        "pumpjack" => (3, 3),

        // 3x3 entities
        n if n.starts_with("assembling-machine") => (3, 3),
        "electric-mining-drill" => (3, 3),
        "chemical-plant" => (3, 3),
        "electric-furnace" => (3, 3),
        "lab" => (3, 3),
        "radar" => (3, 3),
        "centrifuge" => (3, 3),
        "rocket-silo" => (9, 9),

        // 5x5 entities
        "oil-refinery" => (5, 5),

        // Default to 1x1
        _ => (1, 1),
    }
}

/// Return every map tile occupied by an entity.
///
/// Live entity summaries include the authoritative, already-rotated collision
/// box. The prototype-size fallback exists for synthetic/offline inputs and
/// rotates non-square footprints for east/west-facing entities.
pub fn entity_occupied_tiles(entity: &Entity) -> Vec<TilePos> {
    let (left, top, right, bottom) = match entity.bounding_box {
        Some(bounds) => (
            bounds.left_top.x.floor() as i32,
            bounds.left_top.y.floor() as i32,
            bounds.right_bottom.x.ceil() as i32,
            bounds.right_bottom.y.ceil() as i32,
        ),
        None => {
            let (mut width, mut height) = entity_size(&entity.name);
            if matches!(entity.direction_enum(), Direction::East | Direction::West) {
                std::mem::swap(&mut width, &mut height);
            }
            let half_width = width as f64 / 2.0;
            let half_height = height as f64 / 2.0;
            (
                (entity.position.x - half_width).floor() as i32,
                (entity.position.y - half_height).floor() as i32,
                (entity.position.x + half_width).ceil() as i32,
                (entity.position.y + half_height).ceil() as i32,
            )
        }
    };

    (left..right)
        .flat_map(|x| (top..bottom).map(move |y| TilePos::new(x, y)))
        .collect()
}

/// Direction enum matching Factorio 2.0's defines.direction
/// In Factorio 2.0, direction values are multiples of 4 for cardinal directions
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    North = 0,
    NorthEast = 2,
    East = 4,
    SouthEast = 6,
    South = 8,
    SouthWest = 10,
    West = 12,
    NorthWest = 14,
}

impl Direction {
    /// Convert to Factorio's numeric direction
    pub fn to_factorio(self) -> u8 {
        self as u8
    }

    /// Create from Factorio 2.0's numeric direction
    pub fn from_factorio(n: u8) -> Self {
        match n % 16 {
            0 => Direction::North,
            2 => Direction::NorthEast,
            4 => Direction::East,
            6 => Direction::SouthEast,
            8 => Direction::South,
            10 => Direction::SouthWest,
            12 => Direction::West,
            14 => Direction::NorthWest,
            _ => Direction::North,
        }
    }

    /// Parse from string name (e.g., "north", "n", "up")
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "north" | "n" | "up" => Some(Direction::North),
            "northeast" | "ne" => Some(Direction::NorthEast),
            "east" | "e" | "right" => Some(Direction::East),
            "southeast" | "se" => Some(Direction::SouthEast),
            "south" | "s" | "down" => Some(Direction::South),
            "southwest" | "sw" => Some(Direction::SouthWest),
            "west" | "w" | "left" => Some(Direction::West),
            "northwest" | "nw" => Some(Direction::NorthWest),
            _ => None,
        }
    }

    /// Get the short name for this direction
    pub fn to_name(self) -> &'static str {
        match self {
            Direction::North => "north",
            Direction::NorthEast => "northeast",
            Direction::East => "east",
            Direction::SouthEast => "southeast",
            Direction::South => "south",
            Direction::SouthWest => "southwest",
            Direction::West => "west",
            Direction::NorthWest => "northwest",
        }
    }

    /// Parse a direction name or Factorio's numeric direction value.
    ///
    /// Numeric input is never interpreted as an ordinal index: `4` is east,
    /// `8` is south, and `12` is west, matching `defines.direction`.
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(dir) = Self::from_name(s) {
            return Some(dir);
        }
        let n = s.parse::<u8>().ok()?;
        if n <= 14 && n % 2 == 0 {
            Some(Self::from_factorio(n))
        } else {
            None
        }
    }

    /// Get the opposite direction (180 degrees)
    pub fn opposite(&self) -> Self {
        Direction::from_factorio((*self as u8 + 8) % 16)
    }

    /// Rotate 90 degrees clockwise
    pub fn rotate_cw(&self) -> Self {
        Direction::from_factorio((*self as u8 + 4) % 16)
    }

    /// Rotate 90 degrees counter-clockwise
    pub fn rotate_ccw(&self) -> Self {
        Direction::from_factorio((*self as u8 + 12) % 16)
    }
}

/// Game tick information
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Tick {
    pub tick: u64,
}

impl Tick {
    /// Convert ticks to seconds (60 ticks per second)
    pub fn to_seconds(self) -> f64 {
        self.tick as f64 / 60.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_entity_tile_footprints_match_factorio_prototypes() {
        assert_eq!(entity_size("offshore-pump"), (1, 1));
        assert_eq!(entity_size("boiler"), (3, 2));
        assert_eq!(entity_size("steam-engine"), (3, 5));
        assert_eq!(entity_size("small-electric-pole"), (1, 1));
        assert_eq!(entity_size("electric-mining-drill"), (3, 3));
    }

    #[test]
    fn occupied_tiles_use_authoritative_bounding_box() {
        let entity = Entity {
            unit_number: Some(1),
            name: "assembling-machine-1".to_string(),
            entity_type: Some("assembling-machine".to_string()),
            position: Position::new(3.5, 0.5),
            direction: Direction::North.to_factorio(),
            health: None,
            force: None,
            bounding_box: Some(Area::new(2.1, -0.9, 4.9, 1.9)),
            pickup_position: None,
            drop_position: None,
        };

        let occupied = entity_occupied_tiles(&entity);
        assert_eq!(occupied.len(), 9);
        assert!(occupied.contains(&TilePos::new(2, -1)));
        assert!(occupied.contains(&TilePos::new(4, 1)));
    }

    #[test]
    fn occupied_tiles_rotate_non_square_fallback_footprint() {
        let entity = Entity {
            unit_number: Some(1),
            name: "splitter".to_string(),
            entity_type: Some("splitter".to_string()),
            position: Position::new(0.5, 1.0),
            direction: Direction::East.to_factorio(),
            health: None,
            force: None,
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
        };

        let occupied = entity_occupied_tiles(&entity);
        assert_eq!(occupied, vec![TilePos::new(0, 0), TilePos::new(0, 1)]);
    }
}
