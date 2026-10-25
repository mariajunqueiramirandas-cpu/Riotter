//! Per-agent state codes stored in the `u8` lanes of the agent table
//! (DESIGN §2.3, §6.2).

/// What an agent is doing right now (DESIGN §10). Civilians and officers
/// share the byte; officer actions start at `Formation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Action {
    /// Follow the goal.
    #[default]
    March = 0,
    /// Stay near the goal, face the threat.
    Hold = 1,
    /// Move down the threat/gas gradient toward exits.
    Flee = 2,
    Rest = 3,
    /// Go to water or a medic.
    SeekSupply = 4,
    /// Carry someone injured to a medic.
    Help = 5,
    Film = 6,
    Throw = 7,
    Barricade = 8,
    /// Organizers only.
    Chant = 9,
    /// Leave the map: a real loss of turnout.
    GoHome = 10,
    /// Mill about with no plan.
    Idle = 11,
    // --- officers
    /// Hold a slot in the squad's formation.
    Formation = 12,
    /// Formation broken: retreat away from the threat.
    Break = 13,
    /// Baton strike on a neighbour.
    Strike = 14,
    /// Grab and restrain the squad's snatch target.
    Arrest = 15,
    /// Walk a restrained arrestee to the van.
    Escort = 16,
    /// Ride in a van (not on the map).
    Board = 17,
    /// Wait out of sight for an ambush: crouched, silent, no threat.
    Lurk = 18,
}

pub const CIVILIAN_ACTIONS: usize = 12;
pub const ACTIONS: usize = 19;

impl Action {
    pub const ALL: [Action; ACTIONS] = [
        Action::March,
        Action::Hold,
        Action::Flee,
        Action::Rest,
        Action::SeekSupply,
        Action::Help,
        Action::Film,
        Action::Throw,
        Action::Barricade,
        Action::Chant,
        Action::GoHome,
        Action::Idle,
        Action::Formation,
        Action::Break,
        Action::Strike,
        Action::Arrest,
        Action::Escort,
        Action::Board,
        Action::Lurk,
    ];

    #[inline]
    pub fn from_u8(x: u8) -> Action {
        Action::ALL[(x as usize).min(ACTIONS - 1)]
    }

    /// Does this action follow a flow field?
    #[inline]
    pub fn follows_flow(self) -> bool {
        matches!(
            self,
            Action::March | Action::Flee | Action::GoHome | Action::SeekSupply | Action::Help
        )
    }

    /// Does this action produce interaction events (DESIGN §11.3)?
    #[inline]
    pub fn interacts(self) -> bool {
        matches!(
            self,
            Action::Throw
                | Action::Barricade
                | Action::Help
                | Action::Strike
                | Action::Arrest
                | Action::Escort
                | Action::Film
                | Action::Chant
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Action::March => "march",
            Action::Hold => "hold",
            Action::Flee => "flee",
            Action::Rest => "rest",
            Action::SeekSupply => "seek_supply",
            Action::Help => "help",
            Action::Film => "film",
            Action::Throw => "throw",
            Action::Barricade => "barricade",
            Action::Chant => "chant",
            Action::GoHome => "go_home",
            Action::Idle => "idle",
            Action::Formation => "formation",
            Action::Break => "break",
            Action::Strike => "strike",
            Action::Arrest => "arrest",
            Action::Escort => "escort",
            Action::Board => "board",
            Action::Lurk => "lurk",
        }
    }
}

/// Body pose (DESIGN §6.2 pose bytes). Follows from the action and speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Gait {
    #[default]
    Stand = 0,
    Walk = 1,
    Run = 2,
    Crouch = 3,
    Fallen = 4,
    Carried = 5,
    Restrained = 6,
    Throw = 7,
    Strike = 8,
    Film = 9,
}

impl Gait {
    pub const ALL: [Gait; 10] = [
        Gait::Stand,
        Gait::Walk,
        Gait::Run,
        Gait::Crouch,
        Gait::Fallen,
        Gait::Carried,
        Gait::Restrained,
        Gait::Throw,
        Gait::Strike,
        Gait::Film,
    ];

    #[inline]
    pub fn from_u8(x: u8) -> Gait {
        Gait::ALL[(x as usize).min(9)]
    }

    /// Lying on the ground or held: no thinking, no steering.
    #[inline]
    pub fn is_down(self) -> bool {
        matches!(self, Gait::Fallen | Gait::Carried | Gait::Restrained)
    }
}

