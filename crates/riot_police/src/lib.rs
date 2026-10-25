//! Police (DESIGN §2.5, §14).
//!
//! - **Squads** are a small AoS table. Each runs a doctrine state machine:
//!   hold line, advance, kettle, disperse, snatch arrest, retreat, put up a
//!   barricade, wait in ambush, charge, escort a march.
//! - **The director** replans every `director_interval` ticks. It clusters
//!   the crowd with union-find over dense cells in the active field chunks;
//!   the commander ([`riot_tactics`]) tracks those crowds, predicts their
//!   routes, climbs (or descends) the escalation ladder within the setup's
//!   cap, plans an operation for each crowd and gives the squads their jobs.
//! - **Formations**: a line is slots along a segment; a kettle is slots on a
//!   contracting circle around a cluster. Officers steer to their slots with
//!   the shared steering kernel.
//! - **Vehicles** drive along the road graph: vans bring reinforcements from
//!   depots and take arrestees away; water cannon trucks spray.
//!
//! Officers are bodies with mass and shields, so pushing matches between a
//! line and a crowd emerge without special code.
//!
//! The police state is [`riot_world::police`].

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::math::widen_u8;
use riot_core::rng::{Stream, roll_key, unit};

use riot_defs::archetype::Archetype;
use riot_defs::police::{Call, Doctrine, NO_CORDON};
use riot_defs::setup::{DT, Escalation, NotableKind};
use riot_defs::state::{Action, Gait, emo, flag, pose, stat};
use riot_defs::targets::OfficerTarget;
use riot_fields::{CC, CL, Layer, NO_CHUNK};
use riot_flow::{GOAL_POLICE, MAX_GOALS, NO_GOAL};
use riot_snapshot::prop_flag;
use riot_tactics as tactics;
use riot_world::World;
use riot_world::police::{
    Cluster, MAX_CLUSTERS, NO_SQUAD, VehicleKind, VehicleState, deploy_at, deploy_on_foot,
    plan_route,
};
use riot_world::world::NO_SLOT;

/// Phase 9: the director every `director_interval` ticks; squads, arrests and
/// vehicles every tick.
pub fn director(w: &mut World) {
    let tick = w.tick;
    // Reinforcements that became available.
    for s in 0..w.police.squads.len() {
        let sq = w.police.squads[s];
        if !sq.deployed && sq.van == NO_SQUAD && tick >= sq.available && sq.available > 0 {
            dispatch(w, s);
        }
    }
    let every = w.tun.director_interval.max(1.0) as u32;
    if tick.is_multiple_of(every) {
        clusters(w);
        tactics::observe(w);
        tactics::escalate(w);
        tactics::plan(w);
        tactics::assign(w);
        route_squads(w);
        surveil(w);
        tactics::use_force(w);
        tactics::restamp(w);
    }
    tactics::tick(w);
    update_squads(w);
    arrests(w);
    vehicles(w);
    // Officers who left the map.
    let slot_of = &w.slot_of;
    w.police.on_map.retain(|&s| slot_of[s as usize] != NO_SLOT);
}

/// Send squad `s` from its depot: by van along the roads when there is one.
fn dispatch(w: &mut World, s: usize) {
    let depot = w.police.squads[s].depot;
    let van = w.police.vehicles.iter().position(|v| {
        v.kind == VehicleKind::Van
            && v.depot == depot
            && v.squad == NO_SQUAD
            && v.state == VehicleState::Parked
    });
    // Where the trouble is: a cordon still short of squads, else the
    // biggest crowd.
    let squads = &w.police.squads;
    let cordon =
        w.police.cmd.cordons.iter().enumerate().find(|&(ci, c)| {
            c.live() && !squads.iter().any(|q| q.deployed && q.cordon == ci as u8)
        });
    let dest = cordon
        .map(|(_, c)| c.line())
        .or_else(|| w.police.clusters.first().map(|c| c.centroid))
        .unwrap_or(w.police.squads[s].home);
    match van {
        Some(v) if plan_route(w, v, dest) => {
            w.police.vehicles[v].squad = s as u16;
            w.police.squads[s].van = v as u16;
            w.police.squads[s].doctrine = Doctrine::Travel;
        }
        _ => deploy_on_foot(w, s),
    }
}

