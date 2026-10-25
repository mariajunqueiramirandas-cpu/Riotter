//! Phase 11a: outcome accounting (DESIGN §2.7, §17). A riot never just wins
//! or loses: it ends with ground held, arrests, injuries, footage, morale
//! and supplies used, and each matters separately.
//!
//! The tallies themselves are [`riot_world::outcome`]; the render snapshot
//! written in the same phase is [`snapshot_write`].

#![deny(unsafe_code)]

pub mod snapshot_write;

use riot_core::math::widen_u8;

use riot_defs::archetype::Archetype;
use riot_defs::setup::{NotableKind, RiotOutcome};
use riot_defs::state::{emo, flag, stat};
use riot_fields::Layer;
use riot_spatial::FIELD_CELL;
use riot_world::World;
use riot_world::world::MAX_OBJECTIVES;

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
        let rc = o.radius / FIELD_CELL;
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
