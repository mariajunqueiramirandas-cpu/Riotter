//! The procedural city: sound geometry, connected streets, runnable riots.
//! Slow 8 km checks are `#[ignore]`d; run them in release:
//! `cargo test --release -p riot_data_citygen --test citygen -- --ignored`.

use std::time::Instant;

use riot_core::Vec2;
use riot_data_citygen::{self as citygen, CityParams};
use riot_data_tiled::parse_map;
use riot_sim::map::Map;
use riot_sim::mapdef::{EntryKind, MapDef, SolidKind};
use riot_sim::setup::{Arrival, ObjectiveDef, ObjectiveKind, SquadDef, SquadKind};
use riot_sim::{Archetype, RiotSetup, Stance, Tunables, World};

fn seg_dist2(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a[0] + dx * t - p[0], a[1] + dy * t - p[1]);
    x * x + y * y
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Distance between two segments.
fn seg_seg(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2]) -> f32 {
    let (d1, d2) = (cross(a, b, c), cross(a, b, d));
    let (d3, d4) = (cross(c, d, a), cross(c, d, b));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    [
        seg_dist2(a, c, d),
        seg_dist2(b, c, d),
        seg_dist2(c, a, b),
        seg_dist2(d, a, b),
    ]
    .into_iter()
    .fold(f32::MAX, f32::min)
    .sqrt()
}

fn inside(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0])
        {
            inside = !inside;
        }
    }
    inside
}

/// Solids bucketed by 32 m cell, for neighbourhood queries.
struct Buckets {
    dim: usize,
    half: f32,
    cells: Vec<Vec<u32>>,
}

const CELL: f32 = 32.0;

impl Buckets {
    fn new(m: &MapDef) -> Buckets {
        let dim = (m.width.max(m.height) / CELL).ceil() as usize + 1;
        let half = m.width.max(m.height) * 0.5;
        let mut b = Buckets {
            dim,
            half,
            cells: vec![Vec::new(); dim * dim],
        };
        for (n, s) in m.solids.iter().enumerate() {
            let (lo, hi) = bbox(&s.points);
            for c in b.cells_in(lo, hi) {
                b.cells[c].push(n as u32);
            }
        }
        b
    }
    fn cells_in(&self, lo: [f32; 2], hi: [f32; 2]) -> Vec<usize> {
        let f = |v: f32| (((v + self.half) / CELL).floor().max(0.0) as usize).min(self.dim - 1);
        let mut v = Vec::new();
        for y in f(lo[1])..=f(hi[1]) {
            for x in f(lo[0])..=f(hi[0]) {
                v.push(y * self.dim + x);
            }
        }
        v
    }
    fn near(&self, lo: [f32; 2], hi: [f32; 2]) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .cells_in(lo, hi)
            .into_iter()
            .flat_map(|c| self.cells[c].iter().copied())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn bbox(pts: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    pts.iter().fold(
        ([f32::MAX, f32::MAX], [f32::MIN, f32::MIN]),
        |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1])],
                [hi[0].max(p[0]), hi[1].max(p[1])],
            )
        },
    )
}

/// Region node of the walkable cell nearest `p`.
fn rnode(m: &Map, p: Vec2) -> Option<u32> {
    let q = m.nearest_open(p, 8.0)?;
    let (cx, cy) = m.cell_of(q)?;
    m.rnode_of(m.tile_index(cx >> 4, cy >> 4), m.comp_of(cx, cy)?)
}

/// Everything a generated city promises (DESIGN §2.1 and the generator's
/// contract with scenarios).
fn check_city(def: &MapDef) {
    // Named places.
    let tag = |t: &str| def.areas.iter().filter(|a| a.tag == t).count();
    assert!(def.area("central_square").is_some());
    assert_eq!(
        def.area("government_quarter").map(|a| a.tag.as_str()),
        Some("government")
    );
    assert!(tag("square") >= 3, "squares: {}", tag("square"));
    assert!(tag("park") >= 1);
    assert!(tag("bridge") >= 2, "bridges: {}", tag("bridge"));
    assert!((2..=6).contains(&def.depots.len()));
    assert!(def.staging.len() >= tag("square"));
    assert!(def.entries.iter().any(|e| e.kind == EntryKind::Metro));
    for side in ["n", "s", "w", "e"] {
        let n = def
            .entries
            .iter()
            .filter(|e| e.name.starts_with(&format!("edge_{side}")))
            .count();
        assert!(n >= 3, "{n} entries on edge {side}");
    }
    assert!(def.solids.iter().any(|s| s.kind == SolidKind::Water));
    assert!(
        def.solids.iter().any(|s| s.kind == SolidKind::Wall),
        "bridge railings"
    );
    assert!(!def.overhead.is_empty() && !def.props.is_empty());
    let kinds = |k: u8| def.ground.kinds.iter().filter(|&&g| g == k).count();
    assert!((0..5).all(|k| kinds(k) > 0), "every ground kind is painted");
    assert_eq!(def.ground.cell, 4.0);
    check_geometry(def);
}