/// Union-find over dense cells in the active field chunks (DESIGN §14).
fn clusters(w: &mut World) {
    let f = &w.fields;
    let p = &mut w.police;
    p.cells.clear();
    p.parent.clear();
    for slot in 0..f.pool {
        let c = f.slot_chunk[slot];
        let lab = &mut p.label[slot * CL..(slot + 1) * CL];
        if c == NO_CHUNK {
            continue;
        }
        let dens = f.layer(slot, Layer::Dens);
        for (cell, &d) in dens.iter().enumerate() {
            if d >= 1.0 {
                lab[cell] = p.cells.len() as u32;
                p.parent.push(p.cells.len() as u32);
                p.cells.push((slot as u32, cell as u32, c));
            } else {
                lab[cell] = u32::MAX;
            }
        }
    }
    fn find(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    let cdim = f.cdim as i32;
    for k in 0..p.cells.len() {
        let (slot, cell, c) = p.cells[k];
        let (cx, cy) = ((c % f.cdim) as i32, (c / f.cdim) as i32);
        let (lx, ly) = ((cell as usize % CC) as i32, (cell as usize / CC) as i32);
        let (gx, gy) = (cx * CC as i32 + lx, cy * CC as i32 + ly);
        for (dx, dy) in [(1, 0), (0, 1), (1, 1), (-1, 1)] {
            let (nx, ny) = (gx + dx, gy + dy);
            let (ncx, ncy) = (nx.div_euclid(CC as i32), ny.div_euclid(CC as i32));
            if ncx < 0 || ncy < 0 || ncx >= cdim || ncy >= cdim {
                continue;
            }
            let ns = if ncx == cx && ncy == cy {
                Some(slot as usize)
            } else {
                f.slot_of_chunk(ncx, ncy)
            };
            let Some(ns) = ns else { continue };
            let ncell =
                (ny.rem_euclid(CC as i32) as usize) * CC + nx.rem_euclid(CC as i32) as usize;
            let l = p.label[ns * CL + ncell];
            if l == u32::MAX {
                continue;
            }
            let (a, b) = (find(&mut p.parent, k as u32), find(&mut p.parent, l));
            if a != b {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                p.parent[hi as usize] = lo;
            }
        }
    }
    // Accumulate per root, roots in index order.
    #[derive(Clone, Copy)]
    struct Acc {
        root: u32,
        dens: f64,
        x: f64,
        y: f64,
        anger: f64,
        threat: f64,
        cells: u32,
        weight: f64,
        lo: Vec2,
        hi: Vec2,
    }
    let mut acc = [Acc {
        root: u32::MAX,
        dens: 0.0,
        x: 0.0,
        y: 0.0,
        anger: 0.0,
        threat: 0.0,
        cells: 0,
        weight: 0.0,
        lo: Vec2::new(f32::MAX, f32::MAX),
        hi: Vec2::new(f32::MIN, f32::MIN),
    }; MAX_CLUSTERS * 4];
    let empty = acc[0];
    let mut nacc = 0usize;
    for k in 0..p.cells.len() {
        let root = find(&mut p.parent, k as u32);
        let (slot, cell, c) = p.cells[k];
        let d = f.layer(slot as usize, Layer::Dens)[cell as usize] as f64;
        let (cx, cy) = (c % f.cdim, c / f.cdim);
        let (gx, gy) = (
            cx * CC as u32 + cell % CC as u32,
            cy * CC as u32 + cell / CC as u32,
        );
        let (x, y) = f.geom.centre(gx, gy);
        let e = match acc[..nacc].iter().position(|a| a.root == root) {
            Some(e) => e,
            None if nacc < acc.len() => {
                acc[nacc] = Acc { root, ..empty };
                nacc += 1;
                nacc - 1
            }
            None => continue,
        };
        let a = &mut acc[e];
        a.dens += d;
        a.x += x as f64 * d;
        a.y += y as f64 * d;
        a.anger += f.layer(slot as usize, Layer::Anger)[cell as usize] as f64;
        a.weight += f.layer(slot as usize, Layer::Weight)[cell as usize] as f64;
        a.threat += f.layer(slot as usize, Layer::Threat)[cell as usize] as f64;
        a.cells += 1;
        a.lo = Vec2::new(a.lo.x.min(x), a.lo.y.min(y));
        a.hi = Vec2::new(a.hi.x.max(x), a.hi.y.max(y));
    }
    p.clusters.clear();
    let mut order: [usize; MAX_CLUSTERS * 4] = [0; MAX_CLUSTERS * 4];
    for (k, o) in order.iter_mut().enumerate().take(nacc) {
        *o = k;
    }
    order[..nacc].sort_unstable_by(|&a, &b| acc[b].dens.total_cmp(&acc[a].dens).then(a.cmp(&b)));
    for &k in order[..nacc].iter() {
        let a = acc[k];
        if a.dens < 40.0 || p.clusters.len() == MAX_CLUSTERS {
            continue;
        }
        let area = a.cells as f32 * 4.0;
        p.clusters.push(Cluster {
            centroid: Vec2::new((a.x / a.dens) as f32, (a.y / a.dens) as f32),
            count: a.dens as f32,
            radius: (area / core::f32::consts::PI).sqrt(),
            anger: (a.anger / a.weight.max(1e-3)) as f32,
            threat: a.threat as f32,
            cells: a.cells,
            lo: a.lo,
            hi: a.hi,
        });
    }
}

/// Route squads far from their anchor through police flow goals, and
/// squads with an officer who cannot see it (round a corner).
fn route_squads(w: &mut World) {
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        let in_sight =
            |p: Vec2| (p - sq.anchor).len() < 25.0 && w.map.ray_blocked(p, sq.anchor).is_none();
        if !sq.deployed
            || in_sight(sq.pos)
                && sq.officers[..sq.n as usize]
                    .iter()
                    .all(|&o| w.index_of(o).is_none_or(|i| in_sight(w.people.pos(i))))
        {
            w.police.squads[k].goal = NO_GOAL;
            continue;
        }
        let g = police_goal(w, sq.anchor);
        w.police.squads[k].goal = g;
    }
}

