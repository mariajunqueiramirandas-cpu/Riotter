//! Where people are headed, as the world holds it and steering and minds
//! read it: objectives, supply drops and officers' formation targets.

use riot_core::Vec2;

use crate::archetype::Stance;
use crate::setup::{ObjectiveKind, SupplyKind};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Objective {
    pub active: bool,
    pub kind: ObjectiveKind,
    pub pos: Vec2,
    pub radius: f32,
    pub weight: f32,
    pub stance: Stance,
    /// Ticks held by protesters.
    pub held_ticks: u32,
    /// Ticks this objective existed.
    pub live_ticks: u32,
    pub held_now: bool,
    /// Index in `RiotSetup::objectives`, or `u8::MAX` for player-placed ones.
    pub setup_index: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SupplyDrop {
    pub pos: Vec2,
    pub kind: SupplyKind,
    pub amount: u16,
}

/// Per-officer steering target, written by the police phase.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfficerTarget {
    pub pos: Vec2,
    /// Goal to route with when far from `pos`, or `NO_GOAL`.
    pub goal: u8,
    /// Where to face when standing.
    pub face: Vec2,
    pub run: bool,
}
