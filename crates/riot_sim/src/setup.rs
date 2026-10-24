//! The boundary with the campaign (DESIGN §17): a `RiotSetup` goes in and a
//! `RiotOutcome` comes out. `Vec` and `String` are fine here, because these
//! structs are built once per riot and never touched inside the tick.

use crate::archetype::{ARCHETYPES, Stance};

/// Ticks per second. `dt = 1 / TICK_HZ` is a constant (DESIGN §4.2).
pub const TICK_HZ: u32 = 20;
pub const DT: f32 = 1.0 / TICK_HZ as f32;

/// Minutes of game time → ticks.
#[inline]
pub fn minutes(m: f32) -> u32 {
    (m * 60.0 * TICK_HZ as f32 + 0.5) as u32
}

#[derive(Clone, Debug, PartialEq)]
pub struct RiotSetup {
    pub seed: u64,
    /// Map name, resolved by `riot_data` to a [`crate::MapDef`].
    pub map: String,
    /// Total turnout per archetype. Organizer and officer entries are ignored:
    /// organizers come from `organizers` and officers from `police`.
    pub crowd: [u32; ARCHETYPES],
    /// Turnout curve per entry point over time.
    pub arrivals: Vec<Arrival>,
    pub organizers: u16,
    /// What the movement's logistics delivered.
    pub supplies: Supplies,
    pub police: PoliceForce,
    pub escalation_cap: Escalation,
    pub comms: Comms,
    pub weather: Weather,
    pub objectives: Vec<ObjectiveDef>,
    /// Length of the riot in ticks (auto-resolve stops here).
    pub duration: u32,
    /// Time of day at tick 0, minutes after midnight.
    pub start_minute: f32,
    pub limits: Limits,
}

impl Default for RiotSetup {
    fn default() -> Self {
        RiotSetup {
            seed: 1,
            map: String::new(),
            crowd: [0; ARCHETYPES],
            arrivals: Vec::new(),
            organizers: 0,
            supplies: Supplies::default(),
            police: PoliceForce::default(),
            escalation_cap: Escalation::BatonsAndArrests,
            comms: Comms::default(),
            weather: Weather::default(),
            objectives: Vec::new(),
            duration: minutes(10.0),
            start_minute: 12.0 * 60.0,
            limits: Limits::default(),
        }
    }
}

/// One part of the turnout curve: people arriving through `entry` between
/// two times.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrival {
    /// Entry name in the map.
    pub entry: String,
    /// Minutes after tick 0.
    pub from_min: f32,
    pub to_min: f32,
    /// Relative share of the total turnout arriving through this window.
    pub share: f32,
    /// Per-archetype multiplier on `share` (radicals arriving after dark).
    pub bias: [f32; ARCHETYPES],
}

