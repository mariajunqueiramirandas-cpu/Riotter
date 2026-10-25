//! Riotter's real-time riot simulation (DESIGN.md).
//!
//! Headless, deterministic, fixed-tick: one map (up to an 8 km city), one day
//! of protest, thousands of autonomous people against a police force. A
//! [`RiotSetup`] goes in, a [`RiotOutcome`] comes out, and the renderer only
//! watches [`Snapshot`]s. No window, no clock, no I/O (DESIGN §4.2).
//!
//! This crate is the front: [`World`], whose tick runs the phases in order,
//! and every module path the simulation's users name. The code lives in the
//! crates under it, so a change to one system rebuilds one small crate:
//!
//! - definitions: `riot_defs`, `riot_snapshot`, `riot_testkit`;
//! - systems over part of the world: `riot_map`, `riot_people`,
//!   `riot_spatial`, `riot_fields`, `riot_flow`, `riot_steer`,
//!   `riot_physics`;
//! - the world's state: `riot_world`;
//! - phases over the whole world: `riot_think`, `riot_messages`,
//!   `riot_hazards`, `riot_interact`, `riot_tactics`, `riot_police`,
//!   `riot_outcome`.

#![deny(unsafe_code)]

pub use riot_defs::{archetype, command, event, mapdef, setup, state, tunables};
pub use riot_fields as fields;
pub use riot_flow as flow;
pub use riot_map as map;
pub use riot_people as people;
pub use riot_physics as physics;
pub use riot_snapshot as snapshot;
pub use riot_spatial as spatial;
pub use riot_steer as steer;
pub use riot_testkit as testkit;

pub mod world;

/// Hazards (DESIGN §2.6, §2.8): [`riot_hazards`] and their state.
pub mod hazards {
    pub use riot_hazards::*;
    pub use riot_world::hazards::*;
}

/// Phase 8: interactions (DESIGN §11.3): [`riot_interact`].
pub mod interact {
    pub use riot_interact::*;
    pub use riot_world::harm::damage;
}

/// Messages (DESIGN §13): [`riot_messages`] and the message table.
pub mod messages {
    pub use riot_messages::*;
    pub use riot_world::messages::*;
}

/// Outcome accounting (DESIGN §2.7, §17): [`riot_outcome`], its tallies and
/// the causes of harm.
pub mod outcome {
    pub use riot_defs::harm::{HARMS, Harm};
    pub use riot_outcome::*;
    pub use riot_world::outcome::*;
}

/// Police (DESIGN §2.5, §14): the director ([`riot_police`]) and the police
/// state.
pub mod police {
    pub use riot_defs::police::{DOCTRINES, Doctrine};
    pub use riot_police::*;
    pub use riot_world::police::*;
}

/// The police commander (DESIGN §14.1): [`riot_tactics`], its state and the
/// police radio.
pub mod tactics {
    pub use riot_defs::police::{
        Call, CordonState, NO_CORDON, NO_SITE, OpKind, RADIO, RadioCall, call_text,
    };
    pub use riot_tactics::*;
    pub use riot_world::commander::*;
}

/// Phase 4: minds (DESIGN §10): [`riot_think`] and its lookup tables.
pub mod think {
    pub use riot_think::*;
    pub use riot_world::minds::*;
}

pub use archetype::{ARCHETYPES, Archetype, Stance};
pub use command::{Command, CommandKind, DebugCommand};
pub use mapdef::MapDef;
pub use setup::{DT, RiotOutcome, RiotSetup, TICK_HZ};
pub use snapshot::{Overlay, Snapshot};
pub use tunables::{TUNABLES, Tunables};
pub use world::{NoProfiler, Phase, Profiler, SetupError, SnapshotConfig, World};
