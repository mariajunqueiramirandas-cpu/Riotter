//! The committed data files stay loadable and in step with the code: every
//! scenario in `data/scenarios/` builds a world, `data/tunables.ron` lists
//! every tunable with its default, and the annotated example in
//! `data/scenarios/README.md` parses.

use std::path::{Path, PathBuf};
use std::time::Instant;

use riot_data::scenario::parse_scenario;
use riot_data::{load_map, load_scenario, load_tunables, parse_tunables};
use riot_sim::{TUNABLES, Tunables, World};

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

fn scenarios() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(data().join("scenarios"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ron"))
        .collect();
    v.sort();
    v
}

/// Loads a scenario, builds its world and runs `ticks` ticks.
fn run(path: &Path, ticks: u32) {
    let t = Instant::now();
    let s = load_scenario(path).unwrap_or_else(|e| panic!("{e}"));
    let loaded = t.elapsed();
    let t = Instant::now();
    let mut w = World::new(&s.setup, &s.map, s.tunables)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let built = t.elapsed();
    for _ in 0..ticks {
        w.tick(&[]);
    }
    let people: u32 = s.setup.crowd.iter().sum();
    eprintln!(
        "{}: {people} people on {}, loaded in {loaded:.2?}, world in {built:.2?}, {ticks} ticks",
        path.file_name().unwrap().to_string_lossy(),
        s.map_path.display(),
    );
    assert!(s.tunables_path.is_some());
    assert!(w.tick == ticks);
}

/// Scenarios on the 8 km city take seconds to build in a debug build.
fn on_big_map(path: &Path) -> bool {
    std::fs::read_to_string(path).unwrap().contains("city.json")
}

#[test]
fn small_scenarios_load_and_run() {
    let paths: Vec<PathBuf> = scenarios().into_iter().filter(|p| !on_big_map(p)).collect();
    assert!(paths.len() >= 2);
    for p in paths {
        run(&p, 20);
    }
}

#[test]
fn city_scenarios_load_and_build() {
    let paths: Vec<PathBuf> = scenarios().into_iter().filter(|p| on_big_map(p)).collect();
    assert!(paths.len() >= 2);
    for p in paths {
        run(&p, 2);
    }
}

#[test]
fn scenarios_say_what_they_promise() {
    let s = load_scenario(&data().join("scenarios/standard_riot.ron")).unwrap();
    assert_eq!(s.setup.crowd.iter().sum::<u32>(), 10_000);
    assert_eq!(s.setup.police.squads.len(), 30);
    assert_eq!(s.setup.duration, riot_sim::setup::minutes(10.0));
    assert_eq!(
        s.setup.escalation_cap,
        riot_sim::setup::Escalation::GasAndWater
    );
    let s = load_scenario(&data().join("scenarios/small_protest.ron")).unwrap();
    assert_eq!(s.setup.crowd.iter().sum::<u32>(), 2_000);
    assert_eq!(
        s.setup.escalation_cap,
        riot_sim::setup::Escalation::BatonsAndArrests
    );
    let text = std::fs::read_to_string(data().join("scenarios/city_day.ron")).unwrap();
    let file = riot_data::scenario::ScenarioFile::from_ron(&text).unwrap();
    let city = load_map(&data().join("maps/city.json")).unwrap();
    let day = file.into_setup(&city).unwrap();
    assert!(day.duration >= riot_sim::setup::minutes(4.0 * 60.0));
    assert_eq!(day.comms.shutdown_min, Some(12.0 * 60.0));
    let metro = |e: &str| e.starts_with("metro_");
    assert!(day.arrivals.iter().any(|a| metro(&a.entry)));
    let radical = riot_sim::Archetype::Radical as usize;
    let late = day
        .arrivals
        .iter()
        .find(|a| a.from_min >= 11.0 * 60.0)
        .unwrap();
    assert!(late.bias[radical] > 1.0);
    let s = load_scenario(&data().join("scenarios/mass_demonstration.ron")).unwrap();
    assert_eq!(s.setup.crowd.iter().sum::<u32>(), 50_000);
    assert_eq!(s.map.width, 8192.0);
}

#[test]
fn tunables_file_lists_every_tunable_with_its_default() {
    let path = data().join("tunables.ron");
    let text = std::fs::read_to_string(&path).unwrap();
    let values = parse_tunables(&text).unwrap();
    let names: Vec<&str> = values.iter().map(|(n, _)| n.as_str()).collect();
    let expected: Vec<&str> = TUNABLES.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names, expected,
        "data/tunables.ron is out of date; regenerate it with \
         `cargo run -p riot_data --example gen_data`"
    );
    let defaults = Tunables::default();
    for (id, (name, v)) in values.iter().enumerate() {
        assert_eq!(Some(*v), defaults.get(id as u16), "default of {name}");
    }
    for (name, doc) in TUNABLES {
        assert!(
            text.contains(&format!("    // {doc}\n    \"{name}\":")),
            "{name} is missing its doc comment"
        );
    }
    assert_eq!(load_tunables(&path).unwrap(), defaults);
}

#[test]
fn readme_example_parses() {
    let readme = std::fs::read_to_string(data().join("scenarios/README.md")).unwrap();
    let start = readme.find("```ron\n").expect("a ```ron block") + 7;
    let len = readme[start..].find("```").unwrap();
    let s = parse_scenario(&readme[start..start + len], &data().join("scenarios"))
        .unwrap_or_else(|e| panic!("README example: {e}"));
    assert_eq!(s.setup.objectives.len(), 2);
    assert_eq!(s.setup.limits.field_chunks, 200);
    assert_eq!(s.tunables.hop_loss, 5.0);
    World::new(&s.setup, &s.map, s.tunables).unwrap();
}

#[test]
fn committed_maps_load() {
    let d = load_map(&data().join("maps/district.json")).unwrap();
    assert_eq!(d.name, "district");
    assert_eq!((d.width, d.height), (1024.0, 1024.0));
    // The committed district is what the generator makes for seed 1.
    let g = riot_data::citygen::generate_mapdef(&riot_data::citygen::CityParams::new(1024.0, 1));
    assert_eq!(
        (d.solids.len(), d.entries.len()),
        (g.solids.len(), g.entries.len())
    );
}
