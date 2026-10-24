//! Phase 11a: outcome accounting (DESIGN §2.7, §17). A riot never just wins
//! or loses: it ends with ground held, arrests, injuries, footage, morale
//! and supplies used, and each matters separately.

use riot_core::math::widen_u8;
use riot_core::{StateHasher, Vec2};

use crate::archetype::{ARCHETYPES, Archetype};
use crate::fields::{Fields, Layer};
use crate::setup::{Footage, NotableEvent, NotableKind, RiotOutcome, RiotSetup, Supplies};
use crate::state::{emo, flag, stat};
use crate::world::{MAX_OBJECTIVES, Objective, World};

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
    taken: [bool; MAX_OBJECTIVES],
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

/// Every tick: crowd size; every 20 ticks: who holds each objective.
pub fn account(w: &mut World) {
    let officers = w.police.on_map.len() as u32;
    let crowd = w.spatial.live.saturating_sub(officers);
    w.outcome.peak_crowd = w.outcome.peak_crowd.max(crowd);
    if !w.tick.is_multiple_of(20) {
        return;
    }
    for k in 0..MAX_OBJECTIVES {
        let o = w.objectives[k];
        if !o.active {
            continue;
        }
        let mut people = 0.0f32;
        let (fx, fy) = w.fields.geom.to_cell_f(o.pos.x, o.pos.y);
        let rc = o.radius / crate::spatial::FIELD_CELL;
        for cy in (fy - rc).floor() as i32..=(fy + rc).ceil() as i32 {
            for cx in (fx - rc).floor() as i32..=(fx + rc).ceil() as i32 {
                let (dx, dy) = (cx as f32 + 0.5 - fx, cy as f32 + 0.5 - fy);
                if dx * dx + dy * dy <= rc * rc {
                    people += w.fields.cell_value(Layer::Dens, cx, cy);
                }
            }
        }
        let r2 = o.radius * o.radius;
        let mut police = 0u32;
        for &s in &w.police.on_map {
            if let Some(i) = w.index_of(s)
                && (w.people.pos(i) - o.pos).len2() <= r2
            {
                police += 1;
            }
        }
        let protesters = (people - police as f32).max(0.0);
        let area = core::f32::consts::PI * r2;
        let need = (w.tun.hold_density * area).min(150.0);
        let held = protesters >= need && (police as f32) < protesters / 8.0;
        let ob = &mut w.objectives[k];
        ob.live_ticks += 20;
        ob.held_now = held;
        if held {
            ob.held_ticks += 20;
            if !w.outcome.taken[k] {
                w.outcome.taken[k] = true;
                w.outcome
                    .notable(w.tick, NotableKind::ObjectiveTaken, o.pos, k as u32);
            }
        }
    }
}

pub fn outcome(w: &World) -> RiotOutcome {
    let a = &w.outcome;
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    let mut per = Vec::with_capacity(w.setup.objectives.len());
    for (k, _) in w.setup.objectives.iter().enumerate().take(MAX_OBJECTIVES) {
        let o = &w.objectives[k];
        let frac = if o.live_ticks > 0 {
            o.held_ticks as f32 / o.live_ticks as f32
        } else {
            0.0
        };
        per.push(frac);
        num += o.weight * frac;
        den += o.weight;
    }
    let mut morale = 0.0f64;
    let mut n = 0u32;
    for i in 0..w.people.len {
        let st = w.people.stat[i];
        if st[stat::FLAGS] & flag::DEAD != 0 || Archetype::from_u8(st[stat::ARCH]).is_officer() {
            continue;
        }
        morale += widen_u8(w.people.emo[i][emo::MORALE]) as f64;
        n += 1;
    }
    RiotOutcome {
        ticks: w.tick,
        ground_held: if den > 0.0 { num / den } else { 0.0 },
        arrests: a.arrests,
        injuries: a.injuries,
        deaths: a.deaths,
        supplies_used: a.supplies_used,
        footage: a.footage,
        final_morale: if n > 0 {
            (morale / n as f64) as f32
        } else {
            0.0
        },
        notable: a.notable.clone(),
        turnout: a.turnout,
        peak_crowd: a.peak_crowd,
        went_home: a.went_home,
        officers_injured: a.officers_injured,
        max_escalation: w.police.max_reached,
        objective_held: per,
        state_hash: w.state_hash(),
    }
}
