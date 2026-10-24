//! Where the headless runner gets its world from: a RON scenario through
//! `riot_data`, or a generated district.

use riot_sim::testkit;
use riot_sim::{MapDef, RiotSetup, Tunables};

use crate::args::Args;

pub struct Scenario {
    pub setup: RiotSetup,
    pub map: MapDef,
    pub tunables: Tunables,
}

pub fn load(args: &Args) -> Result<Scenario, String> {
    if let Some(path) = args.str("scenario") {
        return Err(format!(
            "loading `{path}` needs riot_data, which is not wired up yet"
        ));
    }
    if let Some(n) = args.u32("crush") {
        let (map, setup) = testkit::crush(n, args.u64("seed").unwrap_or(1));
        return Ok(Scenario {
            setup,
            map,
            tunables: Tunables::default(),
        });
    }
    let size = args.f32("size").unwrap_or(1024.0);
    let people = args.u32("people").unwrap_or(2000);
    let squads = args.u32("squads").unwrap_or(8);
    let seed = args.u64("seed").unwrap_or(1);
    let map = testkit::district(size, 64.0);
    let mut setup = if args.flag("central") {
        testkit::central(&map, people, seed)
    } else {
        testkit::march(&map, people, squads, seed)
    };
    if let Some(cap) = args.u32("cap") {
        setup.escalation_cap = riot_sim::setup::Escalation::from_u8(cap as u8);
    }
    Ok(Scenario {
        setup,
        map,
        tunables: Tunables::default(),
    })
}

pub fn gen_map(_args: &Args) -> Result<(), String> {
    Err("gen-map needs riot_data, which is not wired up yet".into())
}