/// A police flow goal near `p`, reusing one within 30 m or replacing the
/// least recently used.
fn police_goal(w: &mut World, p: Vec2) -> u8 {
    let tick = w.tick;
    let mut lru = (u32::MAX, GOAL_POLICE);
    for g in GOAL_POLICE as usize..MAX_GOALS {
        if w.flow.goals[g].active && (w.police.goal_pos[g] - p).len() < 30.0 {
            w.police.goal_used[g] = tick;
            return g as u8;
        }
        if w.police.goal_used[g] < lru.0 {
            lru = (w.police.goal_used[g], g as u8);
        }
    }
    let g = lru.1;
    w.police.goal_pos[g as usize] = p;
    w.police.goal_used[g as usize] = tick;
    w.flow.set_targets(g, &[p], 6.0, tick);
    g
}

/// Officers recognise organizers near them; footage identifies people.
fn surveil(w: &mut World) {
    for k in 0..w.police.organizers.len() {
        let s = w.police.organizers[k];
        let Some(i) = w.index_of(s) else { continue };
        if w.people.flags(i) & flag::KNOWN != 0 {
            continue;
        }
        let p = w.people.pos(i);
        let seen = w.police.on_map.iter().any(|&o| {
            w.index_of(o)
                .is_some_and(|j| (w.people.pos(j) - p).len2() < 900.0)
        });
        let filmed = w.fields.sample(Layer::Camera, p.x, p.y) > 2.0;
        if seen || filmed {
            w.people.stat[i][stat::FLAGS] |= flag::KNOWN;
        }
    }
}

