//! Phase 4: minds (DESIGN §10).
//!
//! - **Staggered thinking**: each agent rethinks every K ticks, at phase
//!   `mix(stable_id) % K`, so each tick handles 1/K of the crowd. Emergencies
//!   (gassed, crushed, hit, a new message) skip the queue.
//! - **People see what they face**: threat is sampled at points in a field of
//!   view around `heading`, wide when calm and narrow when panicked.
//! - **Utility AI**: each action's score is a product of considerations, each
//!   a 256-entry lookup table indexed by a quantized `u8` input, times an
//!   archetype weight, with deterministic noise.
//! - **Action-major batches**: the agents thinking this tick (per chunk) are
//!   gathered into a scratch SoA and scored one action at a time.
//!
//! The lookup tables are built with the world ([`riot_world::minds`]).

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{narrow_u8, widen_u8};
use riot_core::rng::{Stream, roll, unit};
use riot_team::prelude::*;

use riot_defs::archetype::{ARCHETYPES, Archetype, ArchetypeParams, Stance};
use riot_defs::police::Doctrine;
use riot_defs::setup::{Escalation, ObjectiveKind};
use riot_defs::state::{
    Action, CIVILIAN_ACTIONS, Gait, Item, aux, emo, flag, health, intent, pose, stat,
};
use riot_defs::targets::{Objective, SupplyDrop};
use riot_defs::tunables::Tunables;
use riot_fields::{Fields, Layer, field_cell_of};
use riot_people::{Cold, People};
use riot_spatial::Spatial;
use riot_world::World;
use riot_world::minds::{INPUTS, Minds, due};
use riot_world::police::Police;

struct ThinkCtx<'a> {
    minds: &'a Minds,
    fields: &'a Fields,
    sp: &'a Spatial,
    arch: &'a [ArchetypeParams; ARCHETYPES],
    tun: &'a Tunables,
    objectives: &'a [Objective],
    drops: &'a [SupplyDrop],
    cold: &'a [Cold],
    police: &'a Police,
    slot_of: &'a [u32],
    chanting: &'a [u32],
    seed: u64,
    tick: u32,
    k: u32,
    wake_k: u32,
    late: u8,
    escalation: Escalation,
    px: &'a [f32],
    py: &'a [f32],
    pose: &'a [[u8; 4]],
    emo_all: &'a [[u8; 4]],
    /// Everyone's stat bytes from before this phase, for reading others.
    stat_ro: &'a [[u8; 4]],
    stable: &'a [u32],
}

/// Phase 4.
pub fn think(w: &mut World) {
    let len = w.people.len;
    let chunks = w.people.chunks();
    let span = chunks * CHUNK_AGENTS;
    let k = w.tun.think_interval.max(1.0) as u32;
    let wake_k = w.tun.wake_interval.max(k as f32) as u32;
    let late = narrow_u8(w.tick as f32 / w.setup.duration.max(1) as f32);
    w.stat_scratch[..span].copy_from_slice(&w.people.stat[..span]);
    let People {
        pos_x,
        pos_y,
        pose,
        emo,
        stat,
        intent,
        aux,
        stable_id,
        ..
    } = &mut w.people;
    let cx = ThinkCtx {
        minds: &w.minds,
        fields: &w.fields,
        sp: &w.spatial,
        arch: &w.arch,
        tun: &w.tun,
        objectives: &w.objectives,
        drops: &w.drops,
        cold: &w.cold,
        police: &w.police,
        slot_of: &w.slot_of,
        chanting: &w.msgs.chanting,
        seed: w.seed,
        tick: w.tick,
        k,
        wake_k,
        late,
        escalation: w.police.escalation,
        px: lanes::flat(pos_x),
        py: lanes::flat(pos_y),
        pose,
        emo_all: emo,
        stat_ro: &w.stat_scratch[..span],
        stable: stable_id,
    };
    let thought: u64 = stat[..span]
        .par_chunks_mut(CHUNK_AGENTS)
        .zip(aux[..span].par_chunks_mut(CHUNK_AGENTS))
        .zip(intent[..span].par_chunks(CHUNK_AGENTS))
        .enumerate()
        .map(|(c, ((ss, ax), is))| {
            let base = c * CHUNK_AGENTS;
            think_chunk(
                &cx,
                base,
                len,
                ss,
                ax,
                is,
                &cx.stable[base..base + CHUNK_AGENTS],
            )
        })
        .sum();
    w.minds.thought += thought;
}