/// Indices into the `pose` lanes.
pub mod pose {
    pub const HEADING: usize = 0;
    pub const GAIT: usize = 1;
    pub const STRIDE: usize = 2;
    pub const PRESSURE: usize = 3;
}

/// Indices into the `emo` lanes.
pub mod emo {
    pub const FEAR: usize = 0;
    pub const ANGER: usize = 1;
    pub const MORALE: usize = 2;
    pub const FATIGUE: usize = 3;
}

/// Indices into the `stat` lanes.
pub mod stat {
    pub const HEALTH: usize = 0;
    pub const ACTION: usize = 1;
    pub const ARCH: usize = 2;
    pub const FLAGS: usize = 3;
}

/// Bits of the `flags` byte.
pub mod flag {
    /// Not a live agent: removed, or padding past `len`.
    pub const DEAD: u8 = 1 << 0;
    pub const ASLEEP: u8 = 1 << 1;
    /// Think on the next tick (gassed, crushed, hit, woken).
    pub const URGENT: u8 = 1 << 2;
    pub const PHONE: u8 = 1 << 3;
    /// Identified by cameras or surveillance.
    pub const KNOWN: u8 = 1 << 4;
    /// Holding an item (kind in cold data).
    pub const ITEM: u8 = 1 << 5;
    /// Carries a shield.
    pub const SHIELD: u8 = 1 << 6;
    /// Fallen, carried or restrained.
    pub const DOWN: u8 = 1 << 7;
}

/// Health thresholds on the `u8` health byte.
pub mod health {
    pub const FULL: u8 = 255;
    /// Below this, a person counts as injured.
    pub const INJURED: u8 = 170;
    /// Below this, a person cannot stand.
    pub const INCAPACITATED: u8 = 60;
}

/// `intent`: objective slot in the low byte (`NO_GOAL` = none), stance in
/// bits 8..10.
pub mod intent {
    use crate::archetype::Stance;
    pub const NO_GOAL: u8 = 0xff;
    #[inline]
    pub fn pack(objective: u8, stance: Stance) -> u16 {
        objective as u16 | ((stance as u16) << 8)
    }
    #[inline]
    pub fn objective(i: u16) -> u8 {
        i as u8
    }
    #[inline]
    pub fn stance(i: u16) -> Stance {
        Stance::from_u8((i >> 8) as u8 & 3)
    }
}

/// `belief`: message id in the low byte (0 = none), fidelity in the high
/// byte, so a plain `max` keeps the clearest message (DESIGN §13).
pub mod belief {
    #[inline]
    pub fn pack(id: u8, fidelity: u8) -> u16 {
        id as u16 | ((fidelity as u16) << 8)
    }
    #[inline]
    pub fn id(b: u16) -> u8 {
        b as u8
    }
    #[inline]
    pub fn fidelity(b: u16) -> u8 {
        (b >> 8) as u8
    }
}

/// `aux`: a 24-bit target (a stable id, prop, drop or goal, depending on the
/// action) and an 8-bit stillness counter used for sleeping (DESIGN §7.6).
pub mod aux {
    pub const NO_TARGET: u32 = 0x00ff_ffff;
    #[inline]
    pub fn target(a: u32) -> u32 {
        a & NO_TARGET
    }
    #[inline]
    pub fn still(a: u32) -> u8 {
        (a >> 24) as u8
    }
    #[inline]
    pub fn with_target(a: u32, t: u32) -> u32 {
        (a & !NO_TARGET) | (t & NO_TARGET)
    }
    #[inline]
    pub fn with_still(a: u32, s: u8) -> u32 {
        (a & NO_TARGET) | ((s as u32) << 24)
    }
}

/// Items a person can hold (cold data).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Item {
    #[default]
    None = 0,
    Sign = 1,
    Shield = 2,
    Bottle = 3,
    Camera = 4,
    Flag = 5,
    Baton = 6,
    Megaphone = 7,
    Molotov = 8,
    Water = 9,
    Plank = 10,
}

impl Item {
    #[inline]
    pub fn from_u8(x: u8) -> Item {
        match x {
            1 => Item::Sign,
            2 => Item::Shield,
            3 => Item::Bottle,
            4 => Item::Camera,
            5 => Item::Flag,
            6 => Item::Baton,
            7 => Item::Megaphone,
            8 => Item::Molotov,
            9 => Item::Water,
            10 => Item::Plank,
            _ => Item::None,
        }
    }
}
