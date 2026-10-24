//! Phase 6: steering and facing (DESIGN §11.1). Reads positions, writes
//! velocities and headings, each agent only its own.

use rayon::prelude::*;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{heading_of, turn_toward, widen_u8};
use riot_core::rng::{Stream, roll, unit};
use riot_core::{Vec2, mix};

use crate::archetype::{ARCHETYPES, Archetype, ArchetypeParams};
use crate::fields::{Fields, Layer};
use crate::flow::{
    FlowSample, FlowSystem, GOAL_EXITS, GOAL_MEDIC, GOAL_WATER, NO_GOAL, sample_flow,
};
use crate::map::Map;
use crate::people::People;
use crate::spatial::Spatial;
use crate::state::{Action, Gait, aux, emo, flag, health, intent, pose, stat};
use crate::tunables::Tunables;
use crate::world::{Objective, SupplyDrop, field_cell_pos};

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

pub struct SteerCtx<'a> {
    pub map: &'a Map,
    pub flow: &'a FlowSystem,
    pub fields: &'a Fields,
    pub sp: &'a Spatial,
    pub arch: &'a [ArchetypeParams; ARCHETYPES],
    pub tun: &'a Tunables,
    pub objectives: &'a [Objective],
    pub drops: &'a [SupplyDrop],
    pub officer_targets: &'a [OfficerTarget],
    pub slot_of: &'a [u32],
    pub tick: u32,
    pub seed: u64,
}

/// A personal offset inside an objective's area, stable per person.
#[inline]
pub fn personal_spot(stable: u32, centre: Vec2, radius: f32) -> Vec2 {
    let h = mix(stable as u64 ^ 0x51ed_270b);
    let a = (h & 255) as u8;
    let r = ((h >> 8) & 0xffff) as f32 * (1.0 / 65536.0);
    // sqrt for an even spread over the disc.
    centre + Vec2::from_heading(a) * (radius * r.sqrt())
}

