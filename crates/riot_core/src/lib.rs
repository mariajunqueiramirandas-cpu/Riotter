//! Foundation types for the riot simulation (DESIGN §4.1).
//!
//! Everything here is plain, deterministic arithmetic: no threads, no clocks,
//! no I/O and no std transcendentals (DESIGN §12 rule 7). Small functions are
//! `#[inline]` because dev builds have no LTO (DESIGN §16).

#![forbid(unsafe_code)]

pub mod grid;
pub mod hash;
pub mod lanes;
pub mod math;
pub mod rng;
pub mod sort;
pub mod sparse;

pub use grid::{demorton2, morton2};
pub use hash::StateHasher;
pub use lanes::{CHUNK, CHUNK_AGENTS, F32x8, LANES};
pub use math::{Vec2, narrow_u8, widen_u8};
pub use rng::{Stream, mix, roll, roll_key, unit};
pub use sort::{Buckets, RadixSort24};
pub use sparse::SparseSet;
