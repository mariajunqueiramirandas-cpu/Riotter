//! Behaviour tests on emergent outcomes (DESIGN §19). They catch regressions
//! while tuning. Heavy: run with `cargo test --release -p riot_sim --test behaviour`.

mod common;

use riot_core::Vec2;
use riot_sim::archetype::{ARCHETYPES, Archetype, Stance};
use riot_sim::command::{Command, CommandKind, DebugCommand};
use riot_sim::mapdef::*;
use riot_sim::setup::*;
use riot_sim::state::{flag, stat};
use riot_sim::{Tunables, World, testkit};

fn open_map(size: f32) -> MapDef {
    MapDef {
        name: "open".into(),
        width: size,
        height: size,
        entries: vec![
            EntryDef {
                name: "centre".into(),
                kind: EntryKind::Bus,
                pos: [0.0, 0.0],
                radius: 10.0,
            },
            EntryDef {
                name: "west".into(),
                kind: EntryKind::MapEdge,
                pos: [-size * 0.5 + 4.0, 0.0],
                radius: 4.0,
            },
            EntryDef {
                name: "east".into(),
                kind: EntryKind::MapEdge,
                pos: [size * 0.5 - 4.0, 0.0],
                radius: 4.0,
            },
        ],
        ..Default::default()
    }
}

fn crowd_setup(
    map: &MapDef,
    people: u32,
    radius: f32,
    objective: Option<(ObjectiveKind, f32)>,
) -> RiotSetup {
    let mut crowd = [0u32; ARCHETYPES];
    crowd[Archetype::Worker as usize] = people;
    let mut s = RiotSetup {
        seed: 11,
        map: map.name.clone(),
        crowd,
        ..Default::default()
    };
    let mut m = map.clone();
    m.entries[0].radius = radius;
    s.arrivals = vec![Arrival::new("centre", 0.0, 1.0, 1.0)];
    if let Some((kind, r)) = objective {
        s.objectives.push(ObjectiveDef {
            name: "centre".into(),
            kind,
            area: None,
            pos: [0.0, 0.0],
            radius: r,
            weight: 1.0,
            announced: true,
            stance: Stance::Peaceful,
        });
    }
    s
}

fn people_within(w: &World, p: Vec2, r: f32) -> usize {
    (0..w.people.len)
        .filter(|&i| !w.people.is_dead(i) && (w.people.pos(i) - p).len() <= r)
        .count()
}

/// After a 5/m² crowd settles, fewer than 0.1% of touching pairs overlap by
/// more than 20% (DESIGN §19, §11.2).
#[test]
fn packed_crowd_barely_overlaps() {
    let people = 3000u32;
    let r = (people as f32 / (5.0 * core::f32::consts::PI)).sqrt();
    let mut map = open_map(256.0);
    map.entries[0].radius = r;
    // Personal hold spots spread over the disc at about 5/m²: a standing,
    // settled crowd.
    let hold_r = r / Tunables::default().hold_radius;
    let setup = crowd_setup(&map, people, r, Some((ObjectiveKind::Hold, hold_r)));
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    for _ in 0..2400 {
        w.tick(&[]);
    }
    let (mut touching, mut bad) = (0u32, 0u32);
    for i in 0..w.people.len {
        if w.people.is_dead(i) {
            continue;
        }
        for &j in w.spatial.neighbours(i) {
            let j = j as usize;
            if j <= i {
                continue;
            }
            let rr = w.spatial.radius[i] + w.spatial.radius[j];
            let d = (w.people.pos(i) - w.people.pos(j)).len();
            if d < rr {
                touching += 1;
                if rr - d > 0.2 * rr {
                    bad += 1;
                }
            }
        }
    }
    let mut acts = [0u32; 20];
    let (mut speed, mut dist) = (0.0f32, 0.0f32);
    for i in 0..w.people.len {
        acts[w.people.stat[i][stat::ACTION] as usize] += 1;
        speed += w.people.vel(i).len();
        dist += w.people.pos(i).len();
    }
    let fear: f32 = (0..w.people.len)
        .map(|i| riot_core::widen_u8(w.people.emo[i][riot_sim::state::emo::FEAR]))
        .sum::<f32>()
        / people as f32;
    let press: f32 = (0..w.people.len)
        .map(|i| riot_core::widen_u8(w.people.pose[i][riot_sim::state::pose::PRESSURE]))
        .sum::<f32>()
        / people as f32;
    let maxp = (0..w.people.len)
        .map(|i| w.people.pose[i][riot_sim::state::pose::PRESSURE])
        .max()
        .unwrap();
    eprintln!(
        "fear {fear:.2} pressure mean {press:.3} max {maxp} injuries {} deaths {}",
        w.outcome.injuries, w.outcome.deaths
    );
    eprintln!(
        "acts {acts:?} mean speed {:.2} mean dist {:.1} r {r:.1} flow {:?}",
        speed / people as f32,
        dist / people as f32,
        w.flow.stats
    );
    let inner =
        people_within(&w, Vec2::ZERO, r * 0.5) as f32 / (core::f32::consts::PI * r * r * 0.25);
    eprintln!("touching pairs {touching}, over 20%: {bad}, core density {inner:.2}/m²");
    assert!(touching > people, "the crowd is not packed");
    assert!(inner > 3.5, "core density {inner}");
    assert!(
        (bad as f32) < touching as f32 * 0.001,
        "{bad} of {touching} pairs overlap by more than 20%"
    );
}