/// Formation slots → officer steering targets; squad morale and position.
fn update_squads(w: &mut World) {
    let tick = w.tick;
    let shrink = w.tun.kettle_shrink;
    let charge_ticks = w.tun.charge_ticks.max(1.0) as u32;
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        if !sq.deployed {
            continue;
        }
        // Position, presence, morale and fatigue from the officers.
        let (mut sum, mut n, mut fear, mut tired) = (Vec2::ZERO, 0u32, 0.0f32, 0.0f32);
        for &o in &sq.officers[..sq.n as usize] {
            if let Some(i) = w.index_of(o) {
                sum += w.people.pos(i);
                fear += widen_u8(w.people.emo[i][emo::FEAR]);
                tired += widen_u8(w.people.emo[i][emo::FATIGUE]);
                n += 1;
            }
        }
        let squad = &mut w.police.squads[k];
        squad.present = n as u8;
        if n == 0 {
            continue;
        }
        squad.pos = sum * (1.0 / n as f32);
        squad.fatigue = tired / n as f32;
        let fear = fear / n as f32;
        squad.morale += 0.002 * ((1.0 - fear) * (n as f32 / squad.n as f32) - squad.morale);
        // Doctrine motion.
        match squad.doctrine {
            Doctrine::Advance | Doctrine::Disperse => {
                let target = w
                    .police
                    .clusters
                    .get(squad.cluster as usize)
                    .map(|c| c.centroid);
                if let Some(t) = target {
                    let speed = if squad.doctrine == Doctrine::Disperse {
                        1.0
                    } else {
                        0.5
                    };
                    let to = t - squad.anchor;
                    // Advance only while the line keeps up.
                    if (squad.pos - squad.anchor).len() < 4.0 && to.len() > 1.0 {
                        squad.anchor += to.normalize_or_zero() * (speed * DT);
                    }
                    squad.facing = to.normalize_or_zero();
                }
            }
            Doctrine::Kettle => {
                if let Some(c) = w.police.clusters.get(squad.cluster as usize) {
                    let min_r = (c.count / (2.5 * core::f32::consts::PI)).sqrt().max(4.0);
                    if (squad.pos - squad.anchor).len() < squad.radius + 6.0 {
                        squad.radius = (squad.radius * (1.0 - shrink)).max(min_r);
                    }
                }
            }
            Doctrine::Snatch => {
                let t = squad.target_person;
                match w.slot_of.get(t as usize).copied() {
                    Some(j)
                        if j != NO_SLOT
                            && Gait::from_u8(w.people.pose[j as usize][pose::GAIT])
                                != Gait::Restrained =>
                    {
                        squad.anchor = w.people.pos(j as usize);
                    }
                    _ => {
                        squad.target_person = u32::MAX;
                        squad.doctrine = Doctrine::Reserve;
                        squad.anchor = squad.home;
                    }
                }
            }
            Doctrine::Barricade => {
                if let Some(c) = w.police.cmd.cordons.get(squad.cordon as usize) {
                    squad.anchor = c.line();
                    squad.facing = c.face;
                    squad.width = (2.0 * c.half - 0.6).max(1.0);
                }
            }
            Doctrine::Charge => {
                // Run in across the street, then sweep the rear of the crowd
                // back down the route at a walk, then hold.
                let age = tick.wrapping_sub(squad.since);
                let arrived = (squad.pos - squad.anchor).len() < 5.0 || age > charge_ticks / 3;
                if arrived && age <= charge_ticks {
                    if (squad.pos - squad.anchor).len() < 4.0 {
                        squad.anchor += squad.sweep * (1.1 * DT);
                    }
                } else if age > charge_ticks {
                    squad.doctrine = Doctrine::HoldLine;
                    squad.since = tick;
                    squad.anchor = squad.pos;
                    squad.cordon = NO_CORDON;
                }
                squad.facing = squad.sweep;
            }
            _ => {}
        }
        let sq = w.police.squads[k];
        if tick.is_multiple_of(10) {
            let lined = matches!(
                sq.doctrine,
                Doctrine::HoldLine | Doctrine::Barricade | Doctrine::Kettle | Doctrine::Advance
            );
            let (fled, of) = tactics::fled(w, &sq.officers[..sq.n as usize]);
            if lined && !sq.broken && of >= 4 && fled * 2 >= of {
                w.police.squads[k].broken = true;
                w.police.cmd.memory.lines_broken += 1;
                w.police.cmd.call(tick, Call::LineBroken, sq.pos, k as u32);
                w.outcome
                    .notable(tick, NotableKind::LineBroken, sq.pos, u32::MAX);
            } else if sq.broken && fled * 4 < of.max(1) {
                w.police.squads[k].broken = false;
            }
        }
        let building = sq.doctrine == Doctrine::Barricade
            && w.police
                .cmd
                .cordons
                .get(sq.cordon as usize)
                .is_some_and(|c| c.live());
        // Slots.
        let count = sq.n as usize;
        for (slot, &o) in sq.officers[..count].iter().enumerate() {
            let Some(i) = w.index_of(o) else { continue };
            let me = w.people.pos(i);
            let mut t = OfficerTarget {
                pos: sq.anchor,
                goal: sq.goal,
                face: sq.anchor + sq.facing * 10.0,
                run: false,
            };
            match sq.doctrine {
                Doctrine::HoldLine
                | Doctrine::Advance
                | Doctrine::Disperse
                | Doctrine::Reserve
                | Doctrine::Retreat
                | Doctrine::Barricade
                | Doctrine::Escort
                | Doctrine::Charge => {
                    let side = sq.facing.perp();
                    let off = (slot as f32 - (count as f32 - 1.0) * 0.5)
                        * (sq.width / count.max(1) as f32);
                    t.pos = sq.anchor + side * off;
                    if sq.doctrine == Doctrine::Reserve || sq.doctrine == Doctrine::Retreat {
                        let row = (slot / 4) as f32;
                        t.pos =
                            sq.anchor + side * (((slot % 4) as f32 - 1.5) * 1.0) - sq.facing * row;
                        t.run = sq.doctrine == Doctrine::Retreat;
                    }
                    // A charge runs; squads off to a barricade jog there.
                    if sq.doctrine == Doctrine::Charge
                        || sq.doctrine == Doctrine::Barricade && (me - t.pos).len() > 25.0
                    {
                        t.run = true;
                    }
                    // Far behind a moving march: catch up.
                    if sq.doctrine == Doctrine::Escort {
                        t.run = (me - t.pos).len() > 12.0;
                    }
                    if building {
                        let c = w.police.cmd.cordons[sq.cordon as usize];
                        // An officer whose barrier is down sets it up; the
                        // rest hold the line across the street.
                        let mut post = None;
                        for b in (slot..c.slots as usize).step_by(count.max(1)) {
                            let k = c.props[b];
                            let down = k == u16::MAX
                                || w.props[k as usize].flags & prop_flag::DESTROYED != 0;
                            if down {
                                post = Some(c.slot_post(b));
                                break;
                            }
                        }
                        if let Some(p) = post {
                            t.pos = p;
                            t.face = p + c.face * 10.0;
                            t.run = (me - p).len() > 6.0;
                        }
                    }
                }
                Doctrine::Ambush => {
                    // A tight block out of sight, facing the corner; they jog
                    // there.
                    let side = sq.facing.perp();
                    let row = (slot / 4) as f32;
                    t.pos = sq.anchor + side * (((slot % 4) as f32 - 1.5) * 0.8)
                        - sq.facing * row * 0.8;
                    t.run = (me - t.pos).len() > 20.0;
                }
                Doctrine::Kettle => {
                    let (ri, rn) = sq.ring;
                    let total = rn.max(1) as f32 * count as f32;
                    let m = ri as f32 * count as f32 + slot as f32;
                    let h = ((m / total) * 256.0) as u32 as u8;
                    let h = tactics::kettle_heading(h, sq.sweep);
                    t.pos = sq.anchor + Vec2::from_heading(h) * sq.radius;
                    t.face = sq.anchor;
                    // Run to close the ring; walk once in place.
                    t.run = (me - t.pos).len() > 4.0;
                }
                Doctrine::Snatch => {
                    t.pos = sq.anchor;
                    t.face = sq.anchor;
                    t.run = true;
                    // Straight at them only when no wall is in the way.
                    if (me - sq.anchor).len() < 25.0 && w.map.ray_blocked(me, sq.anchor).is_none() {
                        t.goal = NO_GOAL;
                    }
                }
                Doctrine::Idle | Doctrine::Travel => {}
            }
            // Going for someone: the person, not the slot.
            if let Some(q) = tactics::grab_target(w, i)
                && sq.doctrine != Doctrine::Snatch
            {
                t.pos = q;
                t.face = q;
                t.run = true;
                if (me - q).len() < 25.0 {
                    t.goal = NO_GOAL;
                }
            }
            if let Some(&target) = w.police.escorting.get(o) {
                // Walk the arrestee to the squad's van, or home.
                let van = (sq.van != NO_SQUAD).then(|| w.police.vehicles[sq.van as usize].pos);
                t.pos = van.unwrap_or(sq.home);
                t.goal = if (me - t.pos).len() > 25.0 {
                    police_goal_ro(w, t.pos)
                } else {
                    NO_GOAL
                };
                let _ = target;
            }
            // A new order wakes a sleeping officer (DESIGN §7.6).
            if (t.pos - me).len2() > 2.25 {
                let st = &mut w.people.stat[i][stat::FLAGS];
                if *st & flag::ASLEEP != 0 {
                    *st = (*st & !flag::ASLEEP) | flag::URGENT;
                }
            }
            w.officer_targets[o as usize] = t;
        }
    }
}

