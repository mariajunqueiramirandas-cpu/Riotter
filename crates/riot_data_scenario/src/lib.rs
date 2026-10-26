//! RON scenario files ↔ [`RiotSetup`] (DESIGN §17). The format is documented
//! with an annotated example in `data/scenarios/README.md`.
//!
//! `riot_sim` types carry no serde derives, so [`ScenarioFile`] mirrors
//! `RiotSetup` in file-friendly units (minutes, clock times, archetype and
//! entry names) and [`ScenarioFile::into_setup`] resolves it against a map.
//! Loading the map and the tunables a scenario names is `riot_data`'s job
//! (`riot_data::load_scenario`), so this crate needs neither the Tiled reader
//! nor the city generator.

use std::collections::BTreeMap;
use std::fmt;

use riot_defs::mapdef::MapDef;
use riot_defs::setup::{
    Arrival, Comms, CommsMode, Escalation, Limits, ObjectiveDef, ObjectiveKind, PoliceForce,
    SquadDef, SquadKind, Supplies, Weather, minutes,
};
use riot_defs::{ARCHETYPES, Archetype, RiotSetup, Stance, TICK_HZ};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use riot_data_core::{DataError, Result};
use riot_data_tunables::ron_error;

/// The most people one riot can hold (`riot_sim::World::new`).
pub const MAX_PEOPLE: u32 = 65_000;

// ---------------------------------------------------------------------------
// The file format

/// A scenario file: a serde mirror of [`RiotSetup`] plus the map and
/// tunables to run it with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioFile {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Tiled JSON path relative to the scenario file, or `generated:…`.
    pub map: String,
    /// Tunables file relative to the scenario file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunables: Option<String>,
    /// Tunable overrides applied after the tunables file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tune: BTreeMap<String, f32>,
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Time of day at tick 0: `"14:00"`, or minutes after midnight.
    #[serde(default = "default_start")]
    pub start: Time,
    /// Length of the riot: minutes, or `"H:MM"`.
    pub duration: Time,
    /// Turnout per crowd archetype: `{"student": 3000, "worker": 2500}`.
    pub crowd: BTreeMap<String, u32>,
    #[serde(default)]
    pub organizers: u16,
    #[serde(default)]
    pub arrivals: Vec<ArrivalFile>,
    #[serde(default)]
    pub supplies: SuppliesFile,
    #[serde(default)]
    pub police: PoliceFile,
    #[serde(default = "default_cap")]
    pub escalation_cap: EscalationFile,
    #[serde(default)]
    pub comms: CommsFile,
    #[serde(default)]
    pub weather: WeatherFile,
    #[serde(default)]
    pub objectives: Vec<ObjectiveFile>,
    #[serde(default, skip_serializing_if = "LimitsFile::is_empty")]
    pub limits: LimitsFile,
}

fn default_seed() -> u64 {
    1
}
fn default_start() -> Time {
    Time::Clock(12.0 * 60.0)
}
fn default_cap() -> EscalationFile {
    EscalationFile::BatonsAndArrests
}
fn one() -> f32 {
    1.0
}
fn one_u16() -> u16 {
    1
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}
fn is_one_u16(v: &u16) -> bool {
    *v == 1
}

/// A time: a number of minutes, or a clock time `"HH:MM"`.
///
/// Times inside the riot (arrivals, squads, the internet shutdown) are
/// minutes after the start when written as numbers, and times of day when
/// written as `"HH:MM"` (past midnight wraps to the next day). `start` and
/// `weather.dusk` are always times of day; `duration` is a length either way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Time {
    Minutes(f32),
    /// Minutes after midnight, written `"HH:MM"`.
    Clock(f32),
}

