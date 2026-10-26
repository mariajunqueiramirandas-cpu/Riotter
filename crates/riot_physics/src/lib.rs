//! Phase 7: bodies (DESIGN §11.2) — predict, contacts, walls, props and
//! vehicles, velocity from movement, pressure and pose.
//!
//! Contacts are position-based constraints solved with Jacobi iterations
//! (Weiss et al., "Position-Based Real-Time Simulation of Large Crowds"):
//! each agent reads every neighbour's position from the previous iteration
//! and writes only its own into a second buffer. Unlike Gauss-Seidel this
//! keeps the single-writer rule, and results do not depend on order or
//! thread count.

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{closest_on_segment, narrow_u8, segment_hit, widen_u8};
use riot_team::prelude::*;

use riot_defs::archetype::{ARCHETYPES, Archetype, ArchetypeParams};
use riot_defs::event::Event;
use riot_defs::setup::DT;
use riot_defs::state::{Action, Gait, aux, emo, flag, pose, stat};
use riot_defs::tunables::Tunables;
use riot_map::Map;
use riot_people::People;
use riot_spatial::Spatial;

pub use riot_spatial::obstacles::{Obb, ObstacleGrid};

/// Preallocated buffers for the body phase.
pub struct PhysScratch {
    pub qx: Vec<f32>,
    pub qy: Vec<f32>,
    pub x0: Vec<f32>,
    pub y0: Vec<f32>,
    pub press: Vec<f32>,
    /// Per contact iteration (by parity): did the agent touch someone or
    /// move? Iterations after the first recompute only agents for which
    /// it, or a neighbour's, was set in the last one.
    pub busy: [Vec<u8>; 2],
    /// Per chunk, the agents that can move after the first contact
    /// iteration (local indices): awake, with neighbours or moved.
    pub cand: Vec<u16>,
    pub cand_n: Vec<u16>,
    /// Peak pressure of the last tick (0..1): raises iterations when high.
    pub peak: f32,
}

impl PhysScratch {
    pub fn new(cap: usize) -> Self {
        PhysScratch {
            qx: vec![0.0; cap],
            qy: vec![0.0; cap],
            x0: vec![0.0; cap],
            y0: vec![0.0; cap],
            press: vec![0.0; cap],
            busy: [vec![0; cap], vec![0; cap]],
            cand: vec![0; cap],
            cand_n: vec![0; cap.div_ceil(CHUNK_AGENTS)],
            peak: 0.0,
        }
    }
}

pub struct BodyCtx<'a> {
    pub map: &'a Map,
    pub sp: &'a Spatial,
    pub arch: &'a [ArchetypeParams; ARCHETYPES],
    pub tun: &'a Tunables,
    pub obbs: &'a [Obb],
    pub grid: &'a ObstacleGrid,
    pub veh_obbs: &'a [Obb],
    pub veh_grid: &'a ObstacleGrid,
}

