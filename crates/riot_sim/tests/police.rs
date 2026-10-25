//! The police commander's tactics (DESIGN §14): cordons across a crowd's
//! route, crowds finding another way round, ambushes. Heavy: run with
//! `cargo test --release -p riot_sim --test police`.

use std::collections::BTreeSet;

use riot_sim::archetype::{ARCHETYPES, Archetype, Stance};
use riot_sim::command::{Command, CommandKind, DebugCommand};
use riot_sim::mapdef::*;
use riot_sim::setup::*;
use riot_sim::state::stat;
use riot_sim::tactics::CordonState;
use riot_sim::{Tunables, World};

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon {
    Polygon {
        kind: SolidKind::Building,
        points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
    }
}

/// A 256 m map cut in two by a row of buildings 20 m deep along x = 0,
/// with two 10 m gaps through it: north (y 30..40) and south (y -40..-30).
/// The crowd arrives in the west, level with the north gap; the ministry
/// (a protected site, and the crowd's objective) is in the east, level
/// with it too. One line squad stands in the east: enough to close one gap.
fn two_gaps(people: u32, seed: u64) -> (MapDef, RiotSetup) {
    let map = MapDef {
        name: "two_gaps".into(),
        width: 256.0,
        height: 256.0,
        solids: vec![
            rect(-10.0, -128.0, 10.0, -40.0),
            rect(-10.0, -30.0, 10.0, 30.0),
            rect(-10.0, 40.0, 10.0, 128.0),
        ],
        entries: vec![
            EntryDef {
                name: "west".into(),
                kind: EntryKind::MapEdge,
                pos: [-100.0, 35.0],
                radius: 8.0,
            },
            EntryDef {
                name: "far_east".into(),
                kind: EntryKind::MapEdge,
                pos: [120.0, -100.0],
                radius: 4.0,
            },
        ],
        staging: vec![NamedPoint {
            name: "stage".into(),
            pos: [40.0, 0.0],
        }],
        depots: vec![NamedPoint {
            name: "depot".into(),
            pos: [100.0, 0.0],
        }],
        areas: vec![AreaDef {
            name: "ministry".into(),
            tag: "government".into(),
            pos: [60.0, 35.0],
            radius: 12.0,
        }],
        ..Default::default()
    };
    let mut crowd = [0u32; ARCHETYPES];
    crowd[Archetype::Worker as usize] = people;
    let mut setup = RiotSetup {
        seed,
        map: map.name.clone(),
        crowd,
        arrivals: vec![Arrival::new("west", 0.0, 1.0, 1.0)],
        objectives: vec![ObjectiveDef {
            name: "ministry".into(),
            kind: ObjectiveKind::MarchTo,
            area: Some("ministry".into()),
            pos: [0.0, 0.0],
            radius: 0.0,
            weight: 1.0,
            announced: true,
            stance: Stance::Peaceful,
        }],
        duration: minutes(10.0),
        ..Default::default()
    };
    setup.police.squads.push(SquadDef {
        kind: SquadKind::Line,
        officers: 10,
        depot: "depot".into(),
        staging: Some("stage".into()),
        available_min: 0.0,
    });
    (map, setup)
}

/// The police read the crowd's route to the ministry, barricade the gap it
/// is heading for before it gets there, and the crowd, finding the street
/// closed, goes round through the other gap (DESIGN §14).
#[test]
fn a_crowd_goes_round_a_police_barricade() {
    let (map, setup) = two_gaps(300, 3);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    let mut south = BTreeSet::new();
    let mut north_after = BTreeSet::new();
    let mut up_at = None;
    let mut line_x = 0.0;
    for _ in 0..4800 {
        w.tick(&[]);
        let cordon = w.police.cmd.cordons.iter().find(|c| {
            c.state == CordonState::Up && c.pos.x.abs() < 12.0 && (c.pos.y - 35.0).abs() < 6.0
        });
        if up_at.is_none()
            && let Some(c) = cordon
        {
            up_at = Some(w.tick);
            line_x = c.pos.x;
        }
        if !w.tick.is_multiple_of(10) {
            continue;
        }
        for i in 0..w.people.len {
            if w.people.is_dead(i) || Archetype::from_u8(w.people.stat[i][stat::ARCH]).is_officer()
            {
                continue;
            }
            let p = w.people.pos(i);
            if p.x.abs() < 10.0 && (-40.0..-30.0).contains(&p.y) {
                south.insert(w.people.stable_id[i]);
            }
            // Through the north gap once it is closed: past the barriers.
            if up_at.is_some() && p.x > line_x + 0.8 && p.x < 10.0 && (30.0..40.0).contains(&p.y) {
                north_after.insert(w.people.stable_id[i]);
            }
        }
    }
    let notables: Vec<_> = w.outcome.notable.iter().map(|n| (n.tick, n.kind)).collect();
    eprintln!(
        "barricade up at {up_at:?}; {} people went round by the south gap, {} came through \
         the north one after it closed; cordons {:?}; notables {notables:?}",
        south.len(),
        north_after.len(),
        w.police
            .cmd
            .cordons
            .iter()
            .map(|c| (c.state, c.pos))
            .collect::<Vec<_>>()
    );
    assert!(up_at.is_some(), "the north gap was never barricaded");
    assert!(
        w.props
            .iter()
            .any(|p| p.kind == PropKind::PoliceBarrier && p.pos.x.abs() < 12.0),
        "no police barrier in the gap"
    );
    assert!(
        south.len() >= 60,
        "only {} people found the way round",
        south.len()
    );
    assert!(
        north_after.len() * 4 < south.len(),
        "the barricade did not hold: {} through it",
        north_after.len()
    );
}

