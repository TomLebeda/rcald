//! rcald library: `main.rs` is a thin CLI shell over these modules, one per
//! pipeline stage.
//!
//! Data flow: `config` loads TOML -> `ics` fetches feeds and parses them into
//! `model` types (with VALARMs decoded by `alarm`) -> `recur` expands RRULEs
//! -> `schedule` decides which reminders are due -> `emit` writes them as
//! JSON lines on stdout. `daemon` is the long-running loop on top of these.

pub mod alarm;
pub mod config;
pub mod daemon;
pub mod emit;
pub mod ics;
pub mod model;
pub mod recur;
pub mod schedule;