/// Run the whole body phase.
pub fn move_bodies(p: &mut People, s: &mut PhysScratch, events: &mut [Vec<Event>], cx: &BodyCtx) {
    let len = p.len;
    let chunks = p.chunks();
    let span = chunks * CHUNK_AGENTS;
    let tun = cx.tun;
    let sp = cx.sp;

    // 1. Predict: pos* = pos + vel·dt, remembering where everyone started.
    {
        let px = &mut lanes::flat_mut(&mut p.pos_x)[..span];
        let py = &mut lanes::flat_mut(&mut p.pos_y)[..span];
        let vx = &lanes::flat(&p.vel_x)[..span];
        let vy = &lanes::flat(&p.vel_y)[..span];
        let (x0, y0) = (&mut s.x0[..span], &mut s.y0[..span]);
        let press = &mut s.press[..span];
        px.par_chunks_mut(CHUNK_AGENTS)
            .zip(py.par_chunks_mut(CHUNK_AGENTS))
            .zip(vx.par_chunks(CHUNK_AGENTS).zip(vy.par_chunks(CHUNK_AGENTS)))
            .zip(
                x0.par_chunks_mut(CHUNK_AGENTS)
                    .zip(y0.par_chunks_mut(CHUNK_AGENTS)),
            )
            .zip(press.par_chunks_mut(CHUNK_AGENTS))
            .for_each(|((((px, py), (vx, vy)), (x0, y0)), pr)| {
                assert_eq!(px.len(), vx.len());
                for l in 0..px.len() {
                    x0[l] = px[l];
                    y0[l] = py[l];
                    px[l] += vx[l] * DT;
                    py[l] += vy[l] * DT;
                    pr[l] = 0.0;
                }
            });
    }

    // 2. Contacts: Jacobi iterations, always an even number so the result
    // ends up back in the agent arrays.
    let iters = if s.peak > tun.hot_pressure {
        tun.contact_iters_hot
    } else {
        tun.contact_iters
    };
    let iters = ((iters.max(2.0) as u32).div_ceil(2) * 2).min(8);
    let relax = tun.contact_relax;
    let ctx = ContactCtx {
        sp,
        len,
        relax,
        wake_push: tun.wake_push,
    };
    for it in 0..iters {
        let (src_x, src_y, dst_x, dst_y): (&[f32], &[f32], &mut [f32], &mut [f32]) = if it % 2 == 0
        {
            (
                lanes::flat(&p.pos_x),
                lanes::flat(&p.pos_y),
                &mut s.qx[..],
                &mut s.qy[..],
            )
        } else {
            (
                &s.qx[..],
                &s.qy[..],
                lanes::flat_mut(&mut p.pos_x),
                lanes::flat_mut(&mut p.pos_y),
            )
        };
        // Borrow juggling: when writing into the agent arrays we read the
        // scratch, and vice versa; the two never alias.
        let press = &mut s.press[..span];
        let dst = dst_x[..span]
            .par_chunks_mut(CHUNK_AGENTS)
            .zip(dst_y[..span].par_chunks_mut(CHUNK_AGENTS))
            .zip(press.par_chunks_mut(CHUNK_AGENTS));
        let [busy0, busy1] = &mut s.busy;
        if it == 0 {
            dst.zip(events[..chunks].par_iter_mut())
                .zip(busy0[..span].par_chunks_mut(CHUNK_AGENTS))
                .zip(busy1[..span].par_chunks_mut(CHUNK_AGENTS))
                .zip(
                    s.cand[..span]
                        .par_chunks_mut(CHUNK_AGENTS)
                        .zip(s.cand_n[..chunks].par_iter_mut()),
                )
                .enumerate()
                .for_each(|(c, ((((((dx, dy), pr), ev), busy), next), (cand, n)))| {
                    let base = c * CHUNK_AGENTS;
                    let src = |j: usize| (src_x[j], src_y[j]);
                    *n = first_iteration(&ctx, base, src, dx, dy, pr, ev, busy, cand);
                    next.fill(0);
                });
        } else {
            let (prev, busy) = if it % 2 == 1 {
                (&busy0[..], &mut busy1[..span])
            } else {
                (&busy1[..], &mut busy0[..span])
            };
            dst.zip(busy.par_chunks_mut(CHUNK_AGENTS))
                .zip(
                    s.cand[..span]
                        .par_chunks(CHUNK_AGENTS)
                        .zip(s.cand_n[..chunks].par_iter()),
                )
                .enumerate()
                .for_each(|(c, ((((dx, dy), pr), busy), (cand, &n)))| {
                    let base = c * CHUNK_AGENTS;
                    let src = |j: usize| (src_x[j], src_y[j]);
                    let cand = &cand[..n as usize];
                    later_iteration(&ctx, base, src, prev, cand, dx, dy, pr, busy);
                });
        }
    }

    // 3. Walls, props and vehicles; 4. velocity; 5. pressure; 6. pose.
    let People {
        pos_x,
        pos_y,
        vel_x,
        vel_y,
        pose,
        stat,
        emo,
        aux,
        ..
    } = p;
    let px = &mut lanes::flat_mut(pos_x)[..span];
    let py = &mut lanes::flat_mut(pos_y)[..span];
    let vx = &mut lanes::flat_mut(vel_x)[..span];
    let vy = &mut lanes::flat_mut(vel_y)[..span];
    let pose = &mut pose[..span];
    let stat = &mut stat[..span];
    let aux = &mut aux[..span];
    let emo = &emo[..span];
    let (x0, y0) = (&s.x0[..span], &s.y0[..span]);
    let press = &s.press[..span];
    let pscale = tun.pressure_scale;
    let inv_iters = 1.0 / iters as f32;
    // Walking into a standing crowd overlaps by about one step a tick: that
    // is pushing, not pressure. Only compression beyond it counts.
    let baseline = tun.pressure_baseline;
    let smooth = tun.pressure_smoothing.clamp(0.0, 1.0);
    let (sleep_speed, sleep_ticks) = (tun.sleep_speed, tun.sleep_ticks.clamp(1.0, 255.0) as u8);
    s.peak = px
        .par_chunks_mut(CHUNK_AGENTS)
        .zip(py.par_chunks_mut(CHUNK_AGENTS))
        .zip(
            vx.par_chunks_mut(CHUNK_AGENTS)
                .zip(vy.par_chunks_mut(CHUNK_AGENTS)),
        )
        .zip(
            pose.par_chunks_mut(CHUNK_AGENTS)
                .zip(stat.par_chunks_mut(CHUNK_AGENTS)),
        )
        .zip(aux.par_chunks_mut(CHUNK_AGENTS))
        .enumerate()
        .map(|(c, ((((px, py), (vx, vy)), (pose, stat)), aux))| {
            let base = c * CHUNK_AGENTS;
            let n = px.len();
            // This chunk's part of every array, indexed by `k`.
            let (py, vx, vy) = (&mut py[..n], &mut vx[..n], &mut vy[..n]);
            let (pose, stat, aux) = (&mut pose[..n], &mut stat[..n], &mut aux[..n]);
            let (x0, y0) = (&x0[base..][..n], &y0[base..][..n]);
            let (press, emo) = (&press[base..][..n], &emo[base..][..n]);
            let mut peak = 0.0f32;
            for k in 0..len.saturating_sub(base).min(n) {
                let fl = stat[k][stat::FLAGS];
                if fl & flag::DEAD != 0 {
                    continue;
                }
                if fl & flag::ASLEEP != 0 {
                    vx[k] = 0.0;
                    vy[k] = 0.0;
                    continue;
                }
                let a = &cx.arch[stat[k][stat::ARCH] as usize];
                let r = a.radius;
                let start = Vec2::new(x0[k], y0[k]);
                let mut q = Vec2::new(px[k], py[k]);
                let mut wall_push = 0.0f32;
                // The walls near the predicted position, kept for the tunnel
                // test below while `q` stays there.
                let (q0, near0) = (q, cx.map.walls_near(q));
                let mut near = near0;
                for pass in 0..2 {
                    if pass == 1 {
                        near = cx.map.walls_near(q);
                    }
                    let mut touched = false;
                    for &sg in near {
                        let (sa, sb) = cx.map.segment(sg);
                        let cp = closest_on_segment(q, sa, sb);
                        let d = q - cp;
                        let d2 = d.len2();
                        if d2 < r * r {
                            touched = true;
                            let dist = d2.sqrt();
                            let n = if dist > 1e-6 {
                                d * (1.0 / dist)
                            } else {
                                (sb - sa).perp().normalize_or_zero()
                            };
                            q += n * (r - dist);
                            wall_push += r - dist;
                        }
                    }
                    // A pass that touched no wall left `q` as it was: the
                    // second would repeat it exactly.
                    if !touched {
                        break;
                    }
                }
                let officer = Archetype::from_u8(stat[k][stat::ARCH]).is_officer();
                for o in cx.grid.near(q) {
                    let ob = &cx.obbs[o as usize];
                    if officer && ob.kind == 2 {
                        continue;
                    }
                    if let Some(corr) = ob.push_out(q, r) {
                        wall_push += corr.len();
                        q += corr;
                    }
                }
                for o in cx.veh_grid.near(q) {
                    if let Some(corr) = cx.veh_obbs[o as usize].push_out(q, r) {
                        wall_push += corr.len();
                        q += corr;
                    }
                }
                // Never tunnel through a wall: undo a move that crossed one.
                // Not moving crosses nothing (`segment_hit` of a zero-length
                // move is `None`), and a sub-cell's list is tested once when
                // both ends are in it.
                let mut crossed = false;
                if q != start {
                    let l0 = cx.map.walls_near(start);
                    let l1 = if q == q0 { near0 } else { cx.map.walls_near(q) };
                    let l1 = if core::ptr::eq(l0, l1) { &[][..] } else { l1 };
                    'tunnel: for list in [l0, l1] {
                        for &sg in list {
                            let (sa, sb) = cx.map.segment(sg);
                            if segment_hit(start, q, sa, sb).is_some() {
                                crossed = true;
                                break 'tunnel;
                            }
                        }
                    }
                }
                if crossed {
                    q = start;
                }
                px[k] = q.x;
                py[k] = q.y;
                let v = (q - start) * (1.0 / DT);
                let v = v.clamp_len(10.0);
                vx[k] = v.x;
                vy[k] = v.y;
                let pr =
                    (((press[k] * inv_iters - baseline).max(0.0) + wall_push) * pscale).min(1.0);
                peak = peak.max(pr);
                // What hurts is sustained compression, not a collision.
                let held = widen_u8(pose[k][pose::PRESSURE]);
                pose[k][pose::PRESSURE] = narrow_u8(held + (pr - held) * smooth);

                // Pose: gait from action and speed; stride from distance.
                let speed = v.len();
                let gait = Gait::from_u8(pose[k][pose::GAIT]);
                let act = Action::from_u8(stat[k][stat::ACTION]);
                if !gait.is_down() {
                    let g = if speed > a.walk * 1.35 {
                        Gait::Run
                    } else if speed > 0.15 {
                        Gait::Walk
                    } else {
                        match act {
                            Action::Film => Gait::Film,
                            Action::Throw => Gait::Throw,
                            Action::Strike => Gait::Strike,
                            Action::Barricade | Action::Help | Action::Lurk => Gait::Crouch,
                            _ => Gait::Stand,
                        }
                    };
                    pose[k][pose::GAIT] = g as u8;
                }
                pose[k][pose::STRIDE] =
                    pose[k][pose::STRIDE].wrapping_add((speed * DT * (256.0 / 1.4)) as u8);

                // Stillness counter and falling asleep (DESIGN §7.6).
                // Only sleep untouched: two overlapping sleepers would never separate.
                let calm = widen_u8(emo[k][emo::FEAR]) < 0.45
                    && press[k] * inv_iters < 0.004
                    && fl & flag::URGENT == 0;
                let restful = matches!(
                    act,
                    Action::Hold
                        | Action::Rest
                        | Action::Idle
                        | Action::Formation
                        | Action::Lurk
                        | Action::March
                        | Action::Chant
                ) || gait == Gait::Fallen;
                let still = aux::still(aux[k]);
                if speed < sleep_speed && calm && restful {
                    let s2 = still.saturating_add(1);
                    if s2 >= sleep_ticks && act != Action::Chant {
                        stat[k][stat::FLAGS] |= flag::ASLEEP;
                        vx[k] = 0.0;
                        vy[k] = 0.0;
                        aux[k] = aux::with_still(aux[k], 0);
                    } else {
                        aux[k] = aux::with_still(aux[k], s2);
                    }
                } else if still != 0 {
                    aux[k] = aux::with_still(aux[k], 0);
                }
            }
            peak
        })
        .reduce(|| 0.0f32, f32::max);
}