/// An existing police goal near `p` (read-only: no new goal is made).
fn police_goal_ro(w: &World, p: Vec2) -> u8 {
    for g in GOAL_POLICE as usize..MAX_GOALS {
        if w.flow.goals[g].active && (w.police.goal_pos[g] - p).len() < 30.0 {
            return g as u8;
        }
    }
    NO_GOAL
}

/// Restrain, escort and remove arrestees.
fn arrests(w: &mut World) {
    let need = w.tun.arrest_ticks.max(1.0) as u16;
    let mut k = 0;
    while k < w.police.arrests.len() {
        let (target, a) = w.police.arrests.entries()[k];
        let (ti, oi) = (w.index_of(target), w.index_of(a.officer));
        let (Some(ti), Some(oi)) = (ti, oi) else {
            w.police.arrests.remove(target);
            continue;
        };
        let d = (w.people.pos(ti) - w.people.pos(oi)).len();
        let e = w.police.arrests.get_mut(target).unwrap();
        if d < 1.3 {
            e.progress += 1;
            e.lost = 0;
        } else {
            e.lost += 1;
        }
        if e.lost > 40 {
            w.police.arrests.remove(target);
            continue;
        }
        if e.progress >= need {
            // Restrained is arrested; walking them to the van is logistics.
            w.police.arrests.remove(target);
            w.people.pose[ti][pose::GAIT] = Gait::Restrained as u8;
            w.people.stat[ti][stat::FLAGS] |= flag::DOWN;
            w.people.stat[ti][stat::FLAGS] &= !flag::ASLEEP;
            w.police.escorting.insert(a.officer, target);
            w.people.stat[oi][stat::ACTION] = Action::Escort as u8;
            w.outcome.arrests += 1;
            if w.people.stat[ti][stat::ARCH] == Archetype::Organizer as u8 {
                let p = w.people.pos(ti);
                w.outcome
                    .notable(w.tick, NotableKind::OrganizerArrested, p, target);
            }
            continue;
        }
        k += 1;
    }
    // Escorts: the arrestee follows; delivered at the van or home.
    let mut k = 0;
    while k < w.police.escorting.len() {
        let (officer, target) = w.police.escorting.entries()[k];
        let (Some(oi), Some(ti)) = (w.index_of(officer), w.index_of(target)) else {
            if let Some(ti) = w.index_of(target) {
                release(w, ti);
            }
            w.police.escorting.remove(officer);
            continue;
        };
        let op = w.people.pos(oi);
        let back = Vec2::from_heading(w.people.pose[oi][pose::HEADING]) * -0.7;
        let tp = op + back;
        if w.map.walkable_at(tp) {
            w.people.set_pos(ti, tp);
        }
        w.people.set_vel(ti, w.people.vel(oi));
        let info = w.police.officers[officer as usize];
        let sq = w.police.squads[info.squad as usize];
        let dest = if sq.van != NO_SQUAD {
            w.police.vehicles[sq.van as usize].pos
        } else {
            sq.home
        };
        if (op - dest).len() < 6.0 {
            if sq.van != NO_SQUAD {
                w.police.vehicles[sq.van as usize].cargo += 1;
            }
            w.remove(ti);
            w.police.escorting.remove(officer);
            w.people.stat[oi][stat::ACTION] = Action::Formation as u8;
            w.people.stat[oi][stat::FLAGS] |= flag::URGENT;
            continue;
        }
        k += 1;
    }
}

