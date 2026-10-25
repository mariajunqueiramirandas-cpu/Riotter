//! Where the headless runner gets its world from: a RON scenario through
//! `riot_data`, or a generated district.

use std::path::Path;

use riot_sim::testkit;
use riot_sim::{MapDef, RiotSetup, Tunables};

use crate::args::Args;

pub struct Scenario {
    pub setup: RiotSetup,
    pub map: MapDef,
    pub tunables: Tunables,
}

pub fn load(args: &Args) -> Result<Scenario, String> {
    let mut sc = if let Some(path) = args.str("scenario") {
        let s = riot_data::load_scenario(Path::new(&path)).map_err(|e| e.to_string())?;
        Scenario {
            setup: s.setup,
            map: s.map,
            tunables: s.tunables,
        }
    } else if let Some(n) = args.u32("crush") {
        let (map, setup) = testkit::crush(n, args.u64("seed").unwrap_or(1));
        Scenario {
            setup,
            map,
            tunables: Tunables::default(),
        }
    } else {
        let size = args.f32("size").unwrap_or(1024.0);
        let people = args.u32("people").unwrap_or(2000);
        let squads = args.u32("squads").unwrap_or(8);
        let seed = args.u64("seed").unwrap_or(1);
        let map = testkit::district(size, 64.0);
        let setup = if args.flag("central") {
            testkit::central(&map, people, seed)
        } else {
            testkit::march(&map, people, squads, seed)
        };
        Scenario {
            setup,
            map,
            tunables: Tunables::default(),
        }
    };
    if let Some(seed) = args.u64("seed") {
        sc.setup.seed = seed;
    }
    if let Some(cap) = args.u32("cap") {
        sc.setup.escalation_cap = riot_sim::setup::Escalation::from_u8(cap as u8);
    }
    if let Some(t) = args.str("tunables") {
        sc.tunables = riot_data::load_tunables(Path::new(&t)).map_err(|e| e.to_string())?;
    }
    Ok(sc)
}

/// Write a generated city as Tiled JSON (`--out PATH --size M --seed X`).
pub fn gen_map(args: &Args) -> Result<(), String> {
    let out = args.str("out").unwrap_or("city.json".into());
    let params = riot_data::citygen::CityParams {
        size_m: args.f32("size").unwrap_or(8192.0),
        seed: args.u64("seed").unwrap_or(3),
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let json = riot_data::citygen::generate_json(&params);
    std::fs::write(&out, &json).map_err(|e| format!("{out}: {e}"))?;
    println!(
        "wrote {out} ({:.1} MB) in {:.2?}",
        json.len() as f64 / 1e6,
        t0.elapsed()
    );
    Ok(())
}