#[allow(clippy::too_many_arguments)]
fn think_chunk(
    cx: &ThinkCtx,
    base: usize,
    len: usize,
    ss: &mut [[u8; 4]],
    ax: &mut [u32],
    is: &[u16],
    ids: &[u32],
) -> u64 {
    // Gather the batch.
    let mut batch = [0u16; CHUNK_AGENTS];
    let mut n = 0usize;
    let mut officers = [0u16; CHUNK_AGENTS];
    let mut no = 0usize;
    for k in 0..ss.len() {
        let i = base + k;
        if i >= len {
            break;
        }
        let fl = ss[k][stat::FLAGS];
        if fl & flag::DEAD != 0 {
            continue;
        }
        let gait = Gait::from_u8(cx.pose[i][pose::GAIT]);
        if gait.is_down() {
            ss[k][stat::FLAGS] &= !flag::URGENT;
            continue;
        }
        let asleep = fl & flag::ASLEEP != 0;
        let is_due = if asleep {
            due(ids[k], cx.tick, cx.wake_k)
        } else {
            fl & flag::URGENT != 0 || due(ids[k], cx.tick, cx.k)
        };
        if !is_due {
            continue;
        }
        if is_officer_stat(ss[k]) {
            officers[no] = k as u16;
            no += 1;
        } else {
            batch[n] = k as u16;
            n += 1;
        }
    }
    for &k in &officers[..no] {
        think_officer(
            cx,
            base + k as usize,
            &mut ss[k as usize],
            &mut ax[k as usize],
            ids[k as usize],
        );
    }
    if n == 0 {
        return no as u64;
    }

    // Inputs (SoA over the batch).
    let mut inputs = [[0u8; CHUNK_AGENTS]; INPUTS];
    for b in 0..n {
        let k = batch[b] as usize;
        let i = base + k;
        let st = ss[k];
        let e = cx.emo_all[i];
        let a = Archetype::from_u8(st[stat::ARCH]);
        let ap = &cx.arch[a as usize];
        let pos = Vec2::new(cx.px[i], cx.py[i]);
        let heading = cx.pose[i][pose::HEADING];
        let fear = widen_u8(e[emo::FEAR]);
        // Field of view: wide when calm, narrow when panicked.
        let half = if fear > 0.6 { 24 } else { 56 };
        let mut seen = 0.0f32;
        for (h, d) in [(0i32, 8.0f32), (half, 7.0), (-half, 7.0), (0, 18.0)] {
            let dir = Vec2::from_heading((heading as i32 + h) as u8);
            seen = seen.max(
                cx.fields
                    .sample(Layer::Threat, pos.x + dir.x * d, pos.y + dir.y * d),
            );
        }
        // What is behind reaches them only through the fields at their position.
        seen = seen.max(cx.fields.sample(Layer::Threat, pos.x, pos.y) * 0.5);
        let gas = cx.fields.sample(Layer::Gas, pos.x, pos.y)
            + cx.fields.sample(Layer::Fire, pos.x, pos.y);
        let dens = cx.fields.sample(Layer::Dens, pos.x, pos.y) * 0.25;
        let it = is[k];
        let slot = intent::objective(it);
        let obj = cx.objectives.get(slot as usize).filter(|o| o.active);
        let (gd, holdgoal) = match obj {
            Some(o) => {
                let d = (o.pos - pos).len();
                (
                    narrow_u8(((d - o.radius) / 300.0).max(0.0)),
                    if o.kind == ObjectiveKind::Hold {
                        255
                    } else {
                        0
                    },
                )
            }
            None => (255, 0),
        };
        // The plan's stance weighed against the archetype's own by trust in
        // organizers, then pushed by anger (DESIGN §2.4).
        let level = |s: Stance| s as u8 as f32 * 0.5;
        let own = level(ap.default_stance);
        let stance = match obj {
            Some(_) => ap.trust * level(intent::stance(it)) + (1.0 - ap.trust) * own,
            None => own,
        };
        let anger = widen_u8(e[emo::ANGER]);
        let stance = narrow_u8((stance + (anger - 0.5) * 0.5).clamp(0.0, 1.0));
        // Provocateurs act enraged whatever they feel: that is their job.
        let anger_in = if a == Archetype::Provocateur {
            e[emo::ANGER].max(230)
        } else {
            e[emo::ANGER]
        };
        let item = Item::from_u8(cx.cold[ids[k] as usize].item);
        let armed = matches!(item, Item::Bottle | Item::Molotov) || a == Archetype::Radical;
        let supply = cx
            .drops
            .iter()
            .any(|d| d.amount > 0 && (d.pos - pos).len2() < 250_000.0);
        let chant_asked = a == Archetype::Organizer && cx.chanting.contains(&ids[k]);
        let hp = st[stat::HEALTH];
        let inj_near = cx.fields.sample(Layer::Injured, pos.x, pos.y);
        let row = [
            e[emo::FEAR],
            anger_in,
            e[emo::MORALE],
            e[emo::FATIGUE],
            255 - hp,
            narrow_u8(seen * 0.5),
            narrow_u8(gas),
            narrow_u8(dens * 0.25),
            gd,
            if obj.is_some() { 255 } else { 0 },
            stance,
            narrow_u8(cx.fields.sample(Layer::Camera, pos.x, pos.y) * 0.25),
            narrow_u8(inj_near * 0.5),
            if supply { 255 } else { 0 },
            if armed { 255 } else { 0 },
            narrow_u8(ap.violence),
            holdgoal,
            if chant_asked { 255 } else { 0 },
            cx.late,
        ];
        for (c, v) in row.into_iter().enumerate() {
            inputs[c][b] = v;
        }
    }

    let mut noise = [0u64; CHUNK_AGENTS];
    for b in 0..n {
        noise[b] = roll(cx.seed, cx.tick, ids[batch[b] as usize], Stream::Think);
    }
    // Score, one action at a time across the batch.
    let mut best = [0u8; CHUNK_AGENTS];
    let mut best_s = [f32::NEG_INFINITY; CHUNK_AGENTS];
    let mut second = [0u8; CHUNK_AGENTS];
    let mut second_s = [f32::NEG_INFINITY; CHUNK_AGENTS];
    let mut score = [0.0f32; CHUNK_AGENTS];
    for act in 0..CIVILIAN_ACTIONS {
        for b in 0..n {
            let a = ss[batch[b] as usize][stat::ARCH] as usize;
            score[b] = cx.minds.weights[a][act];
        }
        let m = &cx.minds;
        for c in 0..m.lut_n[act] as usize {
            let lut = &m.luts[act][c];
            let inp = &inputs[m.lut_input[act][c] as usize];
            for b in 0..n {
                score[b] *= lut[inp[b] as usize];
            }
        }
        for b in 0..n {
            let k = batch[b] as usize;
            let cur = ss[k][stat::ACTION] as usize;
            // Five bits of one roll per action: deterministic noise.
            let bits = (noise[b] >> (act * 5)) & 31;
            let mut sc = score[b] * (0.85 + 0.3 * (bits as f32 * (1.0 / 31.0)));
            if cur == act {
                sc *= 1.25;
            }
            if sc > best_s[b] {
                second[b] = best[b];
                second_s[b] = best_s[b];
                best[b] = act as u8;
                best_s[b] = sc;
            } else if sc > second_s[b] {
                second[b] = act as u8;
                second_s[b] = sc;
            }
        }
    }

    // Targets for actions that need one; fall back to the runner-up.
    for b in 0..n {
        let k = batch[b] as usize;
        let i = base + k;
        let pos = Vec2::new(cx.px[i], cx.py[i]);
        let mut act = Action::from_u8(best[b]);
        let mut target = aux::NO_TARGET;
        for attempt in 0..2 {
            target = aux::NO_TARGET;
            let ok = match act {
                Action::Throw => match nearest_officer(cx, pos, cx.tun.throw_range) {
                    Some(s) => {
                        target = s;
                        true
                    }
                    None => false,
                },
                Action::Help => {
                    let cur = Action::from_u8(ss[k][stat::ACTION]);
                    let keep = aux::target(ax[k]);
                    if cur == Action::Help && keep != aux::NO_TARGET {
                        target = keep;
                        true
                    } else {
                        match injured_near(cx, i, pos) {
                            Some(s) => {
                                target = s;
                                true
                            }
                            None => false,
                        }
                    }
                }
                Action::Film => {
                    let p = film_subject(cx, pos, cx.pose[i][pose::HEADING]);
                    target = field_cell_of(cx.fields, p);
                    true
                }
                Action::Barricade => {
                    let ahead = pos + Vec2::from_heading(cx.pose[i][pose::HEADING]) * 1.5;
                    target = field_cell_of(cx.fields, ahead);
                    true
                }
                Action::SeekSupply => {
                    // 1 = medic when hurt, else water.
                    target = if ss[k][stat::HEALTH] < health::INJURED {
                        1
                    } else {
                        0
                    };
                    true
                }
                Action::Chant => Archetype::from_u8(ss[k][stat::ARCH]) == Archetype::Organizer,
                _ => true,
            };
            if ok {
                break;
            }
            act = if attempt == 0 {
                Action::from_u8(second[b])
            } else {
                Action::Idle
            };
        }
        if Action::from_u8(ss[k][stat::ACTION]) != act {
            ss[k][stat::FLAGS] &= !flag::ASLEEP;
        }
        ss[k][stat::ACTION] = act as u8;
        ss[k][stat::FLAGS] &= !flag::URGENT;
        ax[k] = aux::with_target(ax[k], target);
    }
    (n + no) as u64
}

