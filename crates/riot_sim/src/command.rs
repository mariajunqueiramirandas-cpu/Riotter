//! All input is a command stamped with a tick (DESIGN §4.2 rule 4). A replay
//! is seed + setup + command log.

use crate::archetype::{Archetype, Stance};
use crate::setup::{ObjectiveKind, SupplyKind};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    pub tick: u32,
    pub kind: CommandKind,
}

impl Command {
    pub fn at(tick: u32, kind: CommandKind) -> Self {
        Command { tick, kind }
    }
}

/// None of the player's commands are orders (DESIGN §2.4): they are inputs
/// the crowd interprets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CommandKind {
    /// Create or move objective `slot` (0..16). Only agents who hear about it
    /// care.
    PlaceObjective {
        slot: u8,
        kind: ObjectiveKind,
        x: f32,
        y: f32,
        radius: f32,
    },
    ClearObjective {
        slot: u8,
    },
    /// Stance attached to future broadcasts.
    SetStance(Stance),
    /// An organizer starts a message: the objective + the current stance.
    Broadcast {
        organizer: u32,
        objective: u8,
    },
    /// Place supplies where the movement's logistics reach.
    DropSupplies {
        kind: SupplyKind,
        x: f32,
        y: f32,
        amount: u16,
    },
    /// An organizer starts or stops chanting.
    Chant {
        organizer: u32,
        on: bool,
    },
    SetTunable {
        id: u16,
        value: f32,
    },
    Debug(DebugCommand),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DebugCommand {
    /// Drop a gas canister.
    Gas { x: f32, y: f32 },
    /// Start a fire.
    Ignite { x: f32, y: f32 },
    /// Spawn people around a point, ignoring the arrival curve.
    Spawn {
        x: f32,
        y: f32,
        count: u16,
        archetype: Archetype,
        radius: f32,
    },
    /// Stamp threat, as if violence happened here.
    Threat { x: f32, y: f32, amount: f32 },
    /// Force the director's escalation rung (within the cap).
    Escalate { rung: u8 },
}
