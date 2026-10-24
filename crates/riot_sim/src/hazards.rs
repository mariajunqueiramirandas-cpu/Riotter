//! Hazards (DESIGN §2.6): tear gas, fire and smoke, projectiles, cameras,
//! and the metro (DESIGN §2.8).

use riot_core::math::sweep_circle;
use riot_core::rng::{Stream, roll_key, unit};
use riot_core::{StateHasher, Vec2};

use crate::archetype::Archetype;
use crate::fields::{CC, Layer, NO_CHUNK, blur_shift_kernel};
use crate::mapdef::{EntryKind, PropKind};
use crate::setup::{DT, NotableKind, RiotSetup};
use crate::spatial::FIELD_CELL;
use crate::state::{Action, flag, intent, pose, stat};
use crate::world::{Prop, World, prop_flag};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Emitter {
    pub pos: Vec2,
    pub ticks_left: u32,
    pub rate: f32,
    /// Stable id of the officer who fired it, or `u32::MAX`.
    pub owner: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProjectileKind {
    Stone = 0,
    Molotov = 1,
    Canister = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projectile {
    pub kind: ProjectileKind,
    pub pos: Vec2,
    pub vel: Vec2,
    /// Lands here if it hits nothing first.
    pub target: Vec2,
    pub owner: u32,
    pub ttl: u16,
}

pub struct Hazards {
    pub emitters: Vec<Emitter>,
    pub projectiles: Vec<Projectile>,
    pub wind: Vec2,
    /// Projectiles stopped by a shield or a body.
    pub blocked: u32,
    pub hits: u32,
}

impl Hazards {
    pub fn new(setup: &RiotSetup) -> Self {
        Hazards {
            emitters: Vec::with_capacity(256),
            projectiles: Vec::with_capacity(setup.limits.projectiles as usize),
            wind: Vec2::new(setup.weather.wind[0], setup.weather.wind[1]),
            blocked: 0,
            hits: 0,
        }
    }

    /// A canister starts emitting at `p`.
    pub fn add_gas(&mut self, p: Vec2, _tick: u32, owner: u32) {
        if self.emitters.len() < self.emitters.capacity() {
            self.emitters.push(Emitter {
                pos: p,
                ticks_left: 20 * 40,
                rate: 1.0,
                owner,
            });
        }
    }

    pub fn throw_canister(&mut self, from: Vec2, to: Vec2, owner: u32, speed: f32) {
        self.launch(ProjectileKind::Canister, from, to, owner, speed);
    }

    pub fn launch(&mut self, kind: ProjectileKind, from: Vec2, to: Vec2, owner: u32, speed: f32) {
        if self.projectiles.len() == self.projectiles.capacity() {
            return;
        }
        let d = to - from;
        let dist = d.len().max(0.1);
        let ttl = (dist / (speed * DT)).ceil() as u16 + 1;
        self.projectiles.push(Projectile {
            kind,
            pos: from,
            vel: d * (speed / dist),
            target: to,
            owner,
            ttl,
        });
    }

    /// Start a fire at `p`: fuel from props under it, or a little from the
    /// ground.
    pub fn ignite(
        &mut self,
        p: Vec2,
        fields: &mut crate::fields::Fields,
        map: &crate::map::Map,
        props: &mut [Prop],
    ) {
        let mut fuel = 40.0;
        for pr in props.iter_mut() {
            let reach = pr.half.x.max(pr.half.y) + 2.0;
            if pr.flags & prop_flag::DESTROYED != 0 || (pr.pos - p).len2() > reach * reach {
                continue;
            }
            pr.flags |= prop_flag::BURNING;
            fuel += match pr.kind {
                PropKind::Car => 600.0,
                PropKind::Barricade => 400.0,
                PropKind::Bin => 150.0,
                _ => 80.0,
            };
            // Fuel covers the prop's footprint.
            let r = pr.half.x.max(pr.half.y);
            fields.stamp(Layer::Fuel, pr.pos, r + 1.0, fuel * 0.5, map);
        }
        fields.stamp(Layer::Fuel, p, 2.0, fuel, map);
        fields.raise_at(Layer::Fire, p, 1.0, map);
    }

    pub fn hash(&self, h: &mut StateHasher) {
        for e in &self.emitters {
            h.write_f32(e.pos.x);
            h.write_f32(e.pos.y);
            h.write_u32(e.ticks_left);
        }
        for p in &self.projectiles {
            h.write_f32(p.pos.x);
            h.write_f32(p.pos.y);
            h.write_u32(p.kind as u32 | (p.ttl as u32) << 8);
        }
    }
}

/// Keep chunks with emitters, fire or projectiles active.
pub fn pin_emitters(w: &mut World) {
    w.fields.pinned.fill(false);
    for k in 0..w.hazards.emitters.len() {
        let p = w.hazards.emitters[k].pos;
        let (cx, cy) = w.fields.chunk_at(p);
        let c = w.fields.chunk_index(cx, cy);
        if let Some(s) = w.fields.activate(c, &w.map) {
            w.fields.pinned[s] = true;
        }
    }
    for k in 0..w.props.len() {
        if w.props[k].flags & prop_flag::BURNING != 0 {
            let (cx, cy) = w.fields.chunk_at(w.props[k].pos);
            let c = w.fields.chunk_index(cx, cy);
            if let Some(s) = w.fields.activate(c, &w.map) {
                w.fields.pinned[s] = true;
            }
        }
    }
}

/// Cameras: the layer counts the cameras that can see each cell, from
/// filming journalists and phones (DESIGN §2.7). Every 10 ticks.
pub fn stamp_cameras(w: &mut World) {
    for slot in 0..w.fields.pool {
        if w.fields.slot_chunk[slot] != NO_CHUNK {
            w.fields.layer_mut(slot, Layer::Camera).fill(0.0);
        }
    }
    let r = w.tun.camera_radius;
    for i in 0..w.people.len {
        let st = w.people.stat[i];
        if st[stat::FLAGS] & flag::DEAD != 0 || Action::from_u8(st[stat::ACTION]) != Action::Film {
            continue;
        }
        let p = w.people.pos(i);
        // The camera sees a half-disc in front.
        let fwd = Vec2::from_heading(w.people.pose[i][pose::HEADING]);
        let weight = if Archetype::from_u8(st[stat::ARCH]) == Archetype::Journalist {
            2.0
        } else {
            1.0
        };
        stamp_cone(w, p, fwd, r, weight);
    }
}

fn stamp_cone(w: &mut World, p: Vec2, fwd: Vec2, r: f32, v: f32) {
    let f = &mut w.fields;
    let (fx, fy) = f.geom.to_cell_f(p.x, p.y);
    let rc = r / FIELD_CELL;
    let dim = f.geom.dim as i32;
    for cy in ((fy - rc).floor() as i32).max(0)..((fy + rc).ceil() as i32).min(dim) {
        for cx in ((fx - rc).floor() as i32).max(0)..((fx + rc).ceil() as i32).min(dim) {
            let (dx, dy) = (cx as f32 + 0.5 - fx, cy as f32 + 0.5 - fy);
            if dx * dx + dy * dy > rc * rc || dx * fwd.x + dy * fwd.y < -0.5 {
                continue;
            }
            if let Some(s) = f.slot_of_chunk(cx / CC as i32, cy / CC as i32) {
                f.layer_mut(s, Layer::Camera)[(cy as usize % CC) * CC + cx as usize % CC] += v;
            }
        }
    }
}

/// Gas and smoke drift with the wind, diffuse and decay; fire burns fuel and
/// spreads (DESIGN §2.6). Part of phase 3.
pub fn update_fields(w: &mut World) {
    let tick = w.tick;
    // Emitters.
    let emit = w.tun.gas_emit;
    let mut k = 0;
    while k < w.hazards.emitters.len() {
        let e = &mut w.hazards.emitters[k];
        if e.ticks_left == 0 {
            w.hazards.emitters.swap_remove(k);
            continue;
        }
        e.ticks_left -= 1;
        let (p, rate) = (e.pos, e.rate);
        w.fields
            .stamp(Layer::Gas, p, 1.5, emit * rate * 0.25, &w.map);
        k += 1;
    }
    // Wind: semi-Lagrangian shift in cells per tick, plus blur.
    let wind = w.hazards.wind;
    let (sx, sy) = (wind.x * DT / FIELD_CELL, wind.y * DT / FIELD_CELL);
    let a = w.tun.gas_diffuse;
    w.fields.filter(
        Layer::Gas,
        blur_shift_kernel(a, sx),
        blur_shift_kernel(a, sy),
        w.tun.gas_decay,
        true,
    );
    w.fields.filter(
        Layer::Smoke,
        blur_shift_kernel(a * 0.8, sx),
        blur_shift_kernel(a * 0.8, sy),
        w.tun.smoke_decay,
        true,
    );
    fire(w, tick);
}

/// Fire as a cellular automaton on the fuel layer.
fn fire(w: &mut World, tick: u32) {
    let burn = w.tun.fire_burn;
    let spread = w.tun.fire_spread;
    let seed = w.seed;
    let mut any = false;
    for slot in 0..w.fields.pool {
        let c = w.fields.slot_chunk[slot];
        if c == NO_CHUNK {
            continue;
        }
        if !w.fields.layer(slot, Layer::Fire).iter().any(|&v| v > 0.0) {
            continue;
        }
        any = true;
        let (ccx, ccy) = (c % w.fields.cdim, c / w.fields.cdim);
        // Sequential in slot and cell order: a cell lit this pass may burn
        // in the same pass, which is deterministic.
        for cell in 0..crate::fields::CL {
            let f0 = w.fields.layer(slot, Layer::Fire)[cell];
            if f0 <= 0.0 {
                continue;
            }
            let fuel = w.fields.layer(slot, Layer::Fuel)[cell];
            let nf = (fuel - burn).max(0.0);
            w.fields.layer_mut(slot, Layer::Fuel)[cell] = nf;
            w.fields.layer_mut(slot, Layer::Fire)[cell] = if nf > 0.0 {
                (f0 + 0.05).min(1.0)
            } else {
                (f0 - 0.05).max(0.0)
            };
            w.fields.layer_mut(slot, Layer::Smoke)[cell] += 0.3 * f0;
            // Spread to neighbouring cells with fuel.
            let (lx, ly) = ((cell % CC) as i32, (cell / CC) as i32);
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (gx, gy) = (
                    ccx as i32 * CC as i32 + lx + dx,
                    ccy as i32 * CC as i32 + ly + dy,
                );
                let id = (gy as u32)
                    .wrapping_mul(w.fields.geom.dim)
                    .wrapping_add(gx as u32);
                if unit(roll_key(
                    seed,
                    tick,
                    id,
                    (dx + 2 + (dy + 2) * 4) as u32,
                    Stream::Hazard,
                )) >= spread
                {
                    continue;
                }
                let Some(ns) = w
                    .fields
                    .slot_of_chunk(gx.div_euclid(CC as i32), gy.div_euclid(CC as i32))
                else {
                    continue;
                };
                let ncell =
                    (gy.rem_euclid(CC as i32) as usize) * CC + gx.rem_euclid(CC as i32) as usize;
                if w.fields.layer(ns, Layer::Fuel)[ncell] > 0.0
                    && w.fields.layer(ns, Layer::Fire)[ncell] == 0.0
                {
                    w.fields.layer_mut(ns, Layer::Fire)[ncell] = 0.2;
                }
            }
        }
    }
    if !any {
        return;
    }
    // Burning props lose structure; burnt-out props are destroyed.
    for k in 0..w.props.len() {
        let pr = w.props[k];
        if pr.flags & prop_flag::BURNING == 0 {
            continue;
        }
        let f = w.fields.value_at(Layer::Fire, pr.pos);
        let pr = &mut w.props[k];
        pr.health = pr.health.saturating_sub(if f > 0.1 { 1 } else { 0 });
        if pr.health == 0 {
            pr.flags = (pr.flags | prop_flag::DESTROYED) & !prop_flag::BURNING;
        } else if f <= 0.0 && tick % 20 == 0 {
            pr.flags &= !prop_flag::BURNING;
        }
    }
}

/// Move projectiles; hit walls, shields and bodies (DESIGN §11.3). A bottle
/// moves about 0.75 m per tick, more than a body width, so hits are tested
/// along the whole segment it travelled.
pub fn projectiles(w: &mut World) {
    let tick = w.tick;
    let mut k = 0;
    while k < w.hazards.projectiles.len() {
        let pr = w.hazards.projectiles[k];
        let p0 = pr.pos;
        let mut p1 = p0 + pr.vel * DT;
        let mut landed = pr.ttl <= 1;
        if (pr.target - p0).len2() <= (p1 - p0).len2() {
            p1 = pr.target;
            landed = true;
        }
        // Walls.
        if let Some(t) = w.map.ray_blocked(p0, p1) {
            p1 = p0 + (p1 - p0) * (t * 0.98);
            landed = true;
        }
        // Bodies: the earliest one the segment touches.
        let mut hit: Option<(f32, usize)> = None;
        if pr.kind != ProjectileKind::Canister {
            let mid = (p0 + p1) * 0.5;
            let reach = (p1 - p0).len() * 0.5 + 0.5;
            let px = riot_core::lanes::flat(&w.people.pos_x);
            let py = riot_core::lanes::flat(&w.people.pos_y);
            let owner = pr.owner;
            let people = &w.people;
            let sp = &w.spatial;
            sp.query_radius(px, py, mid.x, mid.y, reach, |j| {
                if people.stable_id[j] == owner || people.stat[j][stat::FLAGS] & flag::DEAD != 0 {
                    return;
                }
                if let Some(t) =
                    sweep_circle(p0, p1, Vec2::new(px[j], py[j]), sp.radius[j].max(0.2))
                    && hit.is_none_or(|h| t < h.0)
                {
                    hit = Some((t, j));
                }
            });
        }
        if let Some((t, j)) = hit {
            let at = p0 + (p1 - p0) * t;
            let dir = pr.vel.normalize_or_zero();
            let facing = Vec2::from_heading(w.people.pose[j][pose::HEADING]);
            let shielded =
                w.people.stat[j][stat::FLAGS] & flag::SHIELD != 0 && facing.dot(-dir) > 0.3;
            if shielded {
                w.hazards.blocked += 1;
            } else {
                w.hazards.hits += 1;
                let dmg = riot_core::narrow_u8(w.tun.projectile_damage);
                crate::interact::damage(w, j, dmg, pr.owner);
            }
            if pr.kind == ProjectileKind::Molotov {
                w.hazards.ignite(at, &mut w.fields, &w.map, &mut w.props);
                w.outcome.fires += 1;
            }
            w.hazards.projectiles.swap_remove(k);
            continue;
        }
        if landed {
            match pr.kind {
                ProjectileKind::Molotov => {
                    w.hazards.ignite(p1, &mut w.fields, &w.map, &mut w.props);
                    if w.outcome.fires == 0 {
                        w.outcome.notable(tick, NotableKind::Fire, p1, pr.owner);
                    }
                    w.outcome.fires += 1;
                }
                ProjectileKind::Canister => w.hazards.add_gas(p1, tick, pr.owner),
                ProjectileKind::Stone => {}
            }
            w.hazards.projectiles.swap_remove(k);
            continue;
        }
        let pm = &mut w.hazards.projectiles[k];
        pm.pos = p1;
        pm.ttl -= 1;
        k += 1;
    }
}

// ---------------------------------------------------------------------------
// The metro moves people between stations after a delay (DESIGN §2.8).

#[derive(Clone, Copy, Debug)]
pub struct Rider {
    pub arrive: u32,
    pub stable: u32,
    pub to: u16,
    pub archetype: u8,
    pub emo: [u8; 4],
    pub health: u8,
    pub flags: u8,
    pub intent: u16,
    pub belief: u16,
}

pub struct Metro {
    pub riders: Vec<Rider>,
    pub rides: u32,
}

impl Metro {
    pub fn new(cap: usize) -> Self {
        Metro {
            riders: Vec::with_capacity(cap.min(16_384)),
            rides: 0,
        }
    }
}

/// Board the metro at entry `from` if a station is much closer to where this
/// person is going. Returns true if they boarded.
pub fn try_board(w: &mut World, i: usize, from: usize) -> bool {
    if w.metro.riders.len() == w.metro.riders.capacity() || w.entry_closed[from] {
        return false;
    }
    let slot = intent::objective(w.people.intent[i]);
    let Some(o) = w.objectives.get(slot as usize).filter(|o| o.active) else {
        return false;
    };
    let here = w.map.entries[from].pos;
    let mut best: Option<(f32, usize)> = None;
    for (k, e) in w.map.entries.iter().enumerate() {
        if k == from || e.kind != EntryKind::Metro || w.entry_closed[k] {
            continue;
        }
        let d = (e.pos - o.pos).len();
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, k));
        }
    }
    let Some((d, to)) = best else { return false };
    if d + 600.0 > (here - o.pos).len() {
        return false;
    }
    let st = w.people.stat[i];
    w.metro.riders.push(Rider {
        arrive: w.tick + w.tun.metro_delay.max(1.0) as u32,
        stable: w.people.stable_id[i],
        to: to as u16,
        archetype: st[stat::ARCH],
        emo: w.people.emo[i],
        health: st[stat::HEALTH],
        flags: st[stat::FLAGS] & (flag::PHONE | flag::KNOWN | flag::ITEM),
        intent: w.people.intent[i],
        belief: w.people.belief[i],
    });
    w.metro.rides += 1;
    w.remove(i);
    // Riding the metro is not leaving: undo the "left" mark.
    let s = w.people.stable_id[i] as usize;
    w.cold[s].left = u32::MAX;
    true
}

pub fn metro_arrivals(w: &mut World) {
    let mut k = 0;
    while k < w.metro.riders.len() {
        let r = w.metro.riders[k];
        if r.arrive > w.tick {
            k += 1;
            continue;
        }
        w.metro.riders.swap_remove(k);
        if w.entry_closed[r.to as usize] {
            // Station closed while they rode: they get off at the next one.
            if let Some(alt) = (0..w.map.entries.len()).find(|&e| !w.entry_closed[e]) {
                respawn(w, r, alt);
            }
            continue;
        }
        respawn(w, r, r.to as usize);
    }
}

fn respawn(w: &mut World, r: Rider, entry: usize) {
    if w.people.len >= w.people.cap {
        return;
    }
    let p = w.map.entries[entry].pos;
    let i = w.people.push(crate::people::Spawn {
        pos: p,
        heading: 0,
        emo: r.emo,
        health: r.health,
        action: Action::March as u8,
        archetype: r.archetype,
        flags: r.flags,
        intent: r.intent,
        belief: r.belief,
        stable_id: r.stable,
    });
    w.slot_of[r.stable as usize] = i as u32;
}
