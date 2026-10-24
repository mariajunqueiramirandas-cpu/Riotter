//! Riotter's real-time riot simulation (DESIGN.md).
//!
//! Headless, deterministic, fixed-tick: one map (up to an 8 km city), one day
//! of protest, thousands of autonomous people against a police force. A
//! [`RiotSetup`] goes in, a [`RiotOutcome`] comes out, and the renderer only
//! watches [`Snapshot`]s. No window, no clock, no I/O (DESIGN §4.2).

#![deny(unsafe_code)]

pub mod archetype;
pub mod command;
pub mod event;
pub mod fields;
pub mod flow;
pub mod hazards;
pub mod interact;
pub mod map;
pub mod mapdef;
pub mod messages;
pub mod outcome;
pub mod people;
pub mod physics;
pub mod police;
pub mod setup;
pub mod snapshot;
mod snapshot_write;
pub mod spatial;
pub mod state;
pub mod steer;
pub mod testkit;
pub mod think;
pub mod tunables;
pub mod world;

pub use archetype::{ARCHETYPES, Archetype, Stance};
pub use command::{Command, CommandKind, DebugCommand};
pub use mapdef::MapDef;
pub use setup::{DT, RiotOutcome, RiotSetup, TICK_HZ};
pub use snapshot::{Overlay, Snapshot};
pub use tunables::{TUNABLES, Tunables};
pub use world::{NoProfiler, Phase, Profiler, SetupError, SnapshotConfig, World};
