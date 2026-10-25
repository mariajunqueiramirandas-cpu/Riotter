//! Outcome tallies (DESIGN §2.7, §17), kept as the riot runs: arrests,
//! injuries, footage, notable events. The per-tick accounting and the final
//! `RiotOutcome` are in `riot_outcome`.

use riot_core::{StateHasher, Vec2};

use riot_defs::archetype::{ARCHETYPES, Archetype};
use riot_defs::harm::HARMS;
use riot_defs::setup::{Footage, NotableEvent, NotableKind, RiotSetup, Supplies};
use riot_defs::targets::Objective;
use riot_fields::{Fields, Layer};

use crate::world::MAX_OBJECTIVES;

pub struct OutcomeAccum {
    pub arrests: u32,
    pub injuries: u32,
    pub deaths: u32,
    pub went_home: u32,
    pub turnout: u32,
    pub peak_crowd: u32,
    pub officers_injured: u32,
    pub officers_deployed: u32,
    pub supplies_used: Supplies,
    pub footage: Footage,
    pub notable: Vec<NotableEvent>,
    pub first_gas: bool,
    pub first_water: bool,
    pub crush_noted: bool,
    pub fires: u32,
    pub spawn_overflow: u32,
    last_viral: u32,
    pub arrived_by: [u32; ARCHETYPES],
    /// Health lost (in 1/255ths of a person) by cause.
    pub harm: [u32; HARMS],
    /// Deaths by the cause of the last health lost.
    pub deaths_by: [u32; HARMS],
    /// Objectives already taken once (noted the first time only).
    pub taken: [bool; MAX_OBJECTIVES],
}

impl OutcomeAccum {
    pub fn new(setup: &RiotSetup) -> Self {
        OutcomeAccum {
            arrests: 0,
            injuries: 0,
            deaths: 0,
            went_home: 0,
            turnout: 0,
            peak_crowd: 0,
            officers_injured: 0,
            officers_deployed: 0,
            supplies_used: Supplies::default(),
            footage: Footage::default(),
            notable: Vec::with_capacity(setup.limits.notable as usize),
            first_gas: false,
            first_water: false,
            crush_noted: false,
            fires: 0,
            spawn_overflow: 0,
            last_viral: 0,
            arrived_by: [0; ARCHETYPES],
            harm: [0; HARMS],
            deaths_by: [0; HARMS],
            taken: [false; MAX_OBJECTIVES],
        }
    }

    pub fn start(&mut self, _objectives: &[Objective]) {}

    pub fn arrived(&mut self, a: Archetype) {
        self.turnout += 1;
        self.arrived_by[a as usize] += 1;
    }

    pub fn injured(&mut self, officer: bool) {
        if officer {
            self.officers_injured += 1;
        } else {
            self.injuries += 1;
        }
    }

    pub fn notable(&mut self, tick: u32, kind: NotableKind, pos: Vec2, who: u32) {
        if self.notable.len() < self.notable.capacity() {
            self.notable.push(NotableEvent {
                tick,
                kind,
                pos: [pos.x, pos.y],
                who,
            });
        }
    }

    /// A protester started violence at `pos`. Returns whether a camera saw it.
    pub fn movement_act(
        &mut self,
        tick: u32,
        pos: Vec2,
        f: &Fields,
        severity: f32,
        who: u32,
    ) -> bool {
        let cams = f.sample(Layer::Camera, pos.x, pos.y);
        let side = &mut self.footage.movement;
        side.blame += severity;
        if cams > 0.5 {
            side.reach += severity * cams;
            side.clips += 1;
        }
        self.maybe_viral(tick, pos, severity * cams, who);
        cams > 0.5
    }

    /// Police force used at `to`. Hitting peaceful people is the large
    /// legitimacy win for the movement (DESIGN §2.7).
    pub fn police_act(
        &mut self,
        tick: u32,
        _from: Vec2,
        to: Vec2,
        f: &Fields,
        severity: f32,
        who: u32,
    ) {
        let cams = f.sample(Layer::Camera, to.x, to.y);
        let side = &mut self.footage.police;
        side.blame += severity;
        if cams > 0.5 {
            side.reach += severity * cams;
            side.clips += 1;
        }
        self.maybe_viral(tick, to, severity * cams, who);
    }

    fn maybe_viral(&mut self, tick: u32, pos: Vec2, score: f32, who: u32) {
        if score > 8.0 && (self.last_viral == 0 || tick.wrapping_sub(self.last_viral) > 1200) {
            self.last_viral = tick.max(1);
            self.notable(tick, NotableKind::ViralMoment, pos, who);
        }
    }

    pub fn hash_into(&self, h: &mut StateHasher) {
        for x in [
            self.arrests,
            self.injuries,
            self.deaths,
            self.went_home,
            self.turnout,
            self.officers_injured,
            self.fires,
        ] {
            h.write_u32(x);
        }
        h.write_f32(self.footage.movement.blame);
        h.write_f32(self.footage.police.blame);
        h.write_u64(self.notable.len() as u64);
    }
}