/// What every contact iteration reads besides positions.
struct ContactCtx<'a> {
    sp: &'a Spatial,
    len: usize,
    relax: f32,
    wake_push: f32,
}

/// The first contact iteration over the chunk at `base`, reading positions
/// through `src`: every agent, and the wake events. Sets each agent's busy
/// flag and lists the agents that can still move in later iterations (awake,
/// and with neighbours or moved); returns how many.
#[allow(clippy::too_many_arguments)]
fn first_iteration(
    cx: &ContactCtx,
    base: usize,
    src: impl Fn(usize) -> (f32, f32) + Copy,
    dx: &mut [f32],
    dy: &mut [f32],
    pr: &mut [f32],
    ev: &mut Vec<Event>,
    busy: &mut [u8],
    cand: &mut [u16],
) -> u16 {
    let sp = cx.sp;
    let mut n = 0;
    for k in 0..dx.len() {
        let i = base + k;
        let (xi, yi) = src(i);
        let wi = sp.inv_mass[i];
        if i >= cx.len || wi == 0.0 {
            dx[k] = xi;
            dy[k] = yi;
            busy[k] = 0;
            continue;
        }
        let (cx_, cy_, push, hit) = contact(i, xi, yi, wi, src, sp, cx.relax, |j, wj, overlap| {
            if wj == 0.0 && overlap > cx.wake_push && ev.len() < ev.capacity() {
                ev.push(Event::Wake { target: j as u32 });
            }
        });
        let (x, y) = (xi + cx_, yi + cy_);
        dx[k] = x;
        dy[k] = y;
        pr[k] += push;
        let b = hit || moved(xi, yi, x, y);
        busy[k] = b as u8;
        if b || sp.nbr_n[i] > 0 {
            cand[n] = k as u16;
            n += 1;
        }
    }
    n as u16
}

