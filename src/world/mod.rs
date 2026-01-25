//! World model types for Factorio entities, resources, and terrain

mod entity;
mod inventory;
mod resource;
mod results;
mod surface;
mod tile;

pub use entity::*;
pub use inventory::*;
pub use resource::*;
pub use results::*;
pub use surface::*;
pub use tile::*;

use serde::{Deserialize, Serialize};

/// A position in the game world
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
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

/// Direction enum matching Factorio's defines.direction
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    North = 0,
    NorthEast = 1,
    East = 2,
    SouthEast = 3,
    South = 4,
    SouthWest = 5,
    West = 6,
    NorthWest = 7,
}

impl Direction {
    /// Convert to Factorio's numeric direction
    pub fn to_factorio(&self) -> u8 {
        *self as u8
    }

    /// Create from Factorio's numeric direction
    pub fn from_factorio(n: u8) -> Self {
        match n % 8 {
            0 => Direction::North,
            1 => Direction::NorthEast,
            2 => Direction::East,
            3 => Direction::SouthEast,
            4 => Direction::South,
            5 => Direction::SouthWest,
            6 => Direction::West,
            7 => Direction::NorthWest,
            _ => Direction::North,
        }
    }

    /// Parse from string name
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

    /// Get the opposite direction
    pub fn opposite(&self) -> Self {
        Direction::from_factorio((*self as u8 + 4) % 8)
    }
}

/// Game tick information
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Tick {
    pub tick: u64,
}

impl Tick {
    /// Convert ticks to seconds (60 ticks per second)
    pub fn to_seconds(&self) -> f64 {
        self.tick as f64 / 60.0
    }
}
