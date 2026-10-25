//! The renderer's CPU-side geometry: textured triangle meshes built in
//! layers ([`mesh`]), the map's static ground and roof meshes built from the
//! `MapDef` ([`map`]) and its street lamps ([`lamps`]). Nothing here touches
//! the GPU: `riot_render` uploads and draws what these build.

pub mod lamps;
pub mod map;
pub mod mesh;
