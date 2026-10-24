//! Data files → `riot_sim` types (DESIGN §4.1). Serde lives here so the
//! simulation crate stays free of it; the dependency points from
//! `riot_data` to `riot_sim`, never back.
//!
//! - [`tiled`]: Tiled JSON maps → [`MapDef`](riot_sim::MapDef)
//!   ([`load_map`], [`parse_map`]); the convention is in `data/maps/README.md`.
//! - [`citygen`]: a procedural city up to 8 km across, written as a Tiled map.
//! - [`scenario`]: RON scenarios → [`RiotSetup`](riot_sim::RiotSetup) plus
//!   their map and tunables ([`load_scenario`]); the format is in
//!   `data/scenarios/README.md`.
//! - [`tunables`]: `tunables.ron` ([`load_tunables`], [`parse_tunables`]) and
//!   hot-reload commands ([`tunable_commands`]).
//!
//! Every loader returns a [`DataError`] that names the file and the place in
//! it.

pub mod citygen;
mod error;
mod math;
pub mod scenario;
pub mod tiled;
pub mod tunables;

pub use error::DataError;
pub use scenario::{Scenario, load_scenario};
pub use tiled::{load_map, parse_map};
pub use tunables::{load_tunables, parse_tunables, tunable_commands};
