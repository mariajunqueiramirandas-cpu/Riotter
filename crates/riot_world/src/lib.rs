//! The riot's state (DESIGN §4.3, §5): the [`World`] that owns the agent
//! table, the grids and flow fields, and the state of every system that runs
//! over the whole world (police, the commander, messages, hazards, minds,
//! outcome tallies), with the bookkeeping that only touches that state.
//!
//! The phases that change it live in their own crates (`riot_think`,
//! `riot_messages`, `riot_hazards`, `riot_interact`, `riot_tactics`,
//! `riot_police`, `riot_outcome`), and `riot_sim` runs them in tick order.

#![deny(unsafe_code)]

pub mod commander;
pub mod harm;
pub mod hazards;
pub mod messages;
pub mod minds;
pub mod outcome;
pub mod police;
pub mod world;

pub use world::{SetupError, SnapshotConfig, World};
