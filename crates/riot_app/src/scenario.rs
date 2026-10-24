//! Scenario loading: the map, the setup and the starting tunables.

use std::path::PathBuf;

use riot_sim::{MapDef, RiotSetup, Tunables, testkit};

use crate::args::Args;
use crate::hotreload;

/// What to run, and the tunables file to hot-reload (if any).
pub struct Loaded {
    pub map: MapDef,
    pub setup: RiotSetup,
    pub tunables: Tunables,
    pub watch: Option<PathBuf>,
}

/// Build the riot to run from the command line: `--scenario path.ron`
/// through `riot_data`, or else the procedural district from
/// `riot_sim::testkit` (`--size`, `--people`, `--squads`, `--seed`).
/// `--tunables` overrides the scenario's tunables and is the file watched.
pub fn load_world(args: &Args) -> Result<Loaded, String> {
    let (map, setup, mut tunables, mut watch) = match &args.scenario {
        Some(path) => {
            let s = riot_data::load_scenario(path).map_err(|e| e.to_string())?;
            (s.map, s.setup, s.tunables, s.tunables_path)
        }
        None => {
            let map = testkit::district(args.size, 64.0);
            let setup = testkit::march(&map, args.people, args.squads, args.seed);
            (map, setup, Tunables::default(), None)
        }
    };
    if let Some(path) = &args.tunables {
        let values =
            hotreload::read_tunables(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for name in hotreload::apply(&values, &mut tunables) {
            eprintln!("{}: unknown tunable `{name}`", path.display());
        }
        watch = Some(path.clone());
    }
    Ok(Loaded {
        map,
        setup,
        tunables,
        watch,
    })
}
