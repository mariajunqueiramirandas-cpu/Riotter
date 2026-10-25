//! Phase 7: bodies (DESIGN §11.2) — predict, contacts, walls, props and
//! vehicles, velocity from movement, pressure and pose.
//!
//! Contacts are position-based constraints solved with Jacobi iterations
//! (Weiss et al., "Position-Based Real-Time Simulation of Large Crowds"):
//! each agent reads every neighbour's position from the previous iteration
//! and writes only its own into a second buffer. Unlike Gauss-Seidel this
//! keeps the single-writer rule, and results do not depend on order or
//! thread count.

use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{closest_on_segment, narrow_u8, segment_hit, widen_u8};
use riot_core::{Buckets, Vec2};
use riot_team::prelude::*;

use crate::archetype::{ARCHETYPES, ArchetypeParams};
use crate::event::Event;
use crate::map::Map;
use crate::people::People;
use crate::setup::DT;
use crate::spatial::Spatial;
use crate::state::{Action, Gait, aux, emo, flag, pose, stat};
use crate::tunables::Tunables;

/// An oriented box people collide with: a prop or a vehicle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Obb {
    pub c: Vec2,
    /// Unit vector of the box's local x axis.
    pub ax: Vec2,
    pub half: Vec2,
    /// 0 = prop, 1 = vehicle.
    pub kind: u8,
    pub id: u16,
}

impl Obb {
    /// Push a circle out of the box: returns the correction, if touching.
    #[inline]
    pub fn push_out(&self, p: Vec2, r: f32) -> Option<Vec2> {
        let d = p - self.c;
        let ay = self.ax.perp();
        let (lx, ly) = (d.dot(self.ax), d.dot(ay));
        let (hx, hy) = (self.half.x, self.half.y);
        if lx.abs() > hx + r || ly.abs() > hy + r {
            return None;
        }
        let (cx, cy) = (lx.clamp(-hx, hx), ly.clamp(-hy, hy));
        let (ox, oy) = (lx - cx, ly - cy);
        let dist2 = ox * ox + oy * oy;
        if dist2 > 1e-10 {
            if dist2 >= r * r {
                return None;
            }
            let dist = dist2.sqrt();
            let k = (r - dist) / dist;
            return Some(self.ax * (ox * k) + ay * (oy * k));
        }
        // Centre inside the box: leave by the nearest face.
        let (px, py) = (hx - lx.abs() + r, hy - ly.abs() + r);
        Some(if px < py {
            self.ax * (px * lx.signum())
        } else {
            ay * (py * ly.signum())
        })
    }

    pub fn contains(&self, p: Vec2) -> bool {
        let d = p - self.c;
        d.dot(self.ax).abs() <= self.half.x && d.dot(self.ax.perp()).abs() <= self.half.y
    }
}

/// Coarse hash of obstacles over 4 m cells; an obstacle is listed in every
/// cell its expanded bounds overlap, so a point query reads one bucket.
pub struct ObstacleGrid {
    half: f32,
    mask: u32,
    ent_bucket: Vec<u32>,
    ent_obj: Vec<u32>,
    buckets: Buckets,
}

const OBST_CELL: f32 = 4.0;

#[inline]
fn obst_bucket(x: i32, y: i32, mask: u32) -> u32 {
    ((x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77)) & mask
}

impl ObstacleGrid {
    pub fn new(world_size: f32, max_obstacles: usize) -> Self {
        let cap = max_obstacles * 8 + 64;
        let buckets = (max_obstacles * 4).next_power_of_two().max(256);
        ObstacleGrid {
            half: world_size * 0.5,
            mask: buckets as u32 - 1,
            ent_bucket: Vec::with_capacity(cap),
            ent_obj: Vec::with_capacity(cap),
            buckets: Buckets::new(buckets, cap),
        }
    }

    pub fn rebuild(&mut self, obbs: &[Obb]) {
        self.ent_bucket.clear();
        self.ent_obj.clear();
        // Hash into about four buckets per obstacle present, not per
        // obstacle the scenario allows.
        let n = (obbs.len() * 4)
            .next_power_of_two()
            .clamp(256, self.buckets.buckets());
        self.mask = n as u32 - 1;
        for (k, o) in obbs.iter().enumerate() {
            let ext = o.half.x.max(o.half.y) + 0.6;
            let x0 = ((o.c.x - ext + self.half) / OBST_CELL).floor() as i32;
            let x1 = ((o.c.x + ext + self.half) / OBST_CELL).floor() as i32;
            let y0 = ((o.c.y - ext + self.half) / OBST_CELL).floor() as i32;
            let y1 = ((o.c.y + ext + self.half) / OBST_CELL).floor() as i32;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if self.ent_bucket.len() == self.ent_bucket.capacity() {
                        return;
                    }
                    self.ent_bucket.push(obst_bucket(x, y, self.mask));
                    self.ent_obj.push(k as u32);
                }
            }
        }
        self.buckets.rebuild_in(&self.ent_bucket, n);
    }

    /// Obstacles listed in the cell containing `p` (may include a few far
    /// ones from hash collisions).
    #[inline]
    pub fn near(&self, p: Vec2) -> impl Iterator<Item = u32> + '_ {
        let x = ((p.x + self.half) / OBST_CELL).floor() as i32;
        let y = ((p.y + self.half) / OBST_CELL).floor() as i32;
        self.buckets
            .bucket(obst_bucket(x, y, self.mask))
            .iter()
            .map(|&e| self.ent_obj[e as usize])
    }
}

