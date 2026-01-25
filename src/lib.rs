//! factorioctl library - Factorio control via RCON
//!
//! This library provides:
//! - RCON client for communicating with Factorio servers
//! - Lua command builders for type-safe game interactions
//! - World model types (entities, resources, tiles, inventories)
//! - Server process management

pub mod analyze;
pub mod client;
pub mod config;
pub mod output;
pub mod world;

pub use client::rcon::RconClient;
pub use client::FactorioClient;
