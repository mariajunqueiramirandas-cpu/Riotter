//! Procedural maps and setups for tests, benchmarks and the headless tool.
//! The real maps are authored in Tiled (see `riot_data`); these exist so the
//! simulation can be exercised without any files.

use crate::archetype::{ARCHETYPES, Archetype, Stance};
use crate::mapdef::*;
use crate::setup::*;

fn rect(kind: SolidKind, x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon {
    Polygon {
        kind,
        points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
    }
}

/// A square district `size` metres across: a grid of building blocks with
/// streets, a central square, entries along the edges and two police depots.
/// `block` is the block pitch (building + street).
pub fn district(size: f32, block: f32) -> MapDef {
    let half = size * 0.5;
    let street = (block * 0.18).clamp(10.0, 24.0);
    let n = (size / block).floor() as i32;
    let off = -(n as f32 * block) * 0.5;
    let mut solids = Vec::new();
    let mut props = Vec::new();
    let square_r = block * 1.2;
    for by in 0..n {
        for bx in 0..n {
            let x0 = off + bx as f32 * block + street * 0.5;
            let y0 = off + by as f32 * block + street * 0.5;
            let (x1, y1) = (x0 + block - street, y0 + block - street);
            let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            if cx * cx + cy * cy < square_r * square_r {
                continue; // the square
            }
            // Two buildings per block with an alley between them.
            let alley = 3.0;
            let mid = (x0 + x1) * 0.5;
            solids.push(rect(SolidKind::Building, x0, y0, mid - alley * 0.5, y1));
            solids.push(rect(SolidKind::Building, mid + alley * 0.5, y0, x1, y1));
            // A parked car along the block's north side.
            props.push(PropDef {
                kind: PropKind::Car,
                pos: [x0 + 6.0, y0 - 2.2],
                half: [2.2, 0.9],
                angle: 0.0,
            });
        }
    }
    let mut entries = Vec::new();
    let edge = half - 6.0;
    for k in 0..4 {
        let t = off + (k as f32 + 0.5) * (n as f32 * block / 4.0);
        let t = (((t - off) / block).floor() * block + off).clamp(-half + 8.0, half - 8.0);
        for (name, pos) in [
            ("w", [-edge, t]),
            ("e", [edge, t]),
            ("n", [t, -edge]),
            ("s", [t, edge]),
        ] {
            entries.push(EntryDef {
                name: format!("{name}{k}"),
                kind: EntryKind::MapEdge,
                pos,
                radius: 5.0,
            });
        }
    }
    entries.push(EntryDef {
        name: "metro_centre".into(),
        kind: EntryKind::Metro,
        pos: [square_r * 0.6, 0.0],
        radius: 4.0,
    });
    // Inner entries on the avenues just around the square.
    let inner = 2.0 * block;
    for (name, pos) in [
        ("inner_w", [-inner, 0.0]),
        ("inner_e", [inner, 0.0]),
        ("inner_n", [0.0, -inner]),
        ("inner_s", [0.0, inner]),
    ] {
        entries.push(EntryDef {
            name: name.into(),
            kind: EntryKind::Bus,
            pos,
            radius: 10.0,
        });
    }
    // Roads along the street grid.
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let m = n + 1;
    for j in 0..m {
        for i in 0..m {
            nodes.push([off + i as f32 * block, off + j as f32 * block]);
            let id = (j * m + i) as u32;
            if i > 0 {
                edges.push([id - 1, id]);
            }
            if j > 0 {
                edges.push([id - m as u32, id]);
            }
        }
    }
    let corner = off + block * 0.0;
    MapDef {
        name: format!("district{}", size as u32),
        width: size,
        height: size,
        solids,
        overhead: Vec::new(),
        props,
        ground: GroundLayer::default(),
        roads: RoadGraph { nodes, edges },
        entries,
        depots: vec![
            NamedPoint {
                name: "depot_west".into(),
                pos: [corner, corner],
            },
            NamedPoint {
                name: "depot_east".into(),
                pos: [-corner, -corner],
            },
        ],
        staging: vec![NamedPoint {
            name: "staging_north".into(),
            pos: [0.0, -square_r - block * 0.5],
        }],
        areas: vec![
            AreaDef {
                name: "square".into(),
                tag: "square".into(),
                pos: [0.0, 0.0],
                radius: square_r * 0.7,
            },
            AreaDef {
                name: "ministry".into(),
                tag: "government".into(),
                pos: [0.0, -square_r - block],
                radius: 20.0,
            },
        ],
    }
}

/// A crowd of `people` marching to the square, with `squads` police squads.
pub fn march(map: &MapDef, people: u32, squads: u32, seed: u64) -> RiotSetup {
    let mut crowd = [0u32; ARCHETYPES];
    let share = [
        (Archetype::Student, 0.32),
        (Archetype::Worker, 0.32),
        (Archetype::Elder, 0.12),
        (Archetype::Radical, 0.12),
        (Archetype::Provocateur, 0.01),
        (Archetype::Journalist, 0.02),
        (Archetype::Bystander, 0.09),
    ];
    let mut total = 0;
    for (a, s) in share {
        let c = (people as f32 * s) as u32;
        crowd[a as usize] = c;
        total += c;
    }
    crowd[Archetype::Worker as usize] += people.saturating_sub(total);
    let arrivals = map
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::MapEdge)
        .map(|e| Arrival::new(&e.name, 0.0, 3.0, 1.0))
        .collect();
    let mut police = PoliceForce {
        vans_per_depot: 2,
        water_cannons: 1,
        aggression: 0.5,
        ..Default::default()
    };
    for k in 0..squads {
        let kind = match k % 4 {
            0 | 1 => SquadKind::Line,
            2 => SquadKind::Riot,
            _ => SquadKind::Snatch,
        };
        police.squads.push(SquadDef {
            kind,
            officers: 10,
            depot: if k % 2 == 0 {
                "depot_west".into()
            } else {
                "depot_east".into()
            },
            staging: if k < squads / 2 {
                Some("staging_north".into())
            } else {
                None
            },
            available_min: if k < squads / 2 {
                0.0
            } else {
                2.0 + k as f32 * 0.5
            },
        });
    }
    RiotSetup {
        seed,
        map: map.name.clone(),
        crowd,
        arrivals,
        organizers: (people / 400).clamp(1, 50) as u16,
        supplies: Supplies {
            water: 200,
            medic: 60,
            shields: 40,
            barricade: 40,
        },
        police,
        escalation_cap: Escalation::GasAndWater,
        comms: Comms::default(),
        weather: Weather::default(),
        objectives: vec![ObjectiveDef {
            name: "square".into(),
            kind: ObjectiveKind::MarchTo,
            area: Some("square".into()),
            pos: [0.0, 0.0],
            radius: 0.0,
            weight: 1.0,
            announced: true,
            stance: Stance::Defensive,
        }],
        duration: minutes(10.0),
        start_minute: 14.0 * 60.0,
        limits: Limits::default(),
    }
}

