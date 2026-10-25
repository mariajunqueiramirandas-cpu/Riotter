//! A violent scenario shared by the invariant tests: a crowd converging on
//! the square of a small district, police on the ladder up to gas, and a
//! scripted canister and fire so hazards, arrests and injuries all run.

use riot_sim::testkit;
use riot_sim::{Archetype, Command, CommandKind, DebugCommand, MapDef, RiotSetup, World};

pub fn violent(people: u32) -> (MapDef, RiotSetup) {
    let map = testkit::district(512.0, 64.0);
    let setup = testkit::central(&map, people, 11);
    (map, setup)
}

/// The road node nearest `(x, y)`: gas is blocked by walls, so canisters
/// land in the street.
fn street(map: &MapDef, x: f32, y: f32) -> (f32, f32) {
    map.roads
        .nodes
        .iter()
        .map(|n| (n[0], n[1]))
        .min_by(|a, b| {
            let d = |p: &(f32, f32)| (p.0 - x).powi(2) + (p.1 - y).powi(2);
            d(a).total_cmp(&d(b))
        })
        .unwrap_or((x, y))
}

/// Bring radicals up to the police line at `at`, escalate, gas the
/// square's edge and set a car alight.
pub fn script(map: &MapDef, at: u32) -> Vec<Command> {
    let (gx, gy) = street(map, 30.0, -20.0);
    let car = map
        .props
        .first()
        .map_or((25.0, 20.0), |p| (p.pos[0], p.pos[1]));
    let dbg = |t, d| Command::at(t, CommandKind::Debug(d));
    let radicals = DebugCommand::Spawn {
        x: 0.0,
        y: -45.0,
        count: 80,
        archetype: Archetype::Radical,
        radius: 8.0,
    };
    vec![
        dbg(at, radicals),
        dbg(at, DebugCommand::Escalate { rung: 3 }),
        dbg(at, DebugCommand::Gas { x: gx, y: gy }),
        dbg(at + 40, DebugCommand::Ignite { x: car.0, y: car.1 }),
        dbg(
            at + 80,
            DebugCommand::Threat {
                x: 0.0,
                y: 0.0,
                amount: 2.0,
            },
        ),
    ]
}

/// One tick, passing the scripted commands stamped for it.
pub fn step(w: &mut World, script: &[Command]) {
    let t = w.tick;
    let start = script.partition_point(|c| c.tick < t);
    let end = script.partition_point(|c| c.tick <= t);
    w.tick(&script[start..end]);
}
