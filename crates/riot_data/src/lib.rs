//! Data files → `riot_sim` types (DESIGN §4.1). Serde lives in the data
//! crates so the simulation stays free of it; the dependency points from
//! `riot_data` to `riot_sim`, never back.
//!
//! This crate is the front door: it re-exports the `riot_data_*` crates
//! under their old module names and adds loading a scenario with its map and
//! tunables. The crates are split so they compile side by side:
//!
//! - [`tiled`] (`riot_data_tiled`): Tiled JSON maps →
//!   [`MapDef`](riot_sim::MapDef) ([`load_map`], [`parse_map`]); the
//!   convention is in `data/maps/README.md`. All JSON serde code is here.
//! - [`citygen`] (`riot_data_citygen`): a procedural city up to 8 km across,
//!   written as a Tiled map; plain code without serde.
//! - [`scenario`]: RON scenarios → [`RiotSetup`](riot_sim::RiotSetup) plus
//!   their map and tunables ([`load_scenario`]); the file format is
//!   `riot_data_scenario` and is documented in `data/scenarios/README.md`.
//! - [`tunables`] (`riot_data_tunables`): `tunables.ron` ([`load_tunables`],
//!   [`parse_tunables`]) and hot-reload commands ([`tunable_commands`]).
//! - `riot_data_core`: the shared [`DataError`] and portable trigonometry.
//!
//! Every loader returns a [`DataError`] that names the file and the place in
//! it.

pub mod scenario;

pub use riot_data_citygen as citygen;
pub use riot_data_core::DataError;
pub use riot_data_tiled as tiled;
pub use riot_data_tunables as tunables;
pub use scenario::{Scenario, load_scenario};
pub use tiled::{load_map, parse_map};
pub use tunables::{load_tunables, parse_tunables, tunable_commands};