#[inline]
fn is_officer_stat(st: [u8; 4]) -> bool {
    Archetype::from_u8(st[stat::ARCH]).is_officer()
}

/// The officer to throw at: the nearest one in range, though a shield
/// turned this way makes an officer look half as near a target as one
/// side-on or turned away. Officers hiding in ambush cannot be seen.
fn nearest_officer(cx: &ThinkCtx, pos: Vec2, range: f32) -> Option<u32> {
    let mut best: Option<(f32, u32)> = None;
    for &s in &cx.police.on_map {
        let j = cx.slot_of[s as usize];
        if j == u32::MAX {
            continue;
        }
        let st = cx.stat_ro[j as usize];
        if st[stat::FLAGS] & flag::DOWN != 0 || st[stat::ACTION] == Action::Lurk as u8 {
            continue;
        }
        let q = Vec2::new(cx.px[j as usize], cx.py[j as usize]);
        let d2 = (q - pos).len2();
        if d2 >= range * range {
            continue;
        }
        let shielded = st[stat::FLAGS] & flag::SHIELD != 0
            && Vec2::from_heading(cx.pose[j as usize][pose::HEADING]).dot(pos - q)
                > 0.3 * d2.sqrt();
        let score = if shielded { d2 * 4.0 } else { d2 };
        if best.is_none_or(|b| score < b.0) {
            best = Some((score, s));
        }
    }
    best.map(|b| b.1)
}