impl Time {
    /// Minutes after the riot starts at `start` (minutes after midnight).
    pub fn after(self, start: f32) -> f32 {
        match self {
            Time::Minutes(m) => m,
            Time::Clock(c) => (c - start).rem_euclid(24.0 * 60.0),
        }
    }
    /// Minutes: a time of day, or a length.
    pub fn minutes(self) -> f32 {
        match self {
            Time::Minutes(m) | Time::Clock(m) => m,
        }
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Time::Minutes(m) => write!(f, "{m}"),
            Time::Clock(c) => write!(f, "{:02}:{:02}", (c / 60.0) as u32, (c % 60.0) as u32),
        }
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Time::Minutes(m) => s.serialize_f32(*m),
            Time::Clock(_) => s.serialize_str(&self.to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Time;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("minutes, or a time \"HH:MM\"")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Time, E> {
                if v.is_finite() && v >= 0.0 {
                    Ok(Time::Minutes(v as f32))
                } else {
                    Err(E::custom(format!("{v} minutes: must be zero or more")))
                }
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Time, E> {
                self.visit_f64(v as f64)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Time, E> {
                self.visit_f64(v as f64)
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Time, E> {
                let bad = || E::custom(format!("`{s}` is not a time \"HH:MM\""));
                let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
                let (h, m): (u32, u32) =
                    (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
                if m >= 60 {
                    return Err(bad());
                }
                Ok(Time::Clock((h * 60 + m) as f32))
            }
        }
        d.deserialize_any(V)
    }
}

/// People arriving through an entry over a time window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrivalFile {
    /// An entry name, or a prefix ending in `*` (`"metro_*"`, `"*"`): the
    /// share is split evenly over every matching entry.
    pub entry: String,
    pub from: Time,
    pub to: Time,
    /// Relative share of the total turnout.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub share: f32,
    /// Per-archetype multiplier on the share (missing archetypes: 1).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bias: BTreeMap<String, f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuppliesFile {
    #[serde(default)]
    pub water: u32,
    #[serde(default)]
    pub medic: u32,
    #[serde(default)]
    pub shields: u32,
    #[serde(default)]
    pub barricade: u32,
}

impl From<SuppliesFile> for Supplies {
    fn from(s: SuppliesFile) -> Self {
        Supplies {
            water: s.water,
            medic: s.medic,
            shields: s.shields,
            barricade: s.barricade,
        }
    }
}

impl From<Supplies> for SuppliesFile {
    fn from(s: Supplies) -> Self {
        SuppliesFile {
            water: s.water,
            medic: s.medic,
            shields: s.shields,
            barricade: s.barricade,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliceFile {
    #[serde(default)]
    pub squads: Vec<SquadFile>,
    /// Vans per depot for moving squads and arrestees.
    #[serde(default)]
    pub vans_per_depot: u8,
    /// Water cannon trucks, spread over the depots.
    #[serde(default)]
    pub water_cannons: u8,
    /// 0 = cautious, 1 = aggressive.
    #[serde(default)]
    pub aggression: f32,
}

/// One squad, or `count` alike.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquadFile {
    pub kind: SquadKindFile,
    #[serde(default = "ten")]
    pub officers: u8,
    /// A depot name, or a prefix ending in `*`: `count` squads are dealt
    /// round-robin over the matching depots.
    pub depot: String,
    /// A staging point to start at instead (already deployed); a prefix
    /// ending in `*` is dealt round-robin like `depot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<String>,
    /// When the squad becomes available (default: at once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<Time>,
    #[serde(default = "one_u16", skip_serializing_if = "is_one_u16")]
    pub count: u16,
}

fn ten() -> u8 {
    10
}

/// The comms mode, and when the state shuts the internet down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommsFile {
    #[serde(default)]
    pub mode: CommsModeFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_at: Option<Time>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeatherFile {
    /// m/s, (east, south).
    #[serde(default = "default_wind")]
    pub wind: [f32; 2],
    /// 0 = cool, 1 = hot.
    #[serde(default = "default_heat")]
    pub heat: f32,
    /// Time of day it gets dark.
    #[serde(default = "default_dusk")]
    pub dusk: Time,
}

fn default_wind() -> [f32; 2] {
    Weather::default().wind
}
fn default_heat() -> f32 {
    Weather::default().heat
}
fn default_dusk() -> Time {
    Time::Clock(Weather::default().dusk_minute)
}

impl Default for WeatherFile {
    fn default() -> Self {
        WeatherFile {
            wind: default_wind(),
            heat: default_heat(),
            dusk: default_dusk(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveFile {
    pub name: String,
    #[serde(default)]
    pub kind: ObjectiveKindFile,
    /// A named area of the map; or leave it out and give `pos` and `radius`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// Metres from the map centre (when there is no `area`).
    #[serde(default)]
    pub pos: [f32; 2],
    /// Metres; 0 with an `area` uses the area's radius.
    #[serde(default)]
    pub radius: f32,
    #[serde(default = "one")]
    pub weight: f32,
    /// Every arriving protester already knows it.
    #[serde(default)]
    pub announced: bool,
    #[serde(default)]
    pub stance: StanceFile,
}

/// Preallocation sizes; anything left out keeps `Limits::default()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_chunks: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_tiles: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_pages: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projectiles: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notable: Option<u32>,
}

impl LimitsFile {
    fn is_empty(&self) -> bool {
        *self == LimitsFile::default()
    }
}

impl From<LimitsFile> for Limits {
    fn from(l: LimitsFile) -> Self {
        let d = Limits::default();
        Limits {
            field_chunks: l.field_chunks.unwrap_or(d.field_chunks),
            flow_tiles: l.flow_tiles.unwrap_or(d.flow_tiles),
            flow_pages: l.flow_pages.unwrap_or(d.flow_pages),
            props: l.props.unwrap_or(d.props),
            projectiles: l.projectiles.unwrap_or(d.projectiles),
            notable: l.notable.unwrap_or(d.notable),
        }
    }
}

impl From<Limits> for LimitsFile {
    /// Only what differs from the defaults.
    fn from(l: Limits) -> Self {
        let d = Limits::default();
        let pick = |v: u32, dv: u32| (v != dv).then_some(v);
        LimitsFile {
            field_chunks: (l.field_chunks != d.field_chunks).then_some(l.field_chunks),
            flow_tiles: pick(l.flow_tiles, d.flow_tiles),
            flow_pages: pick(l.flow_pages, d.flow_pages),
            props: pick(l.props, d.props),
            projectiles: pick(l.projectiles, d.projectiles),
            notable: pick(l.notable, d.notable),
        }
    }
}

/// Mirrors a `riot_sim` enum as a RON enum written by variant name (and
/// accepts snake_case aliases), with `From` both ways.
macro_rules! mirror_enum {
    ($(#[$m:meta])* $file:ident => $sim:ident { $($v:ident = $alias:literal),* $(,)? } default $d:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $file {
            $(#[serde(alias = $alias)] $v,)*
        }
        impl Default for $file {
            fn default() -> Self {
                $file::$d
            }
        }
        impl From<$file> for $sim {
            fn from(v: $file) -> Self {
                match v { $($file::$v => $sim::$v,)* }
            }
        }
        impl From<$sim> for $file {
            fn from(v: $sim) -> Self {
                match v { $($sim::$v => $file::$v,)* }
            }
        }
    };
}

mirror_enum!(
    /// `Presence`, `LinesAndKettles`, `BatonsAndArrests`, `GasAndWater`, `MassArrests`.
    EscalationFile => Escalation {
        Presence = "presence",
        LinesAndKettles = "lines_and_kettles",
        BatonsAndArrests = "batons_and_arrests",
        GasAndWater = "gas_and_water",
        MassArrests = "mass_arrests",
    } default Presence
);
mirror_enum!(
    /// `Line` (lines and kettles), `Riot` (shields and batons), `Snatch` (arrests).
    SquadKindFile => SquadKind { Line = "line", Riot = "riot", Snatch = "snatch" } default Line
);
mirror_enum!(
    /// `Phones`, `Degraded` or `Shutdown`.
    CommsModeFile => CommsMode { Phones = "phones", Degraded = "degraded", Shutdown = "shutdown" }
    default Phones
);
mirror_enum!(
    /// `Peaceful`, `Defensive` or `Confrontational`.
    StanceFile => Stance {
        Peaceful = "peaceful",
        Defensive = "defensive",
        Confrontational = "confrontational",
    } default Peaceful
);
mirror_enum!(
    /// `MarchTo`, `Hold` or `Disperse`.
    ObjectiveKindFile => ObjectiveKind { MarchTo = "march_to", Hold = "hold", Disperse = "disperse" }
    default MarchTo
);

impl ScenarioFile {
    /// Parses RON. `Option` fields may be written without `Some(…)`.
    pub fn from_ron(text: &str) -> Result<ScenarioFile> {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(text)
            .map_err(ron_error)
    }

    /// Writes RON that [`ScenarioFile::from_ron`] reads back.
    pub fn to_ron(&self) -> String {
        let cfg = ron::ser::PrettyConfig::new()
            .struct_names(false)
            .extensions(ron::extensions::Extensions::IMPLICIT_SOME);
        ron::ser::to_string_pretty(self, cfg).expect("scenario serializes")
    }

    /// The mirror of `setup`, for a scenario file naming `map`.
    pub fn from_setup(setup: &RiotSetup, map: &str) -> ScenarioFile {
        let names = |v: &[f32; ARCHETYPES], skip: f32| -> BTreeMap<String, f32> {
            Archetype::ALL
                .iter()
                .filter(|a| a.is_crowd() && v[**a as usize] != skip)
                .map(|a| (a.name().to_string(), v[*a as usize]))
                .collect()
        };
        ScenarioFile {
            title: String::new(),
            description: String::new(),
            map: map.to_string(),
            tunables: None,
            tune: BTreeMap::new(),
            seed: setup.seed,
            start: Time::Minutes(setup.start_minute),
            duration: Time::Minutes(setup.duration as f32 / (60 * TICK_HZ) as f32),
            crowd: Archetype::ALL
                .iter()
                .filter(|a| a.is_crowd() && setup.crowd[**a as usize] > 0)
                .map(|a| (a.name().to_string(), setup.crowd[*a as usize]))
                .collect(),
            organizers: setup.organizers,
            arrivals: setup
                .arrivals
                .iter()
                .map(|a| ArrivalFile {
                    entry: a.entry.clone(),
                    from: Time::Minutes(a.from_min),
                    to: Time::Minutes(a.to_min),
                    share: a.share,
                    bias: names(&a.bias, 1.0),
                })
                .collect(),
            supplies: setup.supplies.into(),
            police: PoliceFile {
                squads: setup
                    .police
                    .squads
                    .iter()
                    .map(|s| SquadFile {
                        kind: s.kind.into(),
                        officers: s.officers,
                        depot: s.depot.clone(),
                        staging: s.staging.clone(),
                        available: (s.available_min != 0.0)
                            .then_some(Time::Minutes(s.available_min)),
                        count: 1,
                    })
                    .collect(),
                vans_per_depot: setup.police.vans_per_depot,
                water_cannons: setup.police.water_cannons,
                aggression: setup.police.aggression,
            },
            escalation_cap: setup.escalation_cap.into(),
            comms: CommsFile {
                mode: setup.comms.mode.into(),
                shutdown_at: setup.comms.shutdown_min.map(Time::Minutes),
            },
            weather: WeatherFile {
                wind: setup.weather.wind,
                heat: setup.weather.heat,
                dusk: Time::Minutes(setup.weather.dusk_minute),
            },
            objectives: setup
                .objectives
                .iter()
                .map(|o| ObjectiveFile {
                    name: o.name.clone(),
                    kind: o.kind.into(),
                    area: o.area.clone(),
                    pos: o.pos,
                    radius: o.radius,
                    weight: o.weight,
                    announced: o.announced,
                    stance: o.stance.into(),
                })
                .collect(),
            limits: setup.limits.into(),
        }
    }

    /// Resolves names against `map` and converts to a [`RiotSetup`].
    pub fn into_setup(&self, map: &MapDef) -> Result<RiotSetup> {
        let start = self.start.minutes();
        let mut crowd = [0u32; ARCHETYPES];
        for (name, &n) in &self.crowd {
            crowd[crowd_archetype(name).map_err(|e| e.at("crowd"))? as usize] = n;
        }
        let entries: Vec<&str> = map.entries.iter().map(|e| e.name.as_str()).collect();
        let mut arrivals = Vec::new();
        for (k, a) in self.arrivals.iter().enumerate() {
            let at = || format!("arrivals[{k}]");
            let (from, to) = (a.from.after(start), a.to.after(start));
            if to < from {
                return Err(DataError::new(format!(
                    "ends ({}) before it starts ({})",
                    a.to, a.from
                ))
                .at(at()));
            }
            let mut bias = [1.0f32; ARCHETYPES];
            for (name, &b) in &a.bias {
                bias[crowd_archetype(name).map_err(|e| e.at("bias").at(at()))? as usize] = b;
            }
            let matched = matching(&a.entry, &entries, "entry").map_err(|e| e.at(at()))?;
            let share = a.share / matched.len() as f32;
            for e in matched {
                arrivals.push(Arrival {
                    entry: e.to_string(),
                    from_min: from,
                    to_min: to,
                    share,
                    bias,
                });
            }
        }
        let depots: Vec<&str> = map.depots.iter().map(|e| e.name.as_str()).collect();
        let staging: Vec<&str> = map.staging.iter().map(|e| e.name.as_str()).collect();
        let mut squads = Vec::new();
        for (k, s) in self.police.squads.iter().enumerate() {
            let at = || format!("police.squads[{k}]");
            if !(1..=16).contains(&s.officers) {
                return Err(DataError::new(format!(
                    "{} officers: a squad has 1 to 16 (6–12 is typical)",
                    s.officers
                ))
                .at(at()));
            }
            let ds = matching(&s.depot, &depots, "depot").map_err(|e| e.at(at()))?;
            let ss = match &s.staging {
                Some(p) => Some(matching(p, &staging, "staging point").map_err(|e| e.at(at()))?),
                None => None,
            };
            for i in 0..s.count as usize {
                squads.push(SquadDef {
                    kind: s.kind.into(),
                    officers: s.officers,
                    depot: ds[i % ds.len()].to_string(),
                    staging: ss.as_ref().map(|v| v[i % v.len()].to_string()),
                    available_min: s.available.map_or(0.0, |t| t.after(start)),
                });
            }
        }
        let mut objectives = Vec::new();
        for (k, o) in self.objectives.iter().enumerate() {
            if let Some(area) = &o.area
                && map.area(area).is_none()
            {
                let areas: Vec<&str> = map.areas.iter().map(|a| a.name.as_str()).collect();
                return Err(DataError::new(format!(
                    "unknown area `{area}` (areas: {})",
                    list(&areas)
                ))
                .at(format!("objectives[{k}]")));
            }
            objectives.push(ObjectiveDef {
                name: o.name.clone(),
                kind: o.kind.into(),
                area: o.area.clone(),
                pos: o.pos,
                radius: o.radius,
                weight: o.weight,
                announced: o.announced,
                stance: o.stance.into(),
            });
        }
        let officers: u32 = squads.iter().map(|s| s.officers as u32).sum();
        let people = crowd.iter().sum::<u32>() + self.organizers as u32 + officers;
        if people > MAX_PEOPLE {
            return Err(DataError::new(format!(
                "{people} people (crowd, organizers and officers) is more than the {MAX_PEOPLE} \
                 one riot can hold"
            )));
        }
        let duration = self.duration.minutes();
        if duration <= 0.0 {
            return Err(DataError::new("duration must be more than 0 minutes"));
        }
        Ok(RiotSetup {
            seed: self.seed,
            map: map.name.clone(),
            crowd,
            arrivals,
            organizers: self.organizers,
            supplies: self.supplies.into(),
            police: PoliceForce {
                squads,
                vans_per_depot: self.police.vans_per_depot,
                water_cannons: self.police.water_cannons,
                aggression: self.police.aggression,
            },
            escalation_cap: self.escalation_cap.into(),
            comms: Comms {
                mode: self.comms.mode.into(),
                shutdown_min: self.comms.shutdown_at.map(|t| t.after(start)),
            },
            weather: Weather {
                wind: self.weather.wind,
                heat: self.weather.heat,
                dusk_minute: self.weather.dusk.minutes(),
            },
            objectives,
            duration: minutes(duration),
            start_minute: start,
            limits: self.limits.into(),
        })
    }
}

/// A crowd archetype by name.
fn crowd_archetype(name: &str) -> Result<Archetype> {
    let crowd: Vec<&str> = Archetype::ALL
        .iter()
        .filter(|a| a.is_crowd())
        .map(|a| a.name())
        .collect();
    match Archetype::from_name(name) {
        Some(a) if a.is_crowd() => Ok(a),
        Some(Archetype::Organizer) => Err(DataError::new(
            "`organizer` is not a crowd archetype: set `organizers: N` instead",
        )),
        Some(a) => Err(DataError::new(format!(
            "`{}` is not a crowd archetype: officers come from police squads",
            a.name()
        ))),
        None => Err(DataError::new(format!(
            "unknown archetype `{name}` (expected {})",
            crowd.join(", ")
        ))),
    }
}

/// Names matching `pattern`: the name itself, or every name starting with
/// the prefix before a trailing `*`.
fn matching<'a>(pattern: &str, names: &[&'a str], what: &str) -> Result<Vec<&'a str>> {
    let found: Vec<&'a str> = match pattern.strip_suffix('*') {
        Some(prefix) => names
            .iter()
            .copied()
            .filter(|n| n.starts_with(prefix))
            .collect(),
        None => names.iter().copied().filter(|n| *n == pattern).collect(),
    };
    if found.is_empty() {
        return Err(DataError::new(format!(
            "no {what} matches `{pattern}` ({what}s in the map: {})",
            list(names)
        )));
    }
    Ok(found)
}

/// A short list of names for error messages.
fn list(names: &[&str]) -> String {
    const SHOW: usize = 12;
    if names.is_empty() {
        return "none".into();
    }
    let mut s = names[..names.len().min(SHOW)].join(", ");
    if names.len() > SHOW {
        s.push_str(&format!(", … {} more", names.len() - SHOW));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use riot_testkit as testkit;

    #[test]
    fn setup_round_trips_through_ron() {
        let map = testkit::district(1024.0, 64.0);
        let mut setup = testkit::march(&map, 2000, 8, 7);
        setup.comms.shutdown_min = Some(12.5);
        setup.arrivals[1].bias[Archetype::Radical as usize] = 2.5;
        setup.limits.field_chunks = 300;
        let file = ScenarioFile::from_setup(&setup, "generated:testkit");
        let text = file.to_ron();
        let back = ScenarioFile::from_ron(&text).unwrap();
        assert_eq!(back, file, "{text}");
        assert_eq!(back.into_setup(&map).unwrap(), setup);
    }

    #[test]
    fn times() {
        assert_eq!(Time::Clock(23.0 * 60.0).after(22.0 * 60.0), 60.0);
        assert_eq!(Time::Clock(60.0).after(22.0 * 60.0), 180.0);
        assert_eq!(Time::Minutes(15.0).after(600.0), 15.0);
        assert_eq!(Time::Clock(9.0 * 60.0 + 5.0).to_string(), "09:05");
    }
}