/// A street of radicals marching on the ministry: rows of buildings either
/// side, a side street off to the north at x -45..-35 into a back lane that
/// goes a long way round (about 200 m further) before it reaches the
/// ministry's square in the east. A line squad and a riot squad wait in the
/// square, and batons are allowed.
fn ambush_street(seed: u64) -> (MapDef, RiotSetup) {
    let map = MapDef {
        name: "ambush_street".into(),
        width: 256.0,
        height: 256.0,
        solids: vec![
            // North of the street, either side of the side street.
            rect(-128.0, 41.0, -45.0, 80.0),
            rect(-35.0, 41.0, 10.0, 80.0),
            // Between the back lane and the square.
            rect(10.0, 41.0, 118.0, 80.0),
            // North of the back lane.
            rect(-128.0, 90.0, 128.0, 128.0),
            // South of the street.
            rect(-128.0, -40.0, 10.0, 29.0),
        ],
        entries: vec![
            EntryDef {
                name: "west".into(),
                kind: EntryKind::MapEdge,
                pos: [-115.0, 35.0],
                radius: 5.0,
            },
            EntryDef {
                name: "south".into(),
                kind: EntryKind::MapEdge,
                pos: [60.0, -110.0],
                radius: 4.0,
            },
        ],
        staging: vec![NamedPoint {
            name: "stage".into(),
            pos: [40.0, 20.0],
        }],
        depots: vec![NamedPoint {
            name: "depot".into(),
            pos: [100.0, 0.0],
        }],
        areas: vec![AreaDef {
            name: "ministry".into(),
            tag: "government".into(),
            pos: [70.0, 30.0],
            radius: 12.0,
        }],
        ..Default::default()
    };
    let mut crowd = [0u32; ARCHETYPES];
    crowd[Archetype::Radical as usize] = 350;
    let mut setup = RiotSetup {
        seed,
        map: map.name.clone(),
        crowd,
        arrivals: vec![Arrival::new("west", 0.0, 1.5, 1.0)],
        objectives: vec![ObjectiveDef {
            name: "ministry".into(),
            kind: ObjectiveKind::MarchTo,
            area: Some("ministry".into()),
            pos: [0.0, 0.0],
            radius: 0.0,
            weight: 1.0,
            announced: true,
            stance: Stance::Confrontational,
        }],
        escalation_cap: Escalation::BatonsAndArrests,
        duration: minutes(10.0),
        ..Default::default()
    };
    setup.police.aggression = 1.0;
    for kind in [SquadKind::Line, SquadKind::Riot] {
        setup.police.squads.push(SquadDef {
            kind,
            officers: 10,
            depot: "depot".into(),
            staging: Some("stage".into()),
            available_min: 0.0,
        });
    }
    (map, setup)
}

/// The police barricade the street short of the ministry and hide a squad
/// in the side street, where the radicals cannot see it; once they are at
/// the barricade or come round its corner, it charges out into them
/// (DESIGN §14.1).
#[test]
fn an_ambush_waits_unseen_then_charges() {
    let (map, setup) = ambush_street(5);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    let batons = Command::at(0, CommandKind::Debug(DebugCommand::Escalate { rung: 2 }));
    w.tick(&[batons]);
    let mut set = None;
    let mut sprung = None;
    let mut hidden = true;
    for _ in 0..6000 {
        w.tick(&[]);
        let amb = w
            .police
            .squads
            .iter()
            .position(|s| s.doctrine == riot_sim::police::Doctrine::Ambush);
        if set.is_none() && amb.is_some() {
            set = Some(w.tick);
        }
        // Waiting officers show the crowd nothing.
        if let Some(k) = amb {
            let sq = w.police.squads[k];
            let close = (sq.pos - sq.anchor).len() < 4.0;
            if close
                && w.fields
                    .sample(riot_sim::fields::Layer::Threat, sq.pos.x, sq.pos.y)
                    > 1.0
            {
                hidden = false;
            }
        }
        if let Some(n) = w
            .outcome
            .notable
            .iter()
            .find(|n| n.kind == NotableKind::AmbushSprung)
        {
            sprung = Some(n.tick);
            break;
        }
    }
    let c = w.police.cmd.cordons.first().copied();
    eprintln!(
        "ambush set at {set:?}, sprung at {sprung:?}; cordon {:?}; arrests {}; squads {:?}",
        c.map(|c| (c.state, c.pos, c.ambush)),
        w.outcome.arrests,
        w.police
            .squads
            .iter()
            .map(|s| (s.doctrine, s.pos))
            .collect::<Vec<_>>()
    );
    let c = c.expect("no cordon");
    let a = c.ambush.expect("no ambush planned");
    assert!(
        a.pos.y > 41.0 && (-45.0..-35.0).contains(&a.pos.x),
        "the ambush should wait in the side street, not at {:?}",
        a.pos
    );
    assert!(hidden, "officers in ambush showed up in the threat field");
    assert!(set.is_some(), "no squad went into ambush");
    assert!(sprung.is_some(), "the ambush never sprang");
    // The charge runs at the crowd, then holds a line.
    let k = w
        .police
        .squads
        .iter()
        .position(|s| s.doctrine == riot_sim::police::Doctrine::Charge)
        .expect("nobody charged");
    let from = w.police.squads[k].pos;
    for _ in 0..200 {
        w.tick(&[]);
    }
    let to = w.police.squads[k].pos;
    eprintln!(
        "charge from {from:?} to {to:?}; {} arrests, {} injured",
        w.outcome.arrests, w.outcome.injuries
    );
    assert!((to - from).len() > 8.0, "the charge went nowhere");
}