fn injured_near(cx: &ThinkCtx, me: usize, pos: Vec2) -> Option<u32> {
    let mut best: Option<(f32, u32)> = None;
    cx.sp.query_radius(cx.px, cx.py, pos.x, pos.y, 6.0, |j| {
        if j == me {
            return;
        }
        let st = cx.stat_ro[j];
        if is_officer_stat(st) || st[stat::FLAGS] & flag::DEAD != 0 {
            return;
        }
        let down = Gait::from_u8(cx.pose[j][pose::GAIT]) == Gait::Fallen;
        if !(down || st[stat::HEALTH] < health::INCAPACITATED) {
            return;
        }
        let d2 = (Vec2::new(cx.px[j], cx.py[j]) - pos).len2();
        if best.is_none_or(|b| d2 < b.0) {
            best = Some((d2, j as u32));
        }
    });
    // Targets are stable ids: dense indices change on reorder.
    best.map(|(_, j)| cx.stable[j as usize])
}

/// Where a filmer points the camera: the most threatening spot in view.
fn film_subject(cx: &ThinkCtx, pos: Vec2, heading: u8) -> Vec2 {
    let mut best = pos + Vec2::from_heading(heading) * 10.0;
    let mut bv = -1.0f32;
    for h in [-40i32, -20, 0, 20, 40] {
        let d = Vec2::from_heading((heading as i32 + h) as u8);
        for dist in [8.0f32, 16.0] {
            let p = pos + d * dist;
            let v = cx.fields.sample(Layer::Threat, p.x, p.y)
                + cx.fields.sample(Layer::Anger, p.x, p.y);
            if v > bv {
                bv = v;
                best = p;
            }
        }
    }
    best
}