/// Steering. Avoidance reads neighbours' velocities from before this phase,
/// so the caller copies them into `old_vx`/`old_vy` (preallocated scratch).
pub fn steer(
    p: &mut People,
    old_vx: &[f32],
    old_vy: &[f32],
    records: &mut [Vec<u64>],
    cx: &SteerCtx,
) {
    let len = p.len;
    let chunks = p.chunks();
    let tun = cx.tun;
    let People {
        pos_x,
        pos_y,
        vel_x,
        vel_y,
        pose,
        emo,
        stat,
        intent,
        aux,
        stable_id,
        ..
    } = p;
    let px = lanes::flat(pos_x);
    let py = lanes::flat(pos_y);
    let (stat, emo, intent, aux, stable_id) = (&*stat, &*emo, &*intent, &*aux, &*stable_id);
    let vx = &mut lanes::flat_mut(vel_x)[..chunks * CHUNK_AGENTS];
    let vy = &mut lanes::flat_mut(vel_y)[..chunks * CHUNK_AGENTS];
    let pose = &mut pose[..chunks * CHUNK_AGENTS];
    vx.par_chunks_mut(CHUNK_AGENTS)
        .zip(vy.par_chunks_mut(CHUNK_AGENTS))
        .zip(pose.par_chunks_mut(CHUNK_AGENTS))
        .zip(records[..chunks].par_iter_mut())
        .enumerate()
        .for_each(|(c, (((vxc, vyc), posec), rec))| {
            let base = c * CHUNK_AGENTS;
            for &k in cx.sp.awake_in(c) {
                let k = k as usize;
                let i = base + k;
                if i >= len {
                    continue;
                }
                let st = stat[i];
                let fl = st[stat::FLAGS];
                if fl & flag::DEAD != 0 {
                    continue;
                }
                let gait = Gait::from_u8(posec[k][pose::GAIT]);
                if gait.is_down() {
                    vxc[k] *= 0.5;
                    vyc[k] *= 0.5;
                    continue;
                }
                let a = &cx.arch[st[stat::ARCH] as usize];
                let arch = Archetype::from_u8(st[stat::ARCH]);
                let pos = Vec2::new(px[i], py[i]);
                let vel = Vec2::new(old_vx[i], old_vy[i]);
                let e = emo[i];
                let fear = widen_u8(e[emo::FEAR]);
                let fatigue = widen_u8(e[emo::FATIGUE]);
                let hp = st[stat::HEALTH];
                let act = Action::from_u8(st[stat::ACTION]);
                let stable = stable_id[i];
                let target = aux::target(aux[i]);

                // Preferred speed = base × (1 - fatigue) × injury × panic.
                let injury = if hp < health::INJURED { 0.6 } else { 1.0 };
                let panic = fear > a.fear_threshold;
                let mut speed = a.walk * (1.0 - 0.6 * fatigue) * injury;
                let mut run = panic;

                let mut desired = Vec2::ZERO;
                let mut face: Option<Vec2> = None;
                let route = |g: u8, rec: &mut Vec<u64>, fallback: Vec2| -> Option<Vec2> {
                    if g == NO_GOAL {
                        return None;
                    }
                    match sample_flow(cx.flow, cx.map, g, pos, rec) {
                        FlowSample::Dir(d) => Some(d),
                        FlowSample::Arrived => None,
                        FlowSample::Missing => Some((fallback - pos).normalize_or_zero()),
                    }
                };

                match act {
                    Action::March | Action::Hold => {
                        let slot = intent::objective(intent[i]);
                        if let Some(o) = cx.objectives.get(slot as usize).filter(|o| o.active) {
                            let d2 = (o.pos - pos).len2();
                            let inside = d2 < o.radius * o.radius;
                            if !inside && let Some(d) = route(slot, rec, o.pos) {
                                desired = d * speed;
                            }
                            if inside || desired == Vec2::ZERO {
                                // In the area: drift to a personal spot.
                                let spot = personal_spot(stable, o.pos, o.radius * tun.hold_radius);
                                let to = spot - pos;
                                let dist = to.len();
                                if dist > 0.4 {
                                    desired = to * (speed.min(dist * 0.8) / dist);
                                }
                                if act == Action::Hold {
                                    let g = cx.fields.gradient(Layer::Threat, pos);
                                    face = Some(if g.len2() > 1e-6 { g } else { o.pos - pos });
                                } else {
                                    face = Some(o.pos - pos);
                                }
                            }
                        } else {
                            idle(stable, cx, &mut desired, speed * 0.3);
                        }
                    }
                    Action::Flee => {
                        run = true;
                        speed = a.run * (1.0 - 0.4 * fatigue) * injury;
                        let away = -(cx.fields.gradient(Layer::Threat, pos) * 2.0
                            + cx.fields.gradient(Layer::Gas, pos) * 4.0
                            + cx.fields.gradient(Layer::Fire, pos) * 4.0);
                        let away = away.normalize_or_zero();
                        let exit = route(GOAL_EXITS, rec, pos + away).unwrap_or(Vec2::ZERO);
                        // Get away from the danger first; take the way out
                        // only when it does not lead back into it.
                        let dir = if away == Vec2::ZERO {
                            exit
                        } else if exit.dot(away) < 0.0 {
                            away
                        } else {
                            away * 0.7 + exit
                        };
                        desired = dir.normalize_or_zero() * speed;
                    }
                    Action::GoHome => {
                        if let Some(d) = route(GOAL_EXITS, rec, pos) {
                            desired = d * speed;
                        }
                    }
                    Action::SeekSupply | Action::Help => {
                        let g = if act == Action::Help || target == 1 {
                            GOAL_MEDIC
                        } else {
                            GOAL_WATER
                        };
                        if act == Action::Help && target != aux::NO_TARGET && (target >> 23) == 0 {
                            // Walking to the injured person (`target` is a stable id).
                            let j = cx.slot_of.get(target as usize).copied().unwrap_or(u32::MAX);
                            if j != u32::MAX {
                                let q = Vec2::new(px[j as usize], py[j as usize]);
                                let to = q - pos;
                                if to.len2() > 0.36 {
                                    desired = to.normalize_or_zero() * speed;
                                }
                                face = Some(to);
                            }
                        } else {
                            let near =
                                nearest_drop(cx.drops, pos, if g == GOAL_MEDIC { 1 } else { 0 });
                            match near {
                                Some(d) if (d - pos).len2() < 225.0 => {
                                    let to = d - pos;
                                    if to.len2() > 1.0 {
                                        desired = to.normalize_or_zero() * speed;
                                    }
                                }
                                _ => {
                                    if let Some(d) = route(g, rec, near.unwrap_or(pos)) {
                                        desired = d * speed;
                                    }
                                }
                            }
                        }
                    }
                    Action::Film | Action::Throw => {
                        let subject = if target != aux::NO_TARGET {
                            if act == Action::Throw {
                                let j =
                                    cx.slot_of.get(target as usize).copied().unwrap_or(u32::MAX);
                                (j != u32::MAX).then(|| Vec2::new(px[j as usize], py[j as usize]))
                            } else {
                                Some(field_cell_pos(cx.fields, target))
                            }
                        } else {
                            None
                        };
                        if let Some(q) = subject {
                            let to = q - pos;
                            let d = to.len();
                            let want = if act == Action::Throw {
                                tun.throw_range * 0.6
                            } else {
                                9.0
                            };
                            if d > want + 2.0 {
                                desired = to * (speed / d);
                            } else if d < want - 3.0 && d > 0.01 {
                                desired = -to * (speed * 0.6 / d);
                            }
                            face = Some(to);
                        }
                    }
                    Action::Barricade => {
                        if target != aux::NO_TARGET {
                            let q = field_cell_pos(cx.fields, target);
                            let to = q - pos;
                            if to.len2() > 1.5 {
                                desired = to.normalize_or_zero() * speed;
                            }
                            face = Some(to);
                        }
                    }
                    Action::Rest | Action::Chant => {}
                    Action::Idle => idle(stable, cx, &mut desired, speed * 0.35),
                    Action::Formation | Action::Arrest | Action::Escort | Action::Strike => {
                        let ot = cx
                            .officer_targets
                            .get(stable as usize)
                            .copied()
                            .unwrap_or_default();
                        let to = ot.pos - pos;
                        let d = to.len();
                        if ot.run {
                            run = true;
                            speed = a.run * (1.0 - 0.4 * fatigue);
                        }
                        // The director keeps a route goal while the target is
                        // far or behind a wall.
                        if d > 2.0 && ot.goal != NO_GOAL {
                            if let Some(dir) = route(ot.goal, rec, ot.pos) {
                                desired = dir * speed;
                            }
                        } else if d > 0.25 {
                            desired = to * (speed.min(d * 1.5) / d);
                        }
                        face = Some(if d > 2.0 && act != Action::Strike {
                            to
                        } else {
                            ot.face - pos
                        });
                    }
                    Action::Break => {
                        run = true;
                        speed = a.run * (1.0 - 0.4 * fatigue);
                        let away = -(cx.fields.gradient(Layer::Anger, pos)
                            + cx.fields.gradient(Layer::Dens, pos) * 0.05);
                        let ot = cx
                            .officer_targets
                            .get(stable as usize)
                            .copied()
                            .unwrap_or_default();
                        // A broken officer heads back toward the line's
                        // side; a hurt one just gets out of reach.
                        let home = if hp < health::INJURED {
                            Vec2::ZERO
                        } else {
                            (ot.pos - pos).normalize_or_zero()
                        };
                        desired = (away.normalize_or_zero() + home).normalize_or_zero() * speed;
                    }
                    Action::Board => {}
                }
                if run {
                    speed = speed.max(a.run * 0.8);
                    desired = desired.clamp_len(a.run);
                }

                // Anticipatory avoidance over the neighbour list (time to
                // collision). The response grows as the collision nears but
                // stays bounded, and fades in a packed crowd, where people
                // stop anticipating and simply push.
                let mut avoid = Vec2::ZERO;
                let ri = a.radius;
                let mut touching = 0u32;
                for &j in cx.sp.neighbours(i) {
                    let j = j as usize;
                    let q = Vec2::new(px[j], py[j]);
                    let w = Vec2::new(old_vx[j], old_vy[j]);
                    let d = q - pos;
                    let rv = desired - w;
                    let rr = ri + cx.sp.radius[j];
                    if d.len2() < (rr + 0.05) * (rr + 0.05) {
                        touching += 1;
                    }
                    let ttc = time_to_collision(d, rv, rr);
                    if ttc > 0.0 && ttc < tun.avoid_horizon {
                        // Sideways from where the collision would happen.
                        let at = d - rv * ttc;
                        let x = 1.0 - ttc / tun.avoid_horizon;
                        avoid -= at.normalize_or_zero() * (tun.avoid_strength * x * x);
                    }
                }
                avoid = avoid.clamp_len(1.0) * (1.0 / (1.0 + touching as f32));
                // In a packed crowd people stop shoving toward a spot they
                // cannot reach; marchers keep pushing a little, the panicked
                // push hard (nobody can run in a crowd, but they try). That
                // is how crushes start.
                if touching >= 3 {
                    let patience = match act {
                        Action::Flee => {
                            desired = desired.clamp_len(a.walk);
                            1.0
                        }
                        Action::March | Action::GoHome | Action::SeekSupply | Action::Help => 0.6,
                        _ if arch.is_officer() => 0.8,
                        _ => 0.2,
                    };
                    desired = desired * patience;
                }
                // Walls ahead.
                let ahead = pos + desired * 0.4;
                for &s in cx.map.walls_near(ahead) {
                    let (sa, sb) = cx.map.segment(s);
                    let cp = riot_core::math::closest_on_segment(ahead, sa, sb);
                    let d = ahead - cp;
                    let dist = d.len();
                    let reach = ri + 0.35;
                    if dist < reach && dist > 1e-4 {
                        avoid += d * (tun.wall_avoid * (reach - dist) / (reach * dist));
                    }
                }
                if !arch.is_officer() && !matches!(act, Action::Hold | Action::March) {
                    avoid -= cx.fields.gradient(Layer::Dens, pos) * tun.density_avoid;
                }
                let target_v = (desired + avoid * speed.max(0.3)).clamp_len(a.run);
                let resp = a.response * if hp < health::INJURED { 0.6 } else { 1.0 };
                let nv = vel.lerp(target_v, resp).clamp_len(a.run);
                vxc[k] = nv.x;
                vyc[k] = nv.y;

                // Facing: toward motion, or toward what the person attends to.
                let h = posec[k][pose::HEADING];
                let turn = a.turn_rate;
                let want = if nv.len2() > 0.04 {
                    Some(heading_of(nv.x, nv.y))
                } else {
                    face.filter(|f| f.len2() > 1e-6)
                        .map(|f| heading_of(f.x, f.y))
                };
                if let Some(wh) = want {
                    posec[k][pose::HEADING] = turn_toward(h, wh, turn);
                }
            }
        });
}

