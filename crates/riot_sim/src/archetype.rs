//! Archetypes (DESIGN §2.2, §6.2). Body radius, mass, speed and turn rate are
//! per-archetype tables, not per-agent storage.

pub const ARCHETYPES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Archetype {
    Student = 0,
    Worker = 1,
    Elder = 2,
    Radical = 3,
    Organizer = 4,
    Provocateur = 5,
    Journalist = 6,
    Bystander = 7,
    Officer = 8,
    RiotOfficer = 9,
}

impl Archetype {
    pub const ALL: [Archetype; ARCHETYPES] = [
        Archetype::Student,
        Archetype::Worker,
        Archetype::Elder,
        Archetype::Radical,
        Archetype::Organizer,
        Archetype::Provocateur,
        Archetype::Journalist,
        Archetype::Bystander,
        Archetype::Officer,
        Archetype::RiotOfficer,
    ];

    #[inline]
    pub fn from_u8(x: u8) -> Archetype {
        Archetype::ALL[(x as usize).min(ARCHETYPES - 1)]
    }

    #[inline]
    pub fn is_officer(self) -> bool {
        matches!(self, Archetype::Officer | Archetype::RiotOfficer)
    }

    /// Archetypes that make up the civilian crowd from `RiotSetup::crowd`.
    #[inline]
    pub fn is_crowd(self) -> bool {
        !self.is_officer() && self != Archetype::Organizer
    }

    pub fn name(self) -> &'static str {
        match self {
            Archetype::Student => "student",
            Archetype::Worker => "worker",
            Archetype::Elder => "elder",
            Archetype::Radical => "radical",
            Archetype::Organizer => "organizer",
            Archetype::Provocateur => "provocateur",
            Archetype::Journalist => "journalist",
            Archetype::Bystander => "bystander",
            Archetype::Officer => "officer",
            Archetype::RiotOfficer => "riot_officer",
        }
    }

    pub fn from_name(s: &str) -> Option<Archetype> {
        Archetype::ALL.iter().copied().find(|a| a.name() == s)
    }
}

/// How a message's stance asks people to behave (DESIGN §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Stance {
    #[default]
    Peaceful = 0,
    Defensive = 1,
    Confrontational = 2,
}

impl Stance {
    #[inline]
    pub fn from_u8(x: u8) -> Stance {
        match x {
            0 => Stance::Peaceful,
            1 => Stance::Defensive,
            _ => Stance::Confrontational,
        }
    }
}

/// Per-archetype defaults (DESIGN §2.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArchetypeParams {
    /// Body radius, metres.
    pub radius: f32,
    /// Mass, kg.
    pub mass: f32,
    /// Preferred walking speed, m/s.
    pub walk: f32,
    /// Running speed, m/s; also the speed clamp.
    pub run: f32,
    /// Maximum heading change per tick, in 1.4° steps.
    pub turn_rate: u8,
    /// Velocity blend per tick towards the desired velocity.
    pub response: f32,
    pub base_fear: f32,
    pub base_anger: f32,
    pub base_morale: f32,
    /// Fear above which fleeing dominates.
    pub fear_threshold: f32,
    /// Trust in organizers' messages.
    pub trust: f32,
    /// Stamina: how slowly fatigue builds (1 = normal).
    pub stamina: f32,
    /// Willingness to use violence.
    pub violence: f32,
    /// Chance of carrying a phone.
    pub phone: f32,
    pub default_stance: Stance,
}

const fn p(
    radius: f32,
    mass: f32,
    walk: f32,
    run: f32,
    turn_rate: u8,
    response: f32,
    base_fear: f32,
    base_anger: f32,
    base_morale: f32,
    fear_threshold: f32,
    trust: f32,
    stamina: f32,
    violence: f32,
    phone: f32,
    default_stance: Stance,
) -> ArchetypeParams {
    ArchetypeParams {
        radius,
        mass,
        walk,
        run,
        turn_rate,
        response,
        base_fear,
        base_anger,
        base_morale,
        fear_threshold,
        trust,
        stamina,
        violence,
        phone,
        default_stance,
    }
}

use Stance::*;

/// Default archetype table, indexed by `Archetype as usize`.
pub const ARCHETYPE_PARAMS: [ArchetypeParams; ARCHETYPES] = [
    //  radius mass walk  run  turn resp  fear  anger morale thr  trust stam  viol  phone
    p(
        0.22, 65.0, 1.35, 3.6, 24, 0.35, 0.12, 0.30, 0.60, 0.55, 0.60, 1.00, 0.15, 0.90, Defensive,
    ), // student
    p(
        0.24, 78.0, 1.25, 3.1, 20, 0.30, 0.12, 0.30, 0.55, 0.60, 0.55, 0.90, 0.12, 0.80, Peaceful,
    ), // worker
    p(
        0.23, 70.0, 0.90, 1.7, 12, 0.15, 0.20, 0.20, 0.50, 0.35, 0.65, 0.50, 0.03, 0.50, Peaceful,
    ), // elder
    p(
        0.23,
        72.0,
        1.40,
        4.0,
        26,
        0.40,
        0.08,
        0.55,
        0.60,
        0.80,
        0.35,
        1.00,
        0.60,
        0.85,
        Confrontational,
    ), // radical
    p(
        0.23, 72.0, 1.30, 3.3, 22, 0.35, 0.08, 0.35, 0.80, 0.75, 1.00, 1.00, 0.05, 1.00, Peaceful,
    ), // organizer
    p(
        0.24,
        80.0,
        1.40,
        4.0,
        26,
        0.40,
        0.04,
        0.60,
        0.70,
        0.90,
        0.00,
        1.00,
        0.85,
        1.00,
        Confrontational,
    ), // provocateur
    p(
        0.23, 70.0, 1.30, 3.3, 22, 0.35, 0.12, 0.10, 0.60, 0.60, 0.20, 0.90, 0.00, 1.00, Peaceful,
    ), // journalist
    p(
        0.23, 70.0, 1.20, 3.0, 18, 0.30, 0.20, 0.10, 0.50, 0.40, 0.20, 0.80, 0.02, 0.90, Peaceful,
    ), // bystander
    p(
        0.27, 90.0, 1.30, 3.3, 20, 0.35, 0.08, 0.25, 0.70, 0.80, 1.00, 0.90, 0.30, 1.00, Defensive,
    ), // officer
    p(
        0.32, 105.0, 1.20, 2.9, 16, 0.30, 0.06, 0.30, 0.75, 0.85, 1.00, 0.80, 0.45, 1.00, Defensive,
    ), // riot officer
];

/// Contact reach margin added to the sum of two radii (DESIGN §7.3).
pub const SKIN: f32 = 0.15;

/// Largest body radius in the table.
pub fn max_radius(table: &[ArchetypeParams; ARCHETYPES]) -> f32 {
    table.iter().map(|a| a.radius).fold(0.0, f32::max)
}