/// Officer minds (DESIGN §14): stay in formation or break, strike, arrest.
fn think_officer(cx: &ThinkCtx, i: usize, st: &mut [u8; 4], ax: &mut u32, stable: u32) {
    st[stat::FLAGS] &= !flag::URGENT;
    let e = cx.emo_all[i];
    let fear = widen_u8(e[emo::FEAR]);
    let fatigue = widen_u8(e[emo::FATIGUE]);
    let anger = widen_u8(e[emo::ANGER]);
    let Some(info) = cx.police.officer(stable) else {
        return;
    };
    let squad = &cx.police.squads[info.squad as usize];
    let mut act = Action::Formation;
    let mut target = aux::NO_TARGET;
    let cur = Action::from_u8(st[stat::ACTION]);
    let grabbing = aux::target(*ax);
    if cx.police.escorting.contains(stable) {
        act = Action::Escort;
    } else if st[stat::HEALTH] < health::INJURED
        || fear > cx.tun.officer_break_fear && (squad.morale < 0.45 || fatigue > 0.8)
    {
        // Hurt officers fall back out of the line.
        act = Action::Break;
    } else if squad.doctrine == Doctrine::Snatch && squad.target_person != u32::MAX {
        act = Action::Arrest;
        target = squad.target_person;
    } else if squad.doctrine == Doctrine::Ambush {
        // Crouched around the corner, silent.
        act = Action::Lurk;
    } else if cur == Action::Arrest
        && grabbing != aux::NO_TARGET
        && squad.doctrine != Doctrine::Snatch
        && cx.slot_of.get(grabbing as usize).is_some_and(|&j| {
            j != u32::MAX
                && Gait::from_u8(cx.pose[j as usize][pose::GAIT]) != Gait::Restrained
                && (Vec2::new(cx.px[j as usize], cx.py[j as usize]) - Vec2::new(cx.px[i], cx.py[i]))
                    .len2()
                    < 36.0
        })
    {
        // Hold on to whoever this officer went for in the charge.
        act = Action::Arrest;
        target = grabbing;
    } else if cx.escalation >= Escalation::BatonsAndArrests
        && matches!(
            squad.doctrine,
            Doctrine::Advance
                | Doctrine::Disperse
                | Doctrine::Kettle
                | Doctrine::HoldLine
                | Doctrine::Barricade
                | Doctrine::Charge
        )
    {
        // Strike an adjacent civilian in front, more readily when angry or
        // when the area is violent; a charge hits whoever is in reach and
        // grabs the people the police have identified.
        let pos = Vec2::new(cx.px[i], cx.py[i]);
        let fwd = Vec2::from_heading(cx.pose[i][pose::HEADING]);
        let threat = cx.fields.sample(Layer::Threat, pos.x, pos.y);
        let r = unit(roll(cx.seed, cx.tick, stable, Stream::Police));
        let charging = squad.doctrine == Doctrine::Charge;
        let eager = 0.15
            + anger * 0.6
            + (threat * 0.05).min(0.4)
            + match squad.doctrine {
                Doctrine::Disperse => 0.3,
                Doctrine::Charge => 0.5,
                _ => 0.0,
            };
        let arc = if charging { -0.2 } else { 0.3 };
        if r < eager {
            for &j in cx.sp.neighbours(i) {
                let j = j as usize;
                // Not someone already on the ground.
                let sj = cx.stat_ro[j];
                if is_officer_stat(sj) || sj[stat::FLAGS] & flag::DOWN != 0 {
                    continue;
                }
                let d = Vec2::new(cx.px[j], cx.py[j]) - pos;
                if d.normalize_or_zero().dot(fwd) > arc {
                    let known = sj[stat::FLAGS] & flag::KNOWN != 0
                        && Archetype::from_u8(sj[stat::ARCH]) != Archetype::Provocateur
                        && !cx.police.arrests.contains(cx.stable[j]);
                    // A charge grabs the people the police identified; a
                    // line pulls one in now and then.
                    let grab = known
                        && (charging
                            || unit(roll(cx.seed, cx.tick, stable ^ 0x5a5a, Stream::Police))
                                < 0.35);
                    act = if grab { Action::Arrest } else { Action::Strike };
                    target = cx.stable[j];
                    break;
                }
            }
        }
    }
    st[stat::ACTION] = act as u8;
    *ax = aux::with_target(*ax, target);
}
