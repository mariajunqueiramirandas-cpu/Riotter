//! RON scenarios: a [`RiotSetup`] plus the map and tunables it runs with
//! (DESIGN §17). The format is documented with an annotated example in
//! `data/scenarios/README.md`; the file model ([`ScenarioFile`]) and its
//! resolution against a map are `riot_data_scenario`, re-exported here.
//!
//! The map is a Tiled JSON path relative to the scenario file, or a
//! generator pseudo-path: `generated:city?size=8192&seed=3`,
//! `generated:district?seed=1` (a 1 km city) or
//! `generated:testkit?size=1024&block=64` (`riot_sim::testkit::district`).

use std::path::{Path, PathBuf};

use riot_sim::mapdef::MapDef;
use riot_sim::{RiotSetup, Tunables};

use riot_data_core::{DataError, Result};
pub use riot_data_scenario::*;

use crate::citygen::{self, CityParams};
use crate::tunables::{apply, load_tunables};

/// Everything needed to start a riot.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub setup: RiotSetup,
    pub map: MapDef,
    pub tunables: Tunables,
    /// The map file, or the `generated:` pseudo-path.
    pub map_path: PathBuf,
    /// The tunables file the scenario names, if any: the file to watch for
    /// hot reload.
    pub tunables_path: Option<PathBuf>,
}

/// Loads a scenario file, its map and its tunables.
pub fn load_scenario(path: &Path) -> Result<Scenario> {
    let text = std::fs::read_to_string(path).map_err(|e| DataError::io(path, e))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    parse_scenario(&text, dir).map_err(|e| e.in_file(path))
}

/// Parses a scenario from text; `dir` resolves the map and tunables paths.
pub fn parse_scenario(text: &str, dir: &Path) -> Result<Scenario> {
    let file = ScenarioFile::from_ron(text)?;
    let (map, map_path) = load_map_spec(&file.map, dir).map_err(|e| e.at("map"))?;
    let tunables_path = file.tunables.as_ref().map(|t| dir.join(t));
    let mut tunables = match &tunables_path {
        Some(p) => load_tunables(p)?,
        None => Tunables::default(),
    };
    let mut tune = Vec::new();
    for (name, &v) in &file.tune {
        if Tunables::id_of(name).is_none() {
            return Err(DataError::new(format!("unknown tunable `{name}`")).at("tune"));
        }
        tune.push((name.clone(), v));
    }
    apply(&mut tunables, &tune);
    let setup = file.into_setup(&map)?;
    Ok(Scenario {
        setup,
        map,
        tunables,
        map_path,
        tunables_path,
    })
}

/// Resolves a scenario's `map`: a `generated:` pseudo-path, or a Tiled JSON
/// file relative to `dir`. Returns the map and where it came from.
pub fn load_map_spec(spec: &str, dir: &Path) -> Result<(MapDef, PathBuf)> {
    if let Some(rest) = spec.strip_prefix("generated:") {
        return Ok((generate_spec(spec, rest)?, PathBuf::from(spec)));
    }
    let path = dir.join(spec);
    match path.extension().and_then(|e| e.to_str()) {
        Some("json" | "tmj") => Ok((crate::tiled::load_map(&path)?, path)),
        _ => Err(DataError::new(format!(
            "`{spec}`: maps are Tiled JSON files (.json, .tmj) or `generated:city?size=…&seed=…`"
        ))),
    }
}

