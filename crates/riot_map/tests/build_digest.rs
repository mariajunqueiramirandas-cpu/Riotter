//! `Map::build` must derive exactly the same nav map however it gets there:
//! these digests cover every derived array (tiles, mixed tiles with their
//! bits, wall lists, labels and links, regions and their graph, places) and
//! were recorded from the original, straightforward build. A faster build
//! that changes any of them changes the simulation.
//!
//! The 8 km city is `#[ignore]`d; run it in release:
//! `cargo test --release -p riot_map --test build_digest -- --include-ignored`

use riot_core::hash::StateHasher;
use riot_data_citygen::CityParams;
use riot_defs::mapdef::MapDef;
use riot_map::Map;

fn digest(m: &Map) -> u64 {
    let mut h = StateHasher::new();
    h.write_bytes(m.name.as_bytes());
    for x in [m.width, m.height, m.size, m.half, m.nav.cell, m.nav.half] {
        h.write_f32(x);
    }
    for x in [m.nav.dim, m.tdim, m.tshift, m.rdim] {
        h.write_u32(x);
    }
    h.write_pod(&m.tiles);
    h.write_bytes(&m.tile_rc0);
    h.write_u64(m.mixed.len() as u64);
    for t in &m.mixed {
        h.write_pod(&t.bits);
        for w in t.walls {
            h.write_pod(&w);
        }
        h.write_u32(t.labels);
        h.write_u32(t.ncomp as u32);
        h.write_bytes(&t.rc);
        h.write_pod(&t.links);
    }
    h.write_u64(m.labels.len() as u64);
    for l in &m.labels {
        h.write_bytes(l);
    }
    h.write_u64(m.seg_ax.len() as u64);
    for s in 0..m.seg_ax.len() as u32 {
        let (a, b) = m.segment(s);
        for x in [a.x, a.y, b.x, b.y] {
            h.write_f32(x);
        }
    }
    h.write_pod(&m.wall_idx);
    h.write_pod(&m.region_first);
    for n in &m.rnodes {
        h.write_u32(n.region);
        h.write_f32(n.centroid.x);
        h.write_f32(n.centroid.y);
        h.write_u32(n.tiles);
    }
    h.write_pod(&m.radj_start);
    for &(n, c) in &m.radj {
        h.write_u32(n);
        h.write_u32(c);
    }
    for n in &m.roads.nodes {
        h.write_f32(n.x);
        h.write_f32(n.y);
    }
    h.write_pod(&m.roads.start);
    for &(n, d) in &m.roads.adj {
        h.write_u32(n);
        h.write_f32(d);
    }
    for e in &m.entries {
        h.write_bytes(e.name.as_bytes());
        h.write_u32(e.kind as u32);
        for x in [e.pos.x, e.pos.y, e.radius] {
            h.write_f32(x);
        }
    }
    for p in m.depots.iter().chain(&m.staging).chain(&m.areas) {
        h.write_bytes(p.name.as_bytes());
        h.write_bytes(p.tag.as_bytes());
        for x in [p.pos.x, p.pos.y, p.radius] {
            h.write_f32(x);
        }
    }
    h.write_u64(m.props.len() as u64);
    h.finish()
}

fn city(size: f32, seed: u64) -> MapDef {
    riot_data_citygen::generate_mapdef(&CityParams::new(size, seed))
}

fn check(def: &MapDef, margin: f32, want: u64) {
    let m = Map::build(def, margin).expect("nav map");
    let got = digest(&m);
    assert_eq!(
        got, want,
        "nav map of `{}` (margin {margin}) changed: digest {got:#018x}",
        def.name
    );
}

#[test]
fn testkit_district_is_unchanged() {
    check(
        &riot_testkit::district(400.0, 60.0),
        0.47,
        0xf6d8_c33c_66c4_0675,
    );
}

#[test]
fn small_city_is_unchanged() {
    check(&city(512.0, 1), 0.47, 0xecbe_0181_368f_e59b);
}

#[test]
fn odd_sized_city_is_unchanged() {
    check(&city(1500.0, 9), 0.45, 0xd6e5_9513_64e1_890f);
}

#[test]
fn tiled_district_is_unchanged() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/maps/district.json");
    let def = riot_data_tiled::load_map(std::path::Path::new(path)).expect("district.json");
    check(&def, 0.47, 0xf95b_24c2_6d95_620f);
}

#[test]
#[ignore = "8 km city: run in release"]
fn city_8km_is_unchanged() {
    check(&city(8192.0, 3), 0.47, 0xd749_55c8_a5d3_de8a);
}