/// `people` already standing packed around the centre of an open map, for
/// density and crush tests.
pub fn packed(size: f32, people: u32, seed: u64) -> (MapDef, RiotSetup) {
    let map = MapDef {
        name: "open".into(),
        width: size,
        height: size,
        entries: vec![EntryDef {
            name: "centre".into(),
            kind: EntryKind::MapEdge,
            pos: [0.0, 0.0],
            radius: (people as f32 / 3.0).sqrt(),
        }],
        ..Default::default()
    };
    let mut crowd = [0u32; ARCHETYPES];
    crowd[Archetype::Worker as usize] = people;
    let setup = RiotSetup {
        seed,
        map: map.name.clone(),
        crowd,
        arrivals: vec![Arrival::new("centre", 0.0, 0.0, 1.0)],
        ..Default::default()
    };
    (map, setup)
}

/// `people` arriving through the entries around the square within a minute,
/// marching to it: the same crowd whatever the map size (DESIGN §19
/// map-size test).
pub fn central(map: &MapDef, people: u32, seed: u64) -> RiotSetup {
    let mut s = march(map, people, 8, seed);
    s.arrivals = ["inner_w", "inner_e", "inner_n", "inner_s"]
        .iter()
        .map(|e| Arrival::new(e, 0.0, 1.0, 1.0))
        .collect();
    s.police.squads.iter_mut().for_each(|q| {
        q.staging = Some("staging_north".into());
        q.available_min = 0.0;
    });
    s
}