/// A later contact iteration over the chunk at `base`: of the candidates,
/// only those that touched someone or moved in the last iteration, or have a
/// neighbour that moved (`prev`). Anyone else would read exactly the
/// positions of the last iteration again, touch nobody and add `+0.0` to
/// where they are, which both position buffers already hold.
#[allow(clippy::too_many_arguments)]
fn later_iteration(
    cx: &ContactCtx,
    base: usize,
    src: impl Fn(usize) -> (f32, f32) + Copy,
    prev: &[u8],
    cand: &[u16],
    dx: &mut [f32],
    dy: &mut [f32],
    pr: &mut [f32],
    busy: &mut [u8],
) {
    let sp = cx.sp;
    for &k in cand {
        let k = k as usize;
        let i = base + k;
        if prev[i] == 0 && sp.neighbours(i).iter().all(|&j| prev[j as usize] == 0) {
            busy[k] = 0;
            continue;
        }
        let (xi, yi) = src(i);
        let wi = sp.inv_mass[i];
        let (cx_, cy_, push, hit) = contact(i, xi, yi, wi, src, sp, cx.relax, |_, _, _| {});
        let (x, y) = (xi + cx_, yi + cy_);
        dx[k] = x;
        dy[k] = y;
        pr[k] += push;
        busy[k] = (hit || moved(xi, yi, x, y)) as u8;
    }
}