impl Arrival {
    pub fn new(entry: &str, from_min: f32, to_min: f32, share: f32) -> Self {
        Arrival {
            entry: entry.to_string(),
            from_min,
            to_min,
            share,
            bias: [1.0; ARCHETYPES],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Supplies {
    pub water: u32,
    pub medic: u32,
    pub shields: u32,
    pub barricade: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SupplyKind {
    #[default]
    Water = 0,
    Medic = 1,
    Shields = 2,
    Barricade = 3,
}

impl Supplies {
    pub fn get(&self, k: SupplyKind) -> u32 {
        match k {
            SupplyKind::Water => self.water,
            SupplyKind::Medic => self.medic,
            SupplyKind::Shields => self.shields,
            SupplyKind::Barricade => self.barricade,
        }
    }
    pub fn get_mut(&mut self, k: SupplyKind) -> &mut u32 {
        match k {
            SupplyKind::Water => &mut self.water,
            SupplyKind::Medic => &mut self.medic,
            SupplyKind::Shields => &mut self.shields,
            SupplyKind::Barricade => &mut self.barricade,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoliceForce {
    pub squads: Vec<SquadDef>,
    /// Vans per depot for moving squads and arrestees.
    pub vans_per_depot: u8,
    /// Water cannon trucks, spread over the depots.
    pub water_cannons: u8,
    /// 0 = cautious, 1 = aggressive. Scales how fast the director escalates.
    pub aggression: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SquadKind {
    /// Lines and kettles.
    Line = 0,
    /// Shields and batons.
    Riot = 1,
    /// Snatch arrests.
    Snatch = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SquadDef {
    pub kind: SquadKind,
    /// 6–12 officers.
    pub officers: u8,
    /// Depot the squad starts from.
    pub depot: String,
    /// Staging point to start at instead of the depot (already deployed).
    pub staging: Option<String>,
    /// Minutes after tick 0 when the squad becomes available.
    pub available_min: f32,
}

/// The escalation ladder (DESIGN §2.5). The setup caps the highest rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Escalation {
    #[default]
    Presence = 0,
    LinesAndKettles = 1,
    BatonsAndArrests = 2,
    GasAndWater = 3,
    MassArrests = 4,
}

impl Escalation {
    pub fn from_u8(x: u8) -> Escalation {
        match x {
            0 => Escalation::Presence,
            1 => Escalation::LinesAndKettles,
            2 => Escalation::BatonsAndArrests,
            3 => Escalation::GasAndWater,
            _ => Escalation::MassArrests,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum CommsMode {
    /// Phones and group chats work.
    #[default]
    Phones = 0,
    /// Overloaded networks: slow, lossy.
    Degraded = 1,
    /// Internet shutdown: word of mouth only.
    Shutdown = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Comms {
    pub mode: CommsMode,
    /// The state shuts the internet down at this minute.
    pub shutdown_min: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weather {
    /// Wind in m/s, carries gas and smoke.
    pub wind: [f32; 2],
    /// 0 = cool, 1 = hot: how fast people get thirsty and tired.
    pub heat: f32,
    /// Minute of the day when it gets dark.
    pub dusk_minute: f32,
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            wind: [1.0, 0.0],
            heat: 0.3,
            dusk_minute: 20.0 * 60.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ObjectiveKind {
    /// March to the area.
    #[default]
    MarchTo = 0,
    /// Hold the area against the police.
    Hold = 1,
    /// Disperse to the exits.
    Disperse = 2,
}

impl ObjectiveKind {
    pub fn from_u8(x: u8) -> ObjectiveKind {
        match x {
            0 => ObjectiveKind::MarchTo,
            1 => ObjectiveKind::Hold,
            _ => ObjectiveKind::Disperse,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectiveDef {
    pub name: String,
    pub kind: ObjectiveKind,
    /// A named area in the map, or `None` to use `pos`/`radius`.
    pub area: Option<String>,
    pub pos: [f32; 2],
    pub radius: f32,
    /// Weight in `ground_held`.
    pub weight: f32,
    /// Announced before the riot: every arriving protester already knows it.
    pub announced: bool,
    pub stance: Stance,
}

/// Preallocation sizes. Everything is allocated once, at riot start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Field chunks (128 m × 128 m) that can be active at once.
    pub field_chunks: u16,
    /// Flow tiles (8 m × 8 m) cached across all goals.
    pub flow_tiles: u32,
    /// Flow cost pages (one 128 m region of one goal each).
    pub flow_pages: u32,
    pub props: u32,
    pub projectiles: u32,
    pub notable: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            field_chunks: 160,
            flow_tiles: 24_576,
            flow_pages: 3072,
            props: 8192,
            projectiles: 1024,
            notable: 256,
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RiotOutcome {
    pub ticks: u32,
    /// Objective-weighted fraction of time held.
    pub ground_held: f32,
    pub arrests: u32,
    pub injuries: u32,
    pub deaths: u32,
    pub supplies_used: Supplies,
    /// Blame and reach, per side.
    pub footage: Footage,
    pub final_morale: f32,
    pub notable: Vec<NotableEvent>,
    /// People who ever arrived.
    pub turnout: u32,
    /// Most people on the map at once.
    pub peak_crowd: u32,
    /// People who went home early.
    pub went_home: u32,
    pub officers_injured: u32,
    /// Highest escalation rung the state used.
    pub max_escalation: Escalation,
    /// Fraction of time each objective was held, in setup order.
    pub objective_held: Vec<f32>,
    pub state_hash: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Footage {
    /// Footage of protesters starting violence.
    pub movement: SideFootage,
    /// Footage of police starting violence.
    pub police: SideFootage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SideFootage {
    /// Sum of severity of recorded acts this side started.
    pub blame: f32,
    /// How widely it was seen: cameras × severity.
    pub reach: f32,
    pub clips: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotableEvent {
    pub tick: u32,
    pub kind: NotableKind,
    pub pos: [f32; 2],
    /// Stable id of the person involved, or `u32::MAX`.
    pub who: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NotableKind {
    OrganizerArrested,
    Martyr,
    ViralMoment,
    LineBroken,
    KettleClosed,
    Crush,
    FirstGas,
    FirstWaterCannon,
    Fire,
    InternetShutdown,
    MetroClosed,
    ObjectiveTaken,
    EscalationRaised,
}