/// A settled crowd of `people` holding a disc at about `density`/m².
fn settled(people: u32, density: f32, seed: u64) -> World {
    let r = (people as f32 / (density * core::f32::consts::PI)).sqrt();
    let mut map = open_map(256.0);
    map.entries[0].radius = r;
    let hold_r = r / Tunables::default().hold_radius;
    let mut setup = crowd_setup(&map, people, r, Some((ObjectiveKind::Hold, hold_r)));
    setup.seed = seed;
    World::new(&setup, &map, Tunables::default()).unwrap()
}

/// A gas canister in a crowd cuts density within 10 m by at least half
/// within 10 s (DESIGN §19).
#[test]
fn gas_clears_a_crowd() {
    let mut w = settled(1500, 2.0, 5);
    for _ in 0..1800 {
        w.tick(&[]);
    }
    let before = people_within(&w, Vec2::ZERO, 10.0);
    let gas = Command::at(
        w.tick,
        CommandKind::Debug(DebugCommand::Gas { x: 0.0, y: 0.0 }),
    );
    w.tick(&[gas]);
    for _ in 0..199 {
        w.tick(&[]);
    }
    let after = people_within(&w, Vec2::ZERO, 10.0);
    let mut acts = [0u32; 20];
    let mut speed = 0.0;
    let mut n = 0;
    for i in 0..w.people.len {
        if !w.people.is_dead(i) && w.people.pos(i).len() < 15.0 {
            acts[w.people.stat[i][stat::ACTION] as usize] += 1;
            speed += w.people.vel(i).len();
            n += 1;
        }
    }
    eprintln!(
        "gas at centre {:.2}, at 8 m {:.2}; near: acts {:?} mean speed {:.2}; fallen {}",
        w.fields.sample(riot_sim::fields::Layer::Gas, 0.0, 0.0),
        w.fields.sample(riot_sim::fields::Layer::Gas, 8.0, 0.0),
        &acts[..12],
        speed / n.max(1) as f32,
        (0..w.people.len)
            .filter(|&i| w.people.stat[i][stat::FLAGS] & flag::DOWN != 0)
            .count()
    );
    eprintln!("within 10 m: {before} before, {after} 10 s after the canister");
    assert!(before > 300, "crowd not settled: {before}");
    assert!(after * 2 <= before, "{before} → {after}");
}

/// Calling everyone to one spot crushes people (DESIGN §2.6). Two minutes:
/// the panicked at the edge of the crowd can get away through the entry,
/// so the pressure in the middle takes a while to hurt people.
#[test]
fn calling_everyone_to_one_spot_crushes() {
    let (map, setup) = testkit::crush(3000, 3);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    for _ in 0..2400 {
        w.tick(&[]);
    }
    let fallen = (0..w.people.len)
        .filter(|&i| w.people.stat[i][stat::FLAGS] & flag::DOWN != 0)
        .count();
    let o = &w.outcome;
    let crush = o.harm[riot_sim::outcome::Harm::Crush as usize];
    eprintln!(
        "injuries {} fallen {fallen} deaths {} crush harm {crush}",
        o.injuries, o.deaths
    );
    // A crush: pressure knocks hundreds down and injures dozens, and the
    // harm is crowd pressure, not anything else.
    assert!(fallen > 200, "only {fallen} fell");
    assert!(o.injuries > 30, "only {} injured", o.injuries);
    assert!(crush > o.harm.iter().sum::<u32>() / 2, "harm {:?}", o.harm);
}

