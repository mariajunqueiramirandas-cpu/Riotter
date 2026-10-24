//! Scenario loading: the map, the setup and the starting tunables.

use riot_sim::{MapDef, RiotSetup, Tunables, testkit};

use crate::args::Args;
use crate::hotreload;

/// Build the riot to run from the command line.
///
/// For now this is always the procedural district from `riot_sim::testkit`
/// (`--size`, `--people`, `--squads`, `--seed`). The starting tunables come
/// from `--tunables` when given (the same file is then hot-reloaded).
pub fn load_world(args: &Args) -> (MapDef, RiotSetup, Tunables) {
    // TODO(riot_data): when `args.scenario` is set, load it instead:
    //     let (map, setup) = riot_data::load_scenario(path)?;
    // and replace `hotreload::read_tunables` with riot_data's RON parser.
    if let Some(path) = &args.scenario {
        eprintln!(
            "--scenario {}: scenario files are not supported yet; using the procedural district",
            path.display()
        );
    }
    let map = testkit::district(args.size, 64.0);
    let setup = testkit::march(&map, args.people, args.squads, args.seed);
    let mut tun = Tunables::default();
    if let Some(path) = &args.tunables {
        match hotreload::read_tunables(path) {
            Ok(values) => {
                for name in hotreload::apply(&values, &mut tun) {
                    eprintln!("{}: unknown tunable `{name}`", path.display());
                }
            }
            Err(e) => eprintln!("{}: {e}", path.display()),
        }
    }
    (map, setup, tun)
}
