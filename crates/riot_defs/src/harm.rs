//! What took someone's health (DESIGN §2.6, §17), for the outcome's
//! breakdown and each person's last injury.

/// What took someone's health, for the outcome's breakdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Harm {
    /// A police baton.
    Baton = 0,
    /// A protester's blow.
    Blow = 1,
    Projectile = 2,
    /// Crowd pressure, falls and trampling.
    Crush = 3,
    Fire = 4,
    Gas = 5,
}

pub const HARMS: usize = 6;

impl Harm {
    pub const ALL: [Harm; HARMS] = [
        Harm::Baton,
        Harm::Blow,
        Harm::Projectile,
        Harm::Crush,
        Harm::Fire,
        Harm::Gas,
    ];

    pub fn from_u8(v: u8) -> Harm {
        Harm::ALL[(v as usize).min(HARMS - 1)]
    }

    pub fn name(self) -> &'static str {
        match self {
            Harm::Baton => "baton",
            Harm::Blow => "blow",
            Harm::Projectile => "projectile",
            Harm::Crush => "crush",
            Harm::Fire => "fire",
            Harm::Gas => "gas",
        }
    }
}