/// A broadcast reaches most of a crowd by word of mouth, faster with phones
/// than under an internet shutdown (DESIGN §13).
#[test]
fn broadcasts_spread() {
    let run = |mode: CommsMode| {
        let map = testkit::district(512.0, 64.0);
        let mut setup = testkit::central(&map, 1500, 9);
        setup.comms.mode = mode;
        let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
        for _ in 0..1500 {
            w.tick(&[]);
        }
        // A new objective on the far side, announced by one organizer.
        let org = w.police.organizers[0];
        let cmds = [
            Command::at(
                w.tick,
                CommandKind::PlaceObjective {
                    slot: 1,
                    kind: ObjectiveKind::Hold,
                    x: 100.0,
                    y: 100.0,
                    radius: 20.0,
                },
            ),
            Command::at(
                w.tick,
                CommandKind::Broadcast {
                    organizer: org,
                    objective: 1,
                },
            ),
        ];
        w.tick(&cmds);
        let id = w.msgs.latest;
        for _ in 0..600 {
            w.tick(&[]);
        }
        let civ = (0..w.people.len)
            .filter(|&i| {
                !w.people.is_dead(i)
                    && !Archetype::from_u8(w.people.stat[i][stat::ARCH]).is_officer()
            })
            .count();
        let heard = (0..w.people.len)
            .filter(|&i| {
                !w.people.is_dead(i) && riot_sim::state::belief::id(w.people.belief[i]) == id
            })
            .count();
        heard as f32 / civ as f32
    };
    let phones = run(CommsMode::Phones);
    let shutdown = run(CommsMode::Shutdown);
    eprintln!(
        "heard after 30 s: {:.0}% with phones, {:.0}% under shutdown",
        phones * 100.0,
        shutdown * 100.0
    );
    assert!(phones > 0.5);
    assert!(shutdown < phones);
}

/// A crowd holding the middle of an open square, and `squads` line squads
/// staged 40 m away with a cap of `cap`.
fn crowd_and_police(people: u32, squads: u32, kind: SquadKind, cap: Escalation) -> World {
    let r = (people as f32 / (1.5 * core::f32::consts::PI)).sqrt();
    let mut map = open_map(256.0);
    map.entries[0].radius = r;
    map.staging.push(NamedPoint {
        name: "stage".into(),
        pos: [0.0, -r - 30.0],
    });
    map.depots.push(NamedPoint {
        name: "depot".into(),
        pos: [0.0, -110.0],
    });
    let hold_r = r / Tunables::default().hold_radius;
    let mut setup = crowd_setup(&map, people, r, Some((ObjectiveKind::Hold, hold_r)));
    setup.escalation_cap = cap;
    for _ in 0..squads {
        setup.police.squads.push(SquadDef {
            kind,
            officers: 10,
            depot: "depot".into(),
            staging: Some("stage".into()),
            available_min: 0.0,
        });
    }
    World::new(&setup, &map, Tunables::default()).unwrap()
}

/// A kettle closes around a 500-person cluster within 60 s (DESIGN §19).
#[test]
fn kettle_closes() {
    let mut w = crowd_and_police(500, 6, SquadKind::Line, Escalation::LinesAndKettles);
    for _ in 0..600 {
        w.tick(&[]);
    }
    // Trouble in the crowd: the state moves to lines and kettles.
    let t = w.tick;
    w.tick(&[
        Command::at(t, CommandKind::Debug(DebugCommand::Escalate { rung: 1 })),
        Command::at(
            t,
            CommandKind::Debug(DebugCommand::Threat {
                x: 0.0,
                y: 0.0,
                amount: 30.0,
            }),
        ),
    ]);
    let start = w.tick;
    let mut closed = None;
    for _ in 0..1200 {
        w.tick(&[]);
        if closed.is_none()
            && w.outcome
                .notable
                .iter()
                .any(|n| n.kind == NotableKind::KettleClosed)
        {
            closed = Some(w.tick - start);
        }
    }
    let kettling = w
        .police
        .squads
        .iter()
        .filter(|s| s.doctrine == riot_sim::police::Doctrine::Kettle)
        .count();
    eprintln!(
        "kettling squads {kettling}, closed after {closed:?} ticks, clusters {:?}",
        w.police
            .clusters
            .iter()
            .map(|c| c.count as u32)
            .collect::<Vec<_>>()
    );
    assert!(closed.is_some_and(|t| t <= 1200), "kettle never closed");
}