/// What holds for any generated map: clear roads and markers, connected
/// streets, everything reachable on foot.
fn check_geometry(def: &MapDef) {
    // No solid within 1 m of a road centreline.
    let buckets = Buckets::new(def);
    for e in &def.roads.edges {
        let (a, b) = (
            def.roads.nodes[e[0] as usize],
            def.roads.nodes[e[1] as usize],
        );
        let (lo, hi) = bbox(&[a, b]);
        for s in buckets.near([lo[0] - 2.0, lo[1] - 2.0], [hi[0] + 2.0, hi[1] + 2.0]) {
            let pts = &def.solids[s as usize].points;
            let d = (0..pts.len())
                .map(|i| seg_seg(a, b, pts[i], pts[(i + 1) % pts.len()]))
                .fold(f32::MAX, f32::min);
            assert!(
                d >= 1.0 && !inside(a, pts),
                "{:?} solid {s} is {d} m from road {a:?}–{b:?}",
                def.solids[s as usize].kind
            );
        }
    }

    // No solid on an entry, depot, staging point or area centre.
    let marks = def
        .entries
        .iter()
        .map(|e| (e.pos, 1.0, e.name.as_str()))
        .chain(
            def.depots
                .iter()
                .chain(&def.staging)
                .map(|d| (d.pos, 2.0, d.name.as_str())),
        )
        .chain(def.areas.iter().map(|a| (a.pos, 2.0, a.name.as_str())));
    for (p, r, name) in marks {
        for s in buckets.near([p[0] - r, p[1] - r], [p[0] + r, p[1] + r]) {
            let pts = &def.solids[s as usize].points;
            let d = (0..pts.len())
                .map(|i| seg_dist2(p, pts[i], pts[(i + 1) % pts.len()]))
                .fold(f32::MAX, f32::min);
            assert!(!inside(p, pts) && d >= r * r, "a solid blocks {name}");
        }
    }

    // The road graph is one piece.
    let n = def.roads.nodes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for e in &def.roads.edges {
        let (a, b) = (
            find(&mut parent, e[0] as usize),
            find(&mut parent, e[1] as usize),
        );
        parent[a] = b;
    }
    let root = find(&mut parent, 0);
    assert!(
        (0..n).all(|i| find(&mut parent, i) == root),
        "road graph is split"
    );

    // Walking: every square, area, depot and staging point is reachable from
    // every entry (BFS over the nav region graph).
    let m = Map::build(def, 0.47).expect("nav tiles");
    let start = rnode(&m, m.entries[0].pos).expect("first entry is on the nav grid");
    let mut seen = vec![false; m.rnodes.len()];
    let mut stack = vec![start];
    while let Some(r) = stack.pop() {
        if std::mem::replace(&mut seen[r as usize], true) {
            continue;
        }
        stack.extend(m.region_neighbours(r).iter().map(|e| e.0));
    }
    let places = m.entries.iter().map(|e| (e.pos, &e.name)).chain(
        m.areas
            .iter()
            .chain(&m.depots)
            .chain(&m.staging)
            .map(|a| (a.pos, &a.name)),
    );
    for (p, name) in places {
        let r = rnode(&m, p).unwrap_or_else(|| panic!("{name} is not on the nav grid"));
        assert!(
            seen[r as usize],
            "{name} at {p:?} cannot be reached on foot"
        );
    }
}

/// A small riot on a generated city.
fn setup(def: &MapDef, people: u32) -> RiotSetup {
    let mut s = RiotSetup {
        seed: 3,
        map: def.name.clone(),
        organizers: 4,
        ..Default::default()
    };
    s.crowd[Archetype::Student as usize] = people / 2;
    s.crowd[Archetype::Worker as usize] = people - people / 2;
    for e in def.entries.iter().filter(|e| e.kind != EntryKind::Bus) {
        s.arrivals.push(Arrival::new(&e.name, 0.0, 1.0, 1.0));
    }
    for (k, d) in def.depots.iter().enumerate() {
        s.police.squads.push(SquadDef {
            kind: if k % 2 == 0 {
                SquadKind::Line
            } else {
                SquadKind::Riot
            },
            officers: 8,
            depot: d.name.clone(),
            staging: def.staging.get(k).map(|p| p.name.clone()),
            available_min: 0.0,
        });
    }
    s.police.vans_per_depot = 1;
    s.police.water_cannons = 1;
    s.objectives.push(ObjectiveDef {
        name: "square".into(),
        kind: ObjectiveKind::MarchTo,
        area: Some("central_square".into()),
        pos: [0.0, 0.0],
        radius: 0.0,
        weight: 1.0,
        announced: true,
        stance: Stance::Defensive,
    });
    s
}