/// Agent `i`'s contact correction against its neighbours at positions
/// `src`: the shift, the push it feels and whether it touched anyone.
/// `touch` sees each neighbour it overlaps, with that neighbour's inverse
/// mass and the overlap.
#[inline(always)]
fn contact(
    i: usize,
    xi: f32,
    yi: f32,
    wi: f32,
    src: impl Fn(usize) -> (f32, f32),
    sp: &Spatial,
    relax: f32,
    mut touch: impl FnMut(usize, f32, f32),
) -> (f32, f32, f32, bool) {
    let ri = sp.radius[i];
    let (mut cx_, mut cy_) = (0.0f32, 0.0f32);
    let mut push = 0.0f32;
    let mut hit = false;
    for &j in sp.neighbours(i) {
        let j = j as usize;
        let (xj, yj) = src(j);
        let (ddx, ddy) = (xi - xj, yi - yj);
        let d2 = ddx * ddx + ddy * ddy;
        let rr = ri + sp.radius[j];
        if d2 >= rr * rr {
            continue;
        }
        hit = true;
        let wj = sp.inv_mass[j];
        let share = wi / (wi + wj);
        let (nx, ny, dist) = if d2 > 1e-12 {
            let d = d2.sqrt();
            (ddx / d, ddy / d, d)
        } else {
            // Exactly on top of each other: separate by index.
            let sgn = if i < j { 1.0 } else { -1.0 };
            (sgn, 0.0, 0.0)
        };
        let overlap = rr - dist;
        let m = overlap * share * relax;
        cx_ += nx * m;
        cy_ += ny * m;
        push += overlap * share;
        touch(j, wj, overlap);
    }
    (cx_, cy_, push, hit)
}

/// Did a contact iteration change the position's bits (`x + 0.0` does for
/// `-0.0`)?
#[inline(always)]
fn moved(x0: f32, y0: f32, x: f32, y: f32) -> bool {
    x.to_bits() != x0.to_bits() || y.to_bits() != y0.to_bits()
}
