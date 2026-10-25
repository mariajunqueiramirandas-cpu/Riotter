//! Regenerates the data files that come from code: `data/maps/district.json`
//! (a 1 km city), `data/maps/city.json` (the 8 km city), the ground tileset
//! image `data/maps/ground.png` and `data/tunables.ron`.
//!
//! ```text
//! cargo run --release -p riot_data --example gen_data [-- maps|tunables] [--out DIR] [--nav]
//! ```
//!
//! `maps` or `tunables` writes only those (default: both); `tunables` is the
//! quick fix when `riot_sim` gains or changes a tunable. `--out DIR` writes
//! somewhere other than the repository's `data/`; `--nav` also times deriving
//! the nav tiles (`riot_sim::map::Map::build`).

use std::path::PathBuf;
use std::time::Instant;

use riot_data::citygen::{self, CityParams};

/// The committed district: `generated:district?seed=1` gives the same map.
pub fn district() -> CityParams {
    CityParams::new(1024.0, 1)
}

/// The committed city: `generated:city?size=8192&seed=3` gives the same map.
pub fn city() -> CityParams {
    CityParams::new(8192.0, 3)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data"));
    let nav = args.iter().any(|a| a == "--nav");
    let only = |what: &str| {
        let picked = args.iter().any(|a| a == "maps" || a == "tunables");
        !picked || args.iter().any(|a| a == what)
    };
    if only("maps") {
        write_maps(&out, nav);
    }
    if only("tunables") {
        let tunables = out.join("tunables.ron");
        std::fs::write(&tunables, riot_data::tunables::default_tunables_ron())
            .expect("write tunables");
        println!("{}", tunables.display());
    }
}

fn write_maps(out: &std::path::Path, nav: bool) {
    let maps = out.join("maps");
    std::fs::create_dir_all(&maps).expect("create the maps directory");
    std::fs::write(maps.join("ground.png"), citygen::ground_png()).expect("write ground.png");
    for (file, params) in [("district.json", district()), ("city.json", city())] {
        let t = Instant::now();
        let json = citygen::generate_json(&params);
        let generated = t.elapsed();
        let path = maps.join(file);
        std::fs::write(&path, &json).expect("write the map");
        let t = Instant::now();
        let map = riot_data::load_map(&path).unwrap_or_else(|e| panic!("{e}"));
        let loaded = t.elapsed();
        println!(
            "{}: {:.2} MB, generated in {generated:.2?}, loaded in {loaded:.2?}\n  \
             {} solids, {} overhead, {} props, roads {} nodes / {} edges, \
             {} entries, {} depots, {} staging, {} areas",
            path.display(),
            json.len() as f64 / 1e6,
            map.solids.len(),
            map.overhead.len(),
            map.props.len(),
            map.roads.nodes.len(),
            map.roads.edges.len(),
            map.entries.len(),
            map.depots.len(),
            map.staging.len(),
            map.areas.len(),
        );
        if nav {
            let t = Instant::now();
            let m = riot_sim::map::Map::build(&map, 0.47).expect("nav tiles");
            println!(
                "  nav tiles in {:.2?}: {} mixed tiles, {} region nodes, {:.1} MB",
                t.elapsed(),
                m.mixed.len(),
                m.rnodes.len(),
                m.memory_bytes() as f64 / 1e6
            );
        }
    }
}