/// A bottle thrown at a shield line never passes through an officer
/// (DESIGN §19).
#[test]
fn bottles_never_pass_a_shield_line() {
    let mut w = crowd_and_police(400, 2, SquadKind::Riot, Escalation::Presence);
    // Wait for both lines to form.
    for _ in 0..4000 {
        w.tick(&[]);
        let formed = w.police.squads.iter().all(|s| {
            s.doctrine == riot_sim::police::Doctrine::HoldLine
                && s.officers[..s.n as usize].iter().all(|&o| {
                    w.index_of(o).is_some_and(|i| {
                        (w.people.pos(i) - w.officer_targets[o as usize].pos).len() < 0.5
                    })
                })
        });
        if formed && w.tick % 20 == 10 {
            break;
        }
    }
    let lines: Vec<_> = w
        .police
        .squads
        .iter()
        .filter(|s| s.doctrine == riot_sim::police::Doctrine::HoldLine)
        .copied()
        .collect();
    assert!(!lines.is_empty(), "no line formed");
    let mut launched = 0u32;
    for sq in &lines {
        let side = sq.facing.perp();
        // From between the crowd and the line, at points along it, at angles.
        for k in 0..30 {
            let along = (k as f32 / 29.0 - 0.5) * sq.width * 0.8;
            let aim = sq.anchor + side * along;
            for tilt in [-0.5f32, 0.0, 0.5] {
                let dir = (sq.facing + side * tilt).normalize_or_zero();
                w.hazards.launch(
                    riot_sim::hazards::ProjectileKind::Stone,
                    aim + dir * 6.0,
                    aim - dir * 6.0,
                    u32::MAX,
                    15.0,
                );
                launched += 1;
            }
        }
    }
    let (b0, h0) = (w.hazards.blocked, w.hazards.hits);
    for _ in 0..40 {
        w.tick(&[]);
    }
    let (blocked, hit) = (w.hazards.blocked - b0, w.hazards.hits - h0);
    let landed_behind = launched - blocked - hit - w.hazards.projectiles.len() as u32;
    assert_eq!(landed_behind, 0, "projectiles passed the shield line");
    assert!(blocked > hit, "the shields should take most of it");
}

/// A shield faces where its officer faces and covers the body from the front.
#[test]
fn shields_cover_officers_from_the_front() {
    let mut w = crowd_and_police(0, 1, SquadKind::Riot, Escalation::Presence);
    for _ in 0..200 {
        w.tick(&[]);
    }
    let o = w.police.squads[0].officers[0];
    let i = w.index_of(o).unwrap();
    let c = w.people.pos(i);
    let f = Vec2::from_heading(w.people.pose[i][riot_sim::state::pose::HEADING]);
    let mut out = Vec::new();
    for off in [-0.3f32, -0.15, 0.0, 0.15, 0.3] {
        let from = c + f * 10.0 + f.perp() * off;
        let to = c - f * 5.0 + f.perp() * off;
        let (b0, h0) = (w.hazards.blocked, w.hazards.hits);
        w.hazards.launch(
            riot_sim::hazards::ProjectileKind::Stone,
            from,
            to,
            u32::MAX,
            15.0,
        );
        for _ in 0..30 {
            w.tick(&[]);
        }
        out.push((off, w.hazards.blocked - b0, w.hazards.hits - h0));
    }
    assert!(
        out.iter().all(|&(_, b, h)| b == 1 && h == 0),
        "(offset, blocked, hit) {out:?}"
    );
}

/// Gas, a burning car and radicals at the police line hurt people, but a
/// riot is not a massacre: tear gas never kills, officers wear armour,
/// throws arc over the crowd and routes avoid fire.
#[test]
fn a_violent_riot_is_not_a_massacre() {
    use riot_sim::outcome::Harm;
    let (map, setup) = common::violent(1200);
    let script = common::script(&map, 500);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    for _ in 0..3000 {
        common::step(&mut w, &script);
    }
    let o = &w.outcome;
    eprintln!(
        "injuries {} deaths {} arrests {}; harm {:?}; deaths by {:?}",
        o.injuries, o.deaths, o.arrests, o.harm, o.deaths_by
    );
    assert!(
        o.harm[Harm::Gas as usize] > 0,
        "the gas never reached anyone"
    );
    assert!(
        o.harm[Harm::Projectile as usize] > 0,
        "nobody threw anything"
    );
    assert_eq!(o.deaths_by[Harm::Gas as usize], 0, "gas killed someone");
    assert!(o.deaths <= 3, "{} deaths", o.deaths);
}