#[test]
fn district_is_sound_and_runs() {
    let def = citygen::generate_mapdef(&CityParams::new(1024.0, 1));
    assert_eq!((def.width, def.height), (1024.0, 1024.0));
    assert_eq!(def.name, "generated:city?size=1024&seed=1");
    check_city(&def);
    let mut w = World::new(&setup(&def, 600), &def, Tunables::default()).unwrap();
    for _ in 0..200 {
        w.tick(&[]);
    }
    assert!(
        w.snapshot.stats.present > 100,
        "{}",
        w.snapshot.stats.present
    );
}

#[test]
fn json_round_trips_through_the_loader() {
    let p = CityParams {
        compress_ground: false,
        ..CityParams::new(1024.0, 7)
    };
    let json = citygen::generate_json(&p);
    let loaded = parse_map(&json, &p.map_name()).unwrap();
    assert_eq!(loaded, citygen::generate_mapdef(&p));
    // The `Value` form sorts keys but holds the same map.
    let value = citygen::generate(&p).to_string();
    assert_eq!(parse_map(&value, &p.map_name()).unwrap(), loaded);
    let compressed = citygen::generate_json(&CityParams::new(1024.0, 7));
    assert!(compressed.len() < json.len());
    let loaded = parse_map(&compressed, &p.map_name()).unwrap();
    assert_eq!(loaded, citygen::generate_mapdef(&p));
    assert_eq!(
        loaded,
        citygen::generate_mapdef(&CityParams::new(1024.0, 7))
    );
}

#[test]
fn deterministic_per_seed() {
    let a = citygen::generate_json(&CityParams::new(700.0, 11));
    let b = citygen::generate_json(&CityParams::new(700.0, 11));
    let c = citygen::generate_json(&CityParams::new(700.0, 12));
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn every_size_and_seed_is_sound() {
    for (size, seed) in [(512.0, 1), (512.0, 2), (777.0, 5), (1500.0, 9), (2048.0, 4)] {
        let def = citygen::generate_mapdef(&CityParams::new(size, seed));
        check_city(&def);
    }
    // Without a river there are no bridges, but the rest holds.
    let def = citygen::generate_mapdef(&CityParams {
        river: false,
        ..CityParams::new(1024.0, 2)
    });
    assert!(def.solids.iter().all(|s| s.kind != SolidKind::Wall));
    assert!(def.area("bridge_1").is_none());
}

#[test]
fn odd_parameters_still_make_a_city() {
    let mut n = 0;
    for size in [300.0, 512.0, 640.0, 1000.0, 1300.0] {
        for (block, every) in [(40.0, 1), (60.0, 2), (100.0, 4), (150.0, 7), (400.0, 3)] {
            for seed in 0..3 {
                let p = CityParams {
                    block_m: block,
                    avenue_every: every,
                    river: seed != 1,
                    max_props: if seed == 2 { 0 } else { 6000 },
                    ..CityParams::new(size, seed + n)
                };
                let def = parse_map(&citygen::generate_json(&p), "odd")
                    .unwrap_or_else(|e| panic!("{p:?}: {e}"));
                assert!(
                    def.width >= 512.0 && def.area("central_square").is_some(),
                    "{p:?}"
                );
                assert!(def.entries.len() >= 12 && def.depots.len() >= 2, "{p:?}");
                assert!(def.area("government_quarter").is_some(), "{p:?}");
                eprintln!("{p:?}");
                check_geometry(&def);
                n += 1;
            }
        }
    }
}

#[test]
#[ignore = "8 km city: run in release"]
fn city_8km() {
    let p = CityParams::new(8192.0, 3);
    let t = Instant::now();
    let json = citygen::generate_json(&p);
    let generated = t.elapsed();
    eprintln!(
        "8 km city: {:.2} MB of JSON in {generated:.2?}",
        json.len() as f64 / 1e6
    );
    assert!(generated.as_secs_f32() < 10.0);
    assert!(json.len() < 10_000_000);
    let t = Instant::now();
    let def = parse_map(&json, &p.map_name()).unwrap();
    eprintln!("parsed in {:.2?}", t.elapsed());
    assert_eq!((def.width, def.height), (8192.0, 8192.0));
    assert!(def.props.len() <= 6000, "{} props", def.props.len());
    let t = Instant::now();
    let map = Map::build(&def, 0.47).unwrap();
    eprintln!(
        "nav tiles in {:.2?}, {} region nodes",
        t.elapsed(),
        map.rnodes.len()
    );
    let t = Instant::now();
    check_city(&def);
    eprintln!("checked in {:.2?}", t.elapsed());
    let mut w = World::new(&setup(&def, 2000), &def, Tunables::default()).unwrap();
    for _ in 0..100 {
        w.tick(&[]);
    }
}
