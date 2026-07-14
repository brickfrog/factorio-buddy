//! factorioctl library - Factorio control via RCON
//!
//! This library provides:
//! - RCON client for communicating with Factorio servers
//! - Lua command builders for type-safe game interactions
//! - World model types (entities, resources, tiles, inventories)
//! - Server process management

// Vestigial API surface from the original factorioctl fork: many lib types
// expose methods/fields the agent path doesn't call, and several re-exports are
// used by one bin (mcp) but not another (factorioctl). Suppress the noise here;
// deep pruning is a separate cleanup, not worth breaking cross-bin imports over.
#![allow(dead_code, unused_imports, unused_mut)]

pub mod analyze;
pub mod cli;
pub mod client;
pub mod config;
pub mod issue_report;
pub mod memory;
pub mod output;
pub mod world;

pub use client::rcon::RconClient;
pub use client::FactorioClient;