/// Preallocated buffers for the body phase.
pub struct PhysScratch {
    pub qx: Vec<f32>,
    pub qy: Vec<f32>,
    pub x0: Vec<f32>,
    pub y0: Vec<f32>,
    pub press: Vec<f32>,
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
    let wake_push = tun.wake_push;
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
        let first = it == 0;
        dst_x[..span]
            .par_chunks_mut(CHUNK_AGENTS)
            .zip(dst_y[..span].par_chunks_mut(CHUNK_AGENTS))
            .zip(press.par_chunks_mut(CHUNK_AGENTS))
            .zip(events[..chunks].par_iter_mut())
            .enumerate()
            .for_each(|(c, (((dx, dy), pr), ev))| {
                let base = c * CHUNK_AGENTS;
                for k in 0..dx.len() {
                    let i = base + k;
                    let (xi, yi) = (src_x[i], src_y[i]);
                    let wi = sp.inv_mass[i];
                    if i >= len || wi == 0.0 {
                        dx[k] = xi;
                        dy[k] = yi;
                        continue;
                    }
                    let ri = sp.radius[i];
                    let (mut cx_, mut cy_) = (0.0f32, 0.0f32);
                    let mut push = 0.0f32;
                    for &j in sp.neighbours(i) {
                        let j = j as usize;
                        let (ddx, ddy) = (xi - src_x[j], yi - src_y[j]);
                        let d2 = ddx * ddx + ddy * ddy;
                        let rr = ri + sp.radius[j];
                        if d2 >= rr * rr {
                            continue;
                        }
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
                        if first && wj == 0.0 && overlap > wake_push && ev.len() < ev.capacity() {
                            ev.push(Event::Wake { target: j as u32 });
                        }
                    }
                    dx[k] = xi + cx_;
                    dy[k] = yi + cy_;
                    pr[k] += push;
                }
            });
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
            let mut peak = 0.0f32;
            for k in 0..px.len() {
                let i = base + k;
                if i >= len {
                    continue;
                }
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
                let start = Vec2::new(x0[i], y0[i]);
                let mut q = Vec2::new(px[k], py[k]);
                let mut wall_push = 0.0f32;
                for _ in 0..2 {
                    for &sg in cx.map.walls_near(q) {
                        let (sa, sb) = cx.map.segment(sg);
                        let cp = closest_on_segment(q, sa, sb);
                        let d = q - cp;
                        let d2 = d.len2();
                        if d2 < r * r {
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
                }
                for o in cx.grid.near(q) {
                    if let Some(corr) = cx.obbs[o as usize].push_out(q, r) {
                        wall_push += corr.len();
                        q += corr;
                    }
                }
                // Never tunnel through a wall: undo a move that crossed one.
                let mut crossed = false;
                for list in [cx.map.walls_near(start), cx.map.walls_near(q)] {
                    for &sg in list {
                        let (sa, sb) = cx.map.segment(sg);
                        if segment_hit(start, q, sa, sb).is_some() {
                            crossed = true;
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
                    (((press[i] * inv_iters - baseline).max(0.0) + wall_push) * pscale).min(1.0);
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
                            Action::Barricade | Action::Help => Gait::Crouch,
                            _ => Gait::Stand,
                        }
                    };
                    pose[k][pose::GAIT] = g as u8;
                }
                pose[k][pose::STRIDE] =
                    pose[k][pose::STRIDE].wrapping_add((speed * DT * (256.0 / 1.4)) as u8);

                // Stillness counter and falling asleep (DESIGN §7.6).
                // Only sleep untouched: two overlapping sleepers would never separate.
                let calm = widen_u8(emo[i][emo::FEAR]) < 0.45
                    && press[i] * inv_iters < 0.004
                    && fl & flag::URGENT == 0;
                let restful = matches!(
                    act,
                    Action::Hold
                        | Action::Rest
                        | Action::Idle
                        | Action::Formation
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obb_push_out() {
        let o = Obb {
            c: Vec2::ZERO,
            ax: Vec2::new(1.0, 0.0),
            half: Vec2::new(2.0, 1.0),
            kind: 0,
            id: 0,
        };
        let c = o.push_out(Vec2::new(0.0, 1.1), 0.25).unwrap();
        assert!((c.y - 0.15).abs() < 1e-5 && c.x.abs() < 1e-6);
        assert!(o.push_out(Vec2::new(0.0, 1.3), 0.25).is_none());
        let c = o.push_out(Vec2::new(1.9, 0.0), 0.25).unwrap();
        assert!((c.x - 0.35).abs() < 1e-5);
        // Rotated 90°.
        let o = Obb {
            ax: Vec2::new(0.0, 1.0),
            ..o
        };
        let c = o.push_out(Vec2::new(1.1, 0.0), 0.25).unwrap();
        assert!((c.x - 0.15).abs() < 1e-5, "{c:?}");
    }
}