fn release(w: &mut World, ti: usize) {
    w.people.pose[ti][pose::GAIT] = Gait::Stand as u8;
    w.people.stat[ti][stat::FLAGS] &= !flag::DOWN;
    w.people.stat[ti][stat::FLAGS] |= flag::URGENT;
}

/// Vans and water cannons.
fn vehicles(w: &mut World) {
    let dt = DT;
    for v in 0..w.police.vehicles.len() {
        let veh = w.police.vehicles[v];
        match veh.state {
            VehicleState::Driving => {
                if veh.path_i >= veh.path_len {
                    arrive(w, v);
                    continue;
                }
                let node = w.map.roads.nodes[veh.path[veh.path_i as usize] as usize];
                let to = node - veh.pos;
                let d = to.len();
                let dir = if d > 1e-3 { to * (1.0 / d) } else { veh.ax };
                let top = if veh.kind == VehicleKind::Van {
                    8.0
                } else {
                    5.0
                };
                // Slow for people, stop for barricades.
                let ahead = veh.pos + dir * (veh.half.x + 3.0);
                let crowd = w.fields.sample(Layer::Dens, ahead.x, ahead.y);
                let blocked = w.obst.near(ahead).any(|o| {
                    let ob = w.obbs.get(o as usize);
                    ob.is_some_and(|ob| ob.kind != 1 && ob.contains(ahead))
                });
                let speed = if blocked {
                    0.0
                } else if crowd > 3.0 {
                    0.3
                } else if crowd > 1.0 {
                    1.5
                } else {
                    top
                };
                let vm = &mut w.police.vehicles[v];
                vm.speed += (speed - vm.speed).clamp(-4.0 * dt, 2.5 * dt);
                vm.ax = dir;
                if blocked {
                    vm.blocked += 1;
                    if vm.blocked > 400 {
                        // Delayed by a barricade: get out here.
                        vm.path_i = vm.path_len;
                    }
                } else {
                    vm.blocked = 0;
                }
                let step = vm.speed * dt;
                if step >= d {
                    vm.pos = node;
                    vm.path_i += 1;
                } else {
                    vm.pos += dir * step;
                }
            }
            VehicleState::Spraying => spray(w, v),
            VehicleState::Parked => {}
        }
    }
}