/// `generated:<kind>?k=v&…` → a map named after the full pseudo-path.
fn generate_spec(spec: &str, rest: &str) -> Result<MapDef> {
    let (kind, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut params: Vec<(&str, &str)> = Vec::new();
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| DataError::new(format!("`{spec}`: expected key=value, found `{kv}`")))?;
        params.push((k, v));
    }
    let num = |k: &str, v: &str| -> Result<f64> {
        v.parse::<f64>()
            .ok()
            .filter(|x| x.is_finite())
            .ok_or_else(|| DataError::new(format!("`{spec}`: `{k}` must be a number, not `{v}`")))
    };
    let mut map = match kind {
        "city" | "district" => {
            let mut p = CityParams::new(if kind == "city" { 8192.0 } else { 1024.0 }, 1);
            for &(k, v) in &params {
                match k {
                    "size" => p.size_m = num(k, v)? as f32,
                    "seed" => {
                        p.seed = v.parse().map_err(|_| {
                            DataError::new(format!("`{spec}`: seed must be an integer, not `{v}`"))
                        })?
                    }
                    "block" => p.block_m = num(k, v)? as f32,
                    "avenue_every" => p.avenue_every = num(k, v)?.max(1.0) as u32,
                    "river" => p.river = matches!(v, "1" | "true" | "yes"),
                    "props" => p.max_props = num(k, v)?.max(0.0) as u32,
                    _ => {
                        return Err(DataError::new(format!(
                            "`{spec}`: unknown parameter `{k}` \
                             (expected size, seed, block, avenue_every, river or props)"
                        )));
                    }
                }
            }
            citygen::generate_mapdef(&p)
        }
        "testkit" => {
            let (mut size, mut block) = (1024.0f32, 64.0f32);
            for &(k, v) in &params {
                match k {
                    "size" => size = num(k, v)? as f32,
                    "block" => block = num(k, v)? as f32,
                    _ => {
                        return Err(DataError::new(format!(
                            "`{spec}`: unknown parameter `{k}` (expected size or block)"
                        )));
                    }
                }
            }
            riot_sim::testkit::district(size.clamp(128.0, 8192.0), block.max(16.0))
        }
        _ => {
            return Err(DataError::new(format!(
                "`{spec}`: unknown generator `{kind}` (expected city, district or testkit)"
            )));
        }
    };
    map.name = spec.to_string();
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riot_sim::Archetype;
    use riot_sim::setup::{Escalation, Limits, ObjectiveKind, SquadKind, minutes};

    const SMALL: &str = r#"
        // A comment.
        (
            map: "generated:testkit?size=512&block=64",
            seed: 9,
            start: "14:30",
            duration: 20,
            crowd: { "student": 300, "radical": 50 },
            organizers: 3,
            arrivals: [
                (entry: "w*", from: 0, to: 5),
                (entry: "metro_centre", from: "15:00", to: "15:30", share: 2, bias: { "radical": 3.0 }),
            ],
            police: (
                vans_per_depot: 1,
                squads: [
                    (kind: Line, depot: "depot_*", staging: "staging_north", count: 3),
                    (kind: snatch, officers: 6, depot: "depot_east", available: "15:00"),
                ],
            ),
            escalation_cap: GasAndWater,
            comms: (mode: Phones, shutdown_at: "16:00"),
            weather: (wind: (2.0, -1.0), dusk: "19:45"),
            objectives: [(name: "sq", kind: Hold, area: "square", announced: true, stance: Defensive)],
            limits: (field_chunks: 64),
            tune: { "hop_loss": 5 },
        )
    "#;

    #[test]
    fn parses_and_resolves() {
        let s = parse_scenario(SMALL, Path::new(".")).unwrap();
        let st = &s.setup;
        assert_eq!(st.seed, 9);
        assert_eq!(st.map, "generated:testkit?size=512&block=64");
        assert_eq!(st.start_minute, 14.0 * 60.0 + 30.0);
        assert_eq!(st.duration, minutes(20.0));
        assert_eq!(st.crowd[Archetype::Student as usize], 300);
        assert_eq!(st.crowd[Archetype::Radical as usize], 50);
        // testkit maps have 4 west entries.
        let w: Vec<_> = st
            .arrivals
            .iter()
            .filter(|a| a.entry.starts_with('w'))
            .collect();
        assert_eq!(w.len(), 4);
        assert!(w.iter().all(|a| a.share == 0.25));
        let m = st.arrivals.last().unwrap();
        assert_eq!((m.from_min, m.to_min, m.share), (30.0, 60.0, 2.0));
        assert_eq!(m.bias[Archetype::Radical as usize], 3.0);
        assert_eq!(m.bias[Archetype::Student as usize], 1.0);
        let sq = &st.police.squads;
        assert_eq!(sq.len(), 4);
        assert_eq!(
            sq.iter().map(|s| s.depot.as_str()).collect::<Vec<_>>(),
            ["depot_west", "depot_east", "depot_west", "depot_east"]
        );
        assert_eq!(sq[3].kind, SquadKind::Snatch);
        assert_eq!(sq[3].available_min, 30.0);
        assert_eq!(st.escalation_cap, Escalation::GasAndWater);
        assert_eq!(st.comms.shutdown_min, Some(90.0));
        assert_eq!(st.weather.wind, [2.0, -1.0]);
        assert_eq!(st.weather.dusk_minute, 19.0 * 60.0 + 45.0);
        assert_eq!(st.objectives[0].kind, ObjectiveKind::Hold);
        assert_eq!(st.limits.field_chunks, 64);
        assert_eq!(st.limits.props, Limits::default().props);
        assert_eq!(s.tunables.hop_loss, 5.0);
        let mut w = riot_sim::World::new(st, &s.map, s.tunables).unwrap();
        for _ in 0..20 {
            w.tick(&[]);
        }
    }

    #[test]
    fn errors_name_the_field() {
        let bad = |from: &str, to: &str| {
            let text = SMALL.replace(from, to);
            parse_scenario(&text, Path::new("."))
                .unwrap_err()
                .to_string()
        };
        let e = bad("\"student\"", "\"studnet\"");
        assert!(e.contains("crowd: unknown archetype `studnet`"), "{e}");
        let e = bad("depot: \"depot_east\"", "depot: \"depot_x\"");
        assert!(
            e.contains("police.squads[1]: no depot matches `depot_x`"),
            "{e}"
        );
        let e = bad("\"w*\"", "\"q*\"");
        assert!(e.contains("arrivals[0]: no entry matches `q*`"), "{e}");
        let e = bad("area: \"square\"", "area: \"plaza\"");
        assert!(e.contains("objectives[0]: unknown area `plaza`"), "{e}");
        let e = bad("duration: 20", "duraton: 20");
        assert!(e.contains("field named `duraton`"), "{e}");
        assert!(e.contains("line "), "{e}");
        let e = bad("start: \"14:30\"", "start: \"14:75\"");
        assert!(e.contains("not a time"), "{e}");
        let e = bad("\"student\": 300", "\"organizer\": 300");
        assert!(e.contains("set `organizers: N`"), "{e}");
        let e = bad("\"student\": 300", "\"student\": 70000");
        assert!(e.contains("more than the 65000"), "{e}");
        let e = bad("\"hop_loss\"", "\"hop_los\"");
        assert!(e.contains("tune: unknown tunable `hop_los`"), "{e}");
        let e = bad("generated:testkit?size=512&block=64", "generated:town");
        assert!(
            e.contains("map: `generated:town`: unknown generator"),
            "{e}"
        );
    }
}
