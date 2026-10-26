//! What the police are doing, in the terms the snapshot, the HUD and the
//! tools report it (DESIGN §14): squad doctrines, the commander's
//! operations, barricade states and the police radio.

use riot_core::Vec2;

use crate::setup::Escalation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Doctrine {
    /// Not yet available, or in a van.
    #[default]
    Idle = 0,
    Travel = 1,
    HoldLine = 2,
    Advance = 3,
    Kettle = 4,
    Disperse = 5,
    Snatch = 6,
    Retreat = 7,
    Reserve = 8,
    /// Put steel barriers across a street and hold behind them.
    Barricade = 9,
    /// Wait out of sight in a side street for the crowd to come.
    Ambush = 10,
    /// Run at the crowd, batons out: a sprung ambush cutting a crowd in two,
    /// then sweeping the rear part back the way it came.
    Charge = 11,
    /// Walk alongside a march.
    Escort = 12,
}

pub const DOCTRINES: usize = 13;

pub const NO_CORDON: u8 = u8::MAX;
pub const NO_SITE: u8 = u8::MAX;
/// Radio calls kept for the HUD.
pub const RADIO: usize = 8;

/// What the police do about a crowd.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OpKind {
    /// Too small or too quiet to bother with.
    #[default]
    None = 0,
    /// Lines at a distance.
    Watch = 1,
    /// Walk alongside a march.
    Escort = 2,
    /// Barricade its route to a protected site.
    Cordon = 3,
    /// Lines between it and the site it reached.
    Protect = 4,
    /// Kettle it.
    Contain = 5,
    /// Gas, water and lines push it away.
    Disperse = 6,
}

impl OpKind {
    pub const ALL: [OpKind; 7] = [
        OpKind::None,
        OpKind::Watch,
        OpKind::Escort,
        OpKind::Cordon,
        OpKind::Protect,
        OpKind::Contain,
        OpKind::Disperse,
    ];
    pub fn from_u8(x: u8) -> OpKind {
        OpKind::ALL[(x as usize).min(6)]
    }
    pub fn name(self) -> &'static str {
        match self {
            OpKind::None => "none",
            OpKind::Watch => "watch",
            OpKind::Escort => "escort",
            OpKind::Cordon => "cordon",
            OpKind::Protect => "protect",
            OpKind::Contain => "contain",
            OpKind::Disperse => "disperse",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CordonState {
    #[default]
    Planned = 0,
    Building = 1,
    Up = 2,
    Breached = 3,
    Gone = 4,
}

/// What the police radio reports, for the HUD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Call {
    #[default]
    None = 0,
    Tracking = 1,
    Cordon = 2,
    BarricadeUp = 3,
    Breach = 4,
    AmbushSet = 5,
    AmbushSprung = 6,
    Kettle = 7,
    KettleClosed = 8,
    GateOpen = 9,
    Warning = 10,
    Gas = 11,
    WaterCannon = 12,
    Snatch = 13,
    Relief = 14,
    FallBack = 15,
    Escalate = 16,
    Deescalate = 17,
    LineBroken = 18,
    Escort = 19,
    Dismantle = 20,
    /// A barricade opened to ease a crush against it.
    Opened = 21,
}

impl Call {
    pub const ALL: [Call; 22] = [
        Call::None,
        Call::Tracking,
        Call::Cordon,
        Call::BarricadeUp,
        Call::Breach,
        Call::AmbushSet,
        Call::AmbushSprung,
        Call::Kettle,
        Call::KettleClosed,
        Call::GateOpen,
        Call::Warning,
        Call::Gas,
        Call::WaterCannon,
        Call::Snatch,
        Call::Relief,
        Call::FallBack,
        Call::Escalate,
        Call::Deescalate,
        Call::LineBroken,
        Call::Escort,
        Call::Dismantle,
        Call::Opened,
    ];
    pub fn from_u8(x: u8) -> Call {
        Call::ALL[(x as usize).min(Call::ALL.len() - 1)]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RadioCall {
    pub tick: u32,
    pub call: Call,
    pub pos: Vec2,
    /// A number that goes with the call: people, squads, canisters, a rung.
    pub n: u32,
}

/// What a radio call says, for people watching (not used by the
/// simulation). `rung` names an escalation rung.
pub fn call_text(c: &RadioCall) -> String {
    let rung = |n: u32| match Escalation::from_u8(n as u8) {
        Escalation::Presence => "presence",
        Escalation::LinesAndKettles => "lines & kettles",
        Escalation::BatonsAndArrests => "batons & arrests",
        Escalation::GasAndWater => "gas & water cannon",
        Escalation::MassArrests => "mass arrests",
    };
    let n = c.n;
    match c.call {
        Call::None => String::new(),
        Call::Tracking => format!("tracking a crowd of {n}"),
        Call::Cordon => format!("cordon ordered: {n} barriers across the street"),
        Call::BarricadeUp => format!("barricade up: {n} barriers"),
        Call::Breach => "barricade breached!".to_string(),
        Call::AmbushSet => format!("squad {n} moving into ambush position"),
        Call::AmbushSprung => format!("ambush sprung: squad {n} charges the flank"),
        Call::Kettle => format!("kettling {n} people"),
        Call::KettleClosed => format!("kettle closed around {n} people"),
        Call::GateOpen => format!("kettle gate open: filtering {n} people out"),
        Call::Warning => format!("dispersal order to {n} people"),
        Call::Gas => format!("gas volley: {n} canister{}", if n == 1 { "" } else { "s" }),
        Call::WaterCannon => format!("water cannon {n} spraying"),
        Call::Snatch => format!("snatch squad going in for #{n}"),
        Call::Relief => format!("squad {n} relieves a worn-out line"),
        Call::FallBack => format!("squad {n} reinforces a barricade"),
        Call::Escalate => format!("escalating: {}", rung(n)),
        Call::Deescalate => format!("standing down: {}", rung(n)),
        Call::LineBroken => format!("line broken! squad {n} falls back"),
        Call::Escort => format!("escorting a march of {n}"),
        Call::Dismantle => "barricade taken down".to_string(),
        Call::Opened => format!("crush at the barricade: opening it ({n} people pinned)"),
    }
}