fn arrive(w: &mut World, v: usize) {
    let veh = w.police.vehicles[v];
    w.police.vehicles[v].state = VehicleState::Parked;
    w.police.vehicles[v].speed = 0.0;
    if veh.squad != NO_SQUAD {
        let s = veh.squad as usize;
        let at = veh.pos - veh.ax * (veh.half.x + 1.5);
        deploy_at(w, s, at);
    }
    if veh.kind == VehicleKind::WaterCannon && w.police.escalation >= Escalation::GasAndWater {
        w.police.vehicles[v].state = VehicleState::Spraying;
        if let Some(c) = w.police.clusters.first() {
            w.police.vehicles[v].spray = c.centroid;
        }
    }
}

/// Water cannon (DESIGN §2.6): a cone-shaped impulse that knocks people down
/// and soaks them.
fn spray(w: &mut World, v: usize) {
    let veh = w.police.vehicles[v];
    if w.police.escalation < Escalation::GasAndWater {
        w.police.vehicles[v].state = VehicleState::Parked;
        return;
    }
    let range = w.tun.water_range;
    let aim = veh.spray - veh.pos;
    if aim.len2() < 1e-6 {
        return;
    }
    // The jet sweeps from side to side across the target.
    let h = aim.heading() as i32 + tactics::spray_sweep(w.tick);
    let dir = Vec2::from_heading(h as u8);
    let force = w.tun.water_force;
    let nozzle = veh.pos + dir * veh.half.x;
    let tick = w.tick;
    let seed = w.seed;
    let px = riot_core::lanes::flat(&w.people.pos_x);
    let py = riot_core::lanes::flat(&w.people.pos_y);
    let mut hits = [0u32; 256];
    let mut nh = 0usize;
    w.spatial.query_radius(
        px,
        py,
        nozzle.x + dir.x * range * 0.5,
        nozzle.y + dir.y * range * 0.5,
        range * 0.55,
        |j| {
            if nh < hits.len() {
                hits[nh] = j as u32;
                nh += 1;
            }
        },
    );
    let mut hit_any = false;
    for &j in &hits[..nh] {
        let j = j as usize;
        if Archetype::from_u8(w.people.stat[j][stat::ARCH]).is_officer() {
            continue;
        }
        let d = w.people.pos(j) - nozzle;
        let dist = d.len();
        if dist > range || dist < 0.5 || d.dot(dir) < dist * 0.96 {
            continue;
        }
        hit_any = true;
        let k = force * (1.0 - dist / range);
        let nv = w.people.vel(j) + d * (k / dist);
        w.people.set_vel(j, nv);
        let e = &mut w.people.emo[j];
        e[emo::FATIGUE] = e[emo::FATIGUE].saturating_add(4);
        e[emo::FEAR] = e[emo::FEAR].saturating_add(3);
        w.people.stat[j][stat::FLAGS] =
            (w.people.stat[j][stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
        let stable = w.people.stable_id[j];
        if unit(roll_key(seed, tick, stable, 0x3a7e, Stream::Hazard)) < 0.01 * k {
            w.people.pose[j][pose::GAIT] = Gait::Fallen as u8;
            w.people.stat[j][stat::FLAGS] |= flag::DOWN;
        }
    }
    if hit_any && tick.is_multiple_of(20) {
        w.outcome
            .police_act(tick, veh.pos, veh.spray, &w.fields, 0.5, u32::MAX);
    }
    if !hit_any && tick.is_multiple_of(40) {
        w.police.vehicles[v].state = VehicleState::Parked;
    }
}
