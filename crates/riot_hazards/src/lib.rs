//! Hazards (DESIGN §2.6): tear gas, fire and smoke, projectiles, cameras,
//! and the metro (DESIGN §2.8).
//!
//! The hazards' state is [`riot_world::hazards`].

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::math::{segment_hit, sweep_circle};
use riot_core::rng::{Stream, roll_key, unit};

use riot_defs::archetype::Archetype;
use riot_defs::harm::Harm;
use riot_defs::mapdef::EntryKind;
use riot_defs::setup::{DT, NotableKind};
use riot_defs::state::{Action, flag, intent, pose, stat};
use riot_fields::{CC, CL, Cone, Layer, NO_CHUNK, blur_shift_kernel};
use riot_people::Spawn;
use riot_snapshot::prop_flag;
use riot_spatial::FIELD_CELL;
use riot_world::World;
use riot_world::harm::damage;
use riot_world::hazards::{ProjectileKind, Rider};

/// Half the width of a riot shield (m).
pub const SHIELD_HALF_WIDTH: f32 = 0.36;
/// Radius of a thrown bottle or stone (m): it cannot thread gaps narrower
/// than its own width.
/// The last stretch of a throw, in metres, where it is low enough to hit.
pub const ARC_DESCENT: f32 = 4.0;
pub const PROJECTILE_RADIUS: f32 = 0.06;

/// Keep chunks with emitters, fire or projectiles active. The field phase
/// clears the pins and pins the police barriers' chunks just before.
pub fn pin_emitters(w: &mut World) {
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
/// filming journalists and phones (DESIGN §2.7). Every 10 ticks. The chunks
/// are stamped in parallel, each adding the cameras in index order.
pub fn stamp_cameras(w: &mut World) {
    let f = &mut w.fields;
    f.cones.clear();
    // Room for everyone once, so filming never allocates.
    f.cones.reserve(w.people.cap);
    for i in 0..w.people.len {
        let st = w.people.stat[i];
        if st[stat::FLAGS] & flag::DEAD != 0 || Action::from_u8(st[stat::ACTION]) != Action::Film {
            continue;
        }
        let p = w.people.pos(i);
        // The camera sees a half-disc in front.
        let fwd = Vec2::from_heading(w.people.pose[i][pose::HEADING]);
        let v = if Archetype::from_u8(st[stat::ARCH]) == Archetype::Journalist {
            2.0
        } else {
            1.0
        };
        let (fx, fy) = f.geom.to_cell_f(p.x, p.y);
        f.cones.push(Cone { fx, fy, fwd, v });
    }
    f.stamp_cones(Layer::Camera, w.tun.camera_radius / FIELD_CELL);
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
    if w.hazards.gas_live || !w.hazards.emitters.is_empty() {
        let mx = w.fields.filter(
            Layer::Gas,
            blur_shift_kernel(a, sx),
            blur_shift_kernel(a, sy),
            w.tun.gas_decay,
            true,
        );
        w.hazards.gas_live = mx > 1e-3;
    }
    if w.hazards.smoke_live {
        let mx = w.fields.filter(
            Layer::Smoke,
            blur_shift_kernel(a * 0.8, sx),
            blur_shift_kernel(a * 0.8, sy),
            w.tun.smoke_decay,
            true,
        );
        w.hazards.smoke_live = mx > 1e-3;
    }
    fire(w, tick);
}

/// Fire as a cellular automaton on the fuel layer.
fn fire(w: &mut World, tick: u32) {
    if !w.hazards.fire_live {
        return;
    }
    let burn = w.tun.fire_burn;
    let spread = w.tun.fire_spread;
    let seed = w.seed;
    let mut any = false;
    for slot in 0..w.fields.pool {
        let c = w.fields.slot_chunk[slot];
        if c == NO_CHUNK || w.fields.is_zero(slot, Layer::Fire) {
            continue;
        }
        if !w.fields.layer(slot, Layer::Fire).iter().any(|&v| v > 0.0) {
            // Burnt out: from now on the slot is skipped without a look.
            w.fields.refresh_zero(slot, Layer::Fire);
            continue;
        }
        any = true;
        let (ccx, ccy) = (c % w.fields.cdim, c / w.fields.cdim);
        // Sequential in slot and cell order: a cell lit this pass may burn
        // in the same pass, which is deterministic.
        for cell in 0..CL {
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
            w.hazards.smoke_live = true;
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
    w.hazards.fire_live = any;
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
            if pr.flags & prop_flag::DESTROYED == 0 {
                w.props_dirty = true;
            }
            pr.flags = (pr.flags | prop_flag::DESTROYED) & !prop_flag::BURNING;
        } else if f <= 0.0 && tick.is_multiple_of(20) {
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
        // Bodies and shields: the earliest one the segment touches. A shield
        // is an oriented segment in front of its officer (DESIGN §14). Throws
        // are aimed at the police and arc over the crowd: officers and raised
        // shields can be hit anywhere along the flight, anyone else only in
        // the descent, and nobody lying down.
        let mut hit: Option<(f32, usize, bool)> = None;
        let descending = (pr.target - p1).len() < ARC_DESCENT;
        if pr.kind != ProjectileKind::Canister {
            let mid = (p0 + p1) * 0.5;
            let reach = (p1 - p0).len() * 0.5 + 1.0;
            let px = riot_core::lanes::flat(&w.people.pos_x);
            let py = riot_core::lanes::flat(&w.people.pos_y);
            let owner = pr.owner;
            let people = &w.people;
            let sp = &w.spatial;
            sp.query_radius(px, py, mid.x, mid.y, reach, |j| {
                let st = people.stat[j];
                if people.stable_id[j] == owner
                    || st[stat::FLAGS] & (flag::DEAD | flag::DOWN) != 0
                    || !descending && !Archetype::from_u8(st[stat::ARCH]).is_officer()
                {
                    return;
                }
                let c = Vec2::new(px[j], py[j]);
                if people.stat[j][stat::FLAGS] & flag::SHIELD != 0 {
                    let f = Vec2::from_heading(people.pose[j][pose::HEADING]);
                    let centre = c + f * (sp.radius[j] + 0.1);
                    let half = f.perp() * (SHIELD_HALF_WIDTH + PROJECTILE_RADIUS);
                    if let Some(t) = segment_hit(p0, p1, centre - half, centre + half)
                        && hit.is_none_or(|h| t < h.0)
                    {
                        hit = Some((t, j, true));
                    }
                }
                if let Some(t) = sweep_circle(p0, p1, c, sp.radius[j].max(0.2) + PROJECTILE_RADIUS)
                    && hit.is_none_or(|h| t < h.0)
                {
                    hit = Some((t, j, false));
                }
            });
        }
        if let Some((t, j, shield)) = hit {
            let at = p0 + (p1 - p0) * t;
            if shield {
                w.hazards.blocked += 1;
            } else {
                w.hazards.hits += 1;
                let dmg = riot_core::narrow_u8(w.tun.projectile_damage);
                damage(w, j, dmg, pr.owner, Harm::Projectile);
            }
            if pr.kind != ProjectileKind::Canister
                && Archetype::from_u8(w.people.stat[j][stat::ARCH]).is_officer()
            {
                w.police.cmd.note_violence(tick, at, 0.5);
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
    let i = w.people.push(Spawn {
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
