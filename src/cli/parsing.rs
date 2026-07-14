//! Shared parsing utilities for CLI coordinate and direction arguments

use crate::world::{Area, Direction, Position, TileArea, TilePos};
use anyhow::Result;

/// Parse integer tile coordinates from "x,y" format
pub fn parse_tile(s: &str) -> Result<TilePos> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        anyhow::bail!("Tile position must be x,y (integers), got '{}'", s);
    }

    let x: i32 = parts[0].trim().parse().map_err(|_| {
        anyhow::anyhow!("X coordinate must be an integer, got '{}'", parts[0].trim())
    })?;
    let y: i32 = parts[1].trim().parse().map_err(|_| {
        anyhow::anyhow!("Y coordinate must be an integer, got '{}'", parts[1].trim())
    })?;

    Ok(TilePos::new(x, y))
}

/// Parse float position coordinates from "x,y" format
pub fn parse_position(s: &str) -> Result<Position> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        anyhow::bail!("Position must be x,y (numbers), got '{}'", s);
    }

    let x: f64 = parts[0]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("X coordinate must be a number, got '{}'", parts[0].trim()))?;
    let y: f64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Y coordinate must be a number, got '{}'", parts[1].trim()))?;

    Ok(Position::new(x, y))
}

/// Parse tile area from "x1,y1,x2,y2" format
pub fn parse_tile_area(s: &str) -> Result<TileArea> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        anyhow::bail!("Tile area must be x1,y1,x2,y2 (integers), got '{}'", s);
    }

    let x1: i32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("x1 must be an integer, got '{}'", parts[0].trim()))?;
    let y1: i32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("y1 must be an integer, got '{}'", parts[1].trim()))?;
    let x2: i32 = parts[2]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("x2 must be an integer, got '{}'", parts[2].trim()))?;
    let y2: i32 = parts[3]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("y2 must be an integer, got '{}'", parts[3].trim()))?;

    Ok(TileArea::new(x1, y1, x2, y2))
}

/// Parse world area from "x1,y1,x2,y2" format
pub fn parse_area(s: &str) -> Result<Area> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        anyhow::bail!("Area must be x1,y1,x2,y2 (numbers), got '{}'", s);
    }

    let x1: f64 = parts[0]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("x1 must be a number, got '{}'", parts[0].trim()))?;
    let y1: f64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("y1 must be a number, got '{}'", parts[1].trim()))?;
    let x2: f64 = parts[2]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("x2 must be a number, got '{}'", parts[2].trim()))?;
    let y2: f64 = parts[3]
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("y2 must be a number, got '{}'", parts[3].trim()))?;

    Ok(Area::new(x1, y1, x2, y2))
}

/// Parse direction from CLI input (name or Factorio value 0/2/4/.../14).
pub fn parse_direction(s: &str) -> Result<Direction> {
    Direction::parse(s).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid direction '{}'. Use a name such as north/east/south/west or a Factorio value 0/2/4/.../14",
            s
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tile() {
        assert_eq!(parse_tile("10,20").unwrap(), TilePos::new(10, 20));
        assert_eq!(parse_tile("-5, 3").unwrap(), TilePos::new(-5, 3));
        assert!(parse_tile("10").is_err());
        assert!(parse_tile("a,b").is_err());
    }

    #[test]
    fn test_parse_position() {
        let pos = parse_position("10.5,20.3").unwrap();
        assert!((pos.x - 10.5).abs() < 0.001);
        assert!((pos.y - 20.3).abs() < 0.001);
    }

    #[test]
    fn test_parse_direction() {
        assert_eq!(parse_direction("n").unwrap(), Direction::North);
        assert_eq!(parse_direction("east").unwrap(), Direction::East);
        assert_eq!(parse_direction("2").unwrap(), Direction::NorthEast);
        assert_eq!(parse_direction("4").unwrap(), Direction::East);
        assert_eq!(parse_direction("8").unwrap(), Direction::South);
        assert_eq!(parse_direction("12").unwrap(), Direction::West);
        assert!(parse_direction("invalid").is_err());
    }

    #[test]
    fn test_parse_tile_area() {
        let area = parse_tile_area("0,0,10,10").unwrap();
        assert_eq!(area.min, TilePos::new(0, 0));
        assert_eq!(area.max, TilePos::new(10, 10));
    }
}