#[inline]
fn idle(stable: u32, cx: &SteerCtx, desired: &mut Vec2, speed: f32) {
    // Change direction every few seconds; stand still half of the time.
    let epoch = (cx.tick + stable % 97) / 80;
    let r = roll(cx.seed, epoch, stable, Stream::Think);
    if r & 1 == 0 {
        *desired = Vec2::from_heading((r >> 8) as u8) * (speed * (0.5 + 0.5 * unit(r)));
    }
}

#[inline]
fn nearest_drop(drops: &[SupplyDrop], pos: Vec2, kind: u8) -> Option<Vec2> {
    let mut best: Option<(f32, Vec2)> = None;
    for d in drops {
        if d.amount == 0 || d.kind as u8 != kind {
            continue;
        }
        let d2 = (d.pos - pos).len2();
        if best.is_none_or(|b| d2 < b.0) {
            best = Some((d2, d.pos));
        }
    }
    best.map(|b| b.1)
}

/// Time until two discs with centre offset `d`, relative velocity `v`
/// (closing when positive along `d`) and summed radius `r` touch; negative
/// when they never do or already overlap.
#[inline]
pub fn time_to_collision(d: Vec2, v: Vec2, r: f32) -> f32 {
    let a = v.len2();
    let b = d.dot(v);
    let c = d.len2() - r * r;
    if c < 0.0 {
        return -1.0;
    }
    if a < 1e-8 || b <= 0.0 {
        return -1.0;
    }
    let disc = b * b - a * c;
    if disc <= 0.0 {
        return -1.0;
    }
    (b - disc.sqrt()) / a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttc() {
        // Head-on at 2 m/s closing, 3 m apart, radii sum 0.5: touch after 1.25 s.
        let t = time_to_collision(Vec2::new(3.0, 0.0), Vec2::new(2.0, 0.0), 0.5);
        assert!((t - 1.25).abs() < 1e-5, "{t}");
        assert!(time_to_collision(Vec2::new(3.0, 0.0), Vec2::new(-2.0, 0.0), 0.5) < 0.0);
        assert!(time_to_collision(Vec2::new(3.0, 2.0), Vec2::new(2.0, 0.0), 0.5) < 0.0);
    }

    #[test]
    fn spots_spread_inside() {
        for s in 0..200 {
            let p = personal_spot(s, Vec2::new(10.0, -5.0), 7.0);
            assert!((p - Vec2::new(10.0, -5.0)).len() <= 7.0 + 1e-4);
        }
    }
}
