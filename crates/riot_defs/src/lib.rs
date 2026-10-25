//! What the riot simulation is made of, without the simulation itself
//! (DESIGN §2, §6.2, §17): archetypes, the codes in the agent table, the
//! tunables, the map and setup as authored, commands and events, and the
//! small enums the renderer, the HUD and the tools read back.
//!
//! Nothing here runs during a tick, and nothing depends on more than
//! `riot_core`, so `riot_data` and `riot_render` can build against these
//! definitions without waiting for the simulation.

#![deny(unsafe_code)]

pub mod archetype;
pub mod command;
pub mod event;
pub mod harm;
pub mod mapdef;
pub mod phase;
pub mod police;
pub mod setup;
pub mod state;
pub mod targets;
pub mod tunables;

pub use archetype::{ARCHETYPES, Archetype, Stance};
pub use command::{Command, CommandKind, DebugCommand};
pub use mapdef::MapDef;
pub use phase::{NoProfiler, PHASES, Phase, Profiler};
pub use setup::{DT, RiotOutcome, RiotSetup, TICK_HZ};
pub use tunables::{TUNABLES, Tunables};
