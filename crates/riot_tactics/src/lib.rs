//! The police commander (DESIGN §14.1): the strategic layer above the squads.
//!
//! The director's clustering says where the crowds are; every replan the
//! commander decides what to do about each of them.
//!
//! - **Intelligence.** Crowds are tracked across replans (where they head,
//!   how fast, their mood, the violence around them). Where a crowd is going
//!   is read from the plan its people follow, since the state watches the
//!   group chats, and *how* it will get there from the crowd's own
//!   flow-field corridor: the police predict the route street by street.
//! - **Cordons.** A crowd heading for a protected site is met at the
//!   narrowest street on its route that a squad can reach and barricade
//!   before the crowd's front gets there. The squad sets steel barriers
//!   across the street and holds behind them. Routes price police barriers
//!   almost like walls ([`Layer::Block`]), so the crowd looks for another way
//!   round; the commander reads the new route and cordons that street too,
//!   until the crowd has to push on a barricade. A crowd pressing on a
//!   barrier topples it, faster when angry and slower when officers brace
//!   it; a breach opens the route again and the police fall back to a
//!   cordon nearer the site.
//! - **Ambushes.** Where a side street opens onto the route short of a
//!   cordon, a squad waits around the corner, crouched and unseen: it adds
//!   nothing to the threat people perceive. When the crowd fights at the
//!   barricade, the squad charges out across the street, cuts the crowd in
//!   two and sweeps the rear part back the way it came, while the
//!   barricade's officers deal with the front.
//! - **Operations.** Every crowd gets one: watch it, escort a march, cordon
//!   its route, protect the site it reached, contain it (a kettle that,
//!   once calm, lets people out through a filter gate toward a way out,
//!   with snatch squads behind its lines), or disperse it (a warning
//!   first, then lines on the protected side and gas thrown upwind of the
//!   crowd's violent core, so the wind carries it on; never into a kettle).
//! - **Adaptation.** Once barricades have fallen, a barricade with a crowd
//!   at it gets a second squad, a breached one too; ambushes
//!   that scatter a crowd are used more, ones that do not, less; tired or
//!   shaken squads are relieved by fresh reserves; escalation weighs
//!   violence against how close crowds are to the site and how the footage
//!   looks.
//!
//! Everything here runs on the director's tick (or a few cheap checks every
//! tick), allocates nothing after the riot starts, and iterates in fixed
//! orders, so it is as deterministic as the rest of the simulation.
//!
//! The commander's state is [`riot_world::commander`]; the squads that carry
//! out its plans are run by `riot_police`.

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::math::{heading_delta, widen_u8};
use riot_core::rng::{Stream, roll_key, unit};

use riot_defs::archetype::Archetype;
use riot_defs::mapdef::{EntryKind, PropKind};
use riot_defs::police::{Call, CordonState, Doctrine, NO_CORDON, NO_SITE, OpKind};
use riot_defs::setup::{Escalation, NotableKind, SquadKind, TICK_HZ};
use riot_defs::state::{Action, Gait, Item, aux, emo, flag, intent, pose, stat};
use riot_fields::{CC, Layer, NO_SLOT};
use riot_flow::NO_GOAL;
use riot_map::Map;
use riot_snapshot::prop_flag;
use riot_spatial::FIELD_CELL;
use riot_world::World;
use riot_world::commander::{
    Ambush, BARRIER_DEPTH, BARRIER_LEN, Cordon, MAX_SLOTS, MAX_TRACKS, NO_CLUSTER, NO_PROP,
    NO_TRACK, Track,
};
use riot_world::police::{Cluster, NO_SQUAD, Squad, VehicleKind, VehicleState, plan_route};
use riot_world::world::{MAX_OBJECTIVES, Prop};

/// Ticks an officer takes to set up a barrier.
const BUILD_TICKS: u8 = 40;
/// Crowds smaller than this get squads only when violent.
const MIN_POLICED: f32 = 150.0;
/// A cordon no route has used for this long is taken down (ticks).
const CORDON_IDLE: u32 = 3600;
/// Ticks between a dispersal warning and the first canister.
const WARN_TICKS: u32 = 400;
/// Widest street a squad cordons (half width, m).
const MAX_HALF_STREET: f32 = 18.0;
/// Most barricades around one site.
const MAX_CORDONS_PER_SITE: usize = 7;

// ---------------------------------------------------------------------------
// Intelligence

/// Match this replan's clusters to the tracked crowds.
pub fn observe(w: &mut World) {
    let tick = w.tick;
    let every = w.tun.director_interval.max(1.0) as u32;
    let dt = every as f32 / TICK_HZ as f32;
    let p = &mut w.police;
    let cmd = &mut p.cmd;
    for t in cmd.tracks.iter_mut() {
        t.cluster = NO_CLUSTER;
    }
    for ci in 0..p.clusters.len() {
        let c = p.clusters[ci];
        let mut best: Option<(f32, usize)> = None;
        for (k, t) in cmd.tracks.iter().enumerate() {
            if t.cluster != NO_CLUSTER {
                continue;
            }
            // Near where it should be by now, or inside where it was: a
            // stream's centroid jumps when its clusters merge or split.
            let d = (t.centroid + t.vel * dt - c.centroid).len();
            let inside = c.centroid.x > t.lo.x - 15.0
                && c.centroid.x < t.hi.x + 15.0
                && c.centroid.y > t.lo.y - 15.0
                && c.centroid.y < t.hi.y + 15.0;
            let near = d < c.radius.max(t.radius) + 60.0;
            let d = if inside { d * 0.5 } else { d };
            if (near || inside) && best.is_none_or(|b| d < b.0) {
                best = Some((d, k));
            }
        }
        let (k, fresh) = match best {
            Some((_, k)) => (k, false),
            None if cmd.tracks.len() < cmd.tracks.capacity() => {
                cmd.tracks.push(Track::new(cmd.next_id, &c, tick));
                cmd.next_id += 1;
                if c.count > 600.0 {
                    cmd.call(tick, Call::Tracking, c.centroid, c.count as u32);
                }
                (cmd.tracks.len() - 1, true)
            }
            None => continue,
        };
        let t = &mut cmd.tracks[k];
        if !fresh {
            let raw = ((c.centroid - t.centroid) * (1.0 / dt)).clamp_len(3.0);
            t.vel = t.vel.lerp(raw, 0.35);
        }
        t.centroid = c.centroid;
        t.count = c.count;
        t.radius = c.radius;
        t.lo = c.lo;
        t.hi = c.hi;
        t.anger = c.anger;
        t.threat = c.threat;
        t.cluster = ci as u16;
        t.seen = tick;
    }
    let keep = 3 * every;
    cmd.tracks.retain(|t| tick.wrapping_sub(t.seen) <= keep);
    for k in 0..cmd.tracks.len() {
        let t = cmd.tracks[k];
        let (heat, hot) = cmd.violence_near(tick, t.centroid, t.radius + 20.0);
        let t = &mut cmd.tracks[k];
        t.heat = heat;
        t.hot = hot;
    }
    for k in 0..cmd.cordons.len() {
        let c = cmd.cordons[k];
        if c.live() {
            let (v, _) = cmd.violence_near(tick, c.pos + c.face * 6.0, 18.0);
            cmd.cordons[k].violence = v;
        }
    }
}

/// The objective most of a crowd's people follow: the plan the state
/// reads off their phones. Falls back to the objective it moves toward.
fn destination(w: &World, t: &Track) -> u8 {
    let mut votes = [0u16; MAX_OBJECTIVES];
    let px = riot_core::lanes::flat(&w.people.pos_x);
    let py = riot_core::lanes::flat(&w.people.pos_y);
    let r = (t.radius * 0.6).clamp(6.0, 14.0);
    let mut n = 0u32;
    w.spatial
        .query_radius(px, py, t.centroid.x, t.centroid.y, r, |j| {
            if n >= 48 {
                return;
            }
            let st = w.people.stat[j];
            let act = Action::from_u8(st[stat::ACTION]);
            if !matches!(act, Action::March | Action::Hold | Action::Throw) {
                return;
            }
            let slot = intent::objective(w.people.intent[j]) as usize;
            if slot < MAX_OBJECTIVES && w.objectives[slot].active {
                votes[slot] += 1;
                n += 1;
            }
        });
    let mut best = (2u16, NO_GOAL);
    for (k, &v) in votes.iter().enumerate() {
        if v > best.0 {
            best = (v, k as u8);
        }
    }
    if best.1 != NO_GOAL {
        return best.1;
    }
    let speed = t.vel.len();
    let mut guess = (0.6f32, NO_GOAL);
    for (k, o) in w.objectives.iter().enumerate() {
        if !o.active {
            continue;
        }
        let to = o.pos - t.centroid;
        let d = to.len();
        let score = if d < o.radius + t.radius * 0.5 {
            2.0
        } else if speed > 0.25 {
            t.vel.dot(to) / (speed * d.max(1e-3))
        } else {
            0.0
        };
        if score > guess.0 {
            guess = (score, k as u8);
        }
    }
    guess.1
}

/// The site a crowd threatens: its destination is in it, or its route runs
/// through it.
fn threatened_site(w: &World, t: &Track) -> u8 {
    let sites = &w.police.cmd.sites;
    if t.dest != NO_GOAL
        && let Some(o) = w.objectives.get(t.dest as usize)
    {
        for (k, s) in sites.iter().enumerate() {
            if (o.pos - s.pos).len() < s.radius + o.radius * 0.5 + 10.0 {
                return k as u8;
            }
        }
    }
    for (k, s) in sites.iter().enumerate() {
        for &q in &t.route[..t.route_n as usize] {
            if (q - s.pos).len() < s.radius * 0.8 {
                return k as u8;
            }
        }
    }
    NO_SITE
}

/// Wall to wall across the street at `p`, perpendicular to `dir`: the
/// street's centre there and half its width, when walls close it on both
/// sides within `MAX_HALF_STREET`.
fn street_across(map: &Map, p: Vec2, dir: Vec2) -> Option<(Vec2, f32)> {
    let n = dir.perp();
    let reach = MAX_HALF_STREET * 1.6;
    let a = map.ray_blocked(p, p + n * reach)? * reach;
    let b = map.ray_blocked(p, p - n * reach)? * reach;
    let half = (a + b) * 0.5;
    Some((p + n * ((a - b) * 0.5), half))
}

/// A point `d` metres along a polyline, and the direction there.
fn along(route: &[Vec2], d: f32) -> Option<(Vec2, Vec2)> {
    let mut left = d;
    for k in 0..route.len().saturating_sub(1) {
        let (a, b) = (route[k], route[k + 1]);
        let seg = (b - a).len();
        if seg < 1e-3 {
            continue;
        }
        if left <= seg {
            let dir = (b - a) * (1.0 / seg);
            return Some((a + dir * left, dir));
        }
        left -= seg;
    }
    None
}

fn route_len(route: &[Vec2]) -> f32 {
    route.windows(2).map(|p| (p[1] - p[0]).len()).sum()
}

/// Read every tracked crowd: where it goes, by which route, what it
/// threatens, and what the police will do about it.
pub fn plan(w: &mut World) {
    let tick = w.tick;
    let rung = w.police.escalation;
    for k in 0..w.police.cmd.tracks.len() {
        let mut t = w.police.cmd.tracks[k];
        if t.cluster == NO_CLUSTER {
            continue;
        }
        t.dest = destination(w, &t);
        // The route, from the crowd's own corridor.
        let from = w.map.nearest_open(t.centroid, 24.0).unwrap_or(t.centroid);
        t.route_n = if t.dest != NO_GOAL {
            w.flow.route(&w.map, t.dest, from, &mut t.route) as u8
        } else {
            t.route[0] = from;
            1
        };
        if t.route_n == 1
            && t.dest != NO_GOAL
            && let Some(o) = w.objectives.get(t.dest as usize)
        {
            t.route[1] = o.pos;
            t.route_n = 2;
        }
        let dir = if t.route_n >= 2 {
            (t.route[1] - t.route[0]).normalize_or_zero()
        } else {
            t.vel.normalize_or_zero()
        };
        // The front: the farthest corner of the crowd along its route.
        let mut front = t.radius * 0.5;
        for c in [
            t.lo,
            t.hi,
            Vec2::new(t.lo.x, t.hi.y),
            Vec2::new(t.hi.x, t.lo.y),
        ] {
            front = front.max((c - t.centroid).dot(dir));
        }
        t.front = front.min(300.0);
        t.site = threatened_site(w, &t);
        t.eta = f32::INFINITY;
        if t.site != NO_SITE {
            let s = w.police.cmd.sites[t.site as usize];
            let n = t.route_n as usize;
            let mut d = 0.0;
            let mut reached = None;
            for q in 0..n {
                if (t.route[q] - s.pos).len() < s.radius {
                    reached = Some(d);
                    break;
                }
                if q + 1 < n {
                    d += (t.route[q + 1] - t.route[q]).len();
                }
            }
            let dist = reached.unwrap_or(d.max((t.centroid - s.pos).len() - s.radius));
            t.eta = (dist - t.front).max(0.0) / t.vel.len().max(0.4);
        }
        t.op = choose_op(w, &t, rung);
        w.police.cmd.tracks[k] = t;
        if t.op == OpKind::Cordon {
            cordon_route(w, k, tick);
        } else {
            w.police.cmd.tracks[k].cordon = NO_CORDON;
        }
    }
    retire_cordons(w);
}

fn choose_op(w: &World, t: &Track, rung: Escalation) -> OpKind {
    let violent = t.violent();
    let big = t.count >= MIN_POLICED;
    if rung >= Escalation::MassArrests && t.count < 1500.0 && (big || violent) {
        return OpKind::Contain;
    }
    // A kettle once begun is finished, not dropped when the trouble that
    // started it fades.
    let kettling = w.police.squads.iter().any(|sq| {
        sq.doctrine == Doctrine::Kettle && (sq.cluster_at - t.centroid).len() < t.radius + 30.0
    });
    if rung >= Escalation::LinesAndKettles && kettling && t.count < 1500.0 {
        return OpKind::Contain;
    }
    if violent && rung >= Escalation::GasAndWater {
        return OpKind::Disperse;
    }
    let at_site = t.site != NO_SITE && {
        let s = w.police.cmd.sites[t.site as usize];
        (t.centroid - s.pos).len() < s.radius + t.radius + 20.0
    };
    if violent && rung >= Escalation::LinesAndKettles && t.count < 1500.0 && !at_site {
        return OpKind::Contain;
    }
    // Cordon the route of a crowd that will be at the site within a quarter
    // of an hour; further out, it is only watched.
    let soon = t.site != NO_SITE && {
        let s = w.police.cmd.sites[t.site as usize];
        t.eta < 900.0 || (t.centroid - s.pos).len() < s.radius + 350.0
    };
    if soon && !at_site && (big || violent) {
        return OpKind::Cordon;
    }
    if at_site && (big || violent) {
        return OpKind::Protect;
    }
    if violent && rung >= Escalation::LinesAndKettles {
        return OpKind::Contain;
    }
    if !big && !violent {
        return OpKind::None;
    }
    if t.vel.len() > 0.3
        && rung <= Escalation::LinesAndKettles
        && (400.0..2500.0).contains(&t.count)
    {
        return OpKind::Escort;
    }
    OpKind::Watch
}

/// Make sure a cordon stands across the route of track `k`.
fn cordon_route(w: &mut World, k: usize, tick: u32) {
    let t = w.police.cmd.tracks[k];
    // Already cordoned: a standing cordon on the route ahead of the front.
    let n = t.route_n as usize;
    for (ci, c) in w.police.cmd.cordons.iter().enumerate() {
        if !matches!(
            c.state,
            CordonState::Planned | CordonState::Building | CordonState::Up
        ) {
            continue;
        }
        let on_route = (0..n.saturating_sub(1)).any(|q| {
            let cp = riot_core::math::closest_on_segment(c.pos, t.route[q], t.route[q + 1]);
            (cp - c.pos).len() < c.half + 3.0
        });
        if on_route {
            let c = &mut w.police.cmd.cordons[ci];
            c.needed = tick;
            c.track = t.id;
            w.police.cmd.tracks[k].cordon = ci as u8;
            return;
        }
    }
    let Some((pos, dir, half, at)) = find_chokepoint(w, &t) else {
        w.police.cmd.tracks[k].cordon = NO_CORDON;
        return;
    };
    // The same street as a cordon already there (a breached one is rebuilt).
    if let Some(ci) = w.police.cmd.cordons.iter().position(|c| {
        matches!(
            c.state,
            CordonState::Planned | CordonState::Building | CordonState::Up
        ) && (c.pos - pos).len() < 30.0
    }) {
        let c = &mut w.police.cmd.cordons[ci];
        c.needed = tick;
        c.track = t.id;
        w.police.cmd.tracks[k].cordon = ci as u8;
        return;
    }
    // A perimeter of so many barricades at most: beyond that, lines.
    let live = w
        .police
        .cmd
        .cordons
        .iter()
        .filter(|c| {
            matches!(
                c.state,
                CordonState::Planned | CordonState::Building | CordonState::Up
            ) && c.site == t.site
        })
        .count();
    if live >= MAX_CORDONS_PER_SITE {
        w.police.cmd.tracks[k].cordon = NO_CORDON;
        return;
    }
    let ambush = find_ambush(&w.map, &t.route[..n], at, half);
    let slots = ((2.0 * half / BARRIER_LEN).ceil() as usize).clamp(1, MAX_SLOTS) as u8;
    let face = -dir;
    let c = Cordon {
        state: CordonState::Planned,
        pos,
        axis: face.perp(),
        face,
        half,
        site: t.site,
        created: tick,
        needed: tick,
        slots,
        props: [NO_PROP; MAX_SLOTS],
        progress: [0; MAX_SLOTS],
        wear: [255.0; MAX_SLOTS],
        breached: 0,
        rebuilt: 0,
        ambush,
        violence: 0.0,
        track: t.id,
    };
    let cmd = &mut w.police.cmd;
    let slot = match cmd
        .cordons
        .iter()
        .position(|c| c.state == CordonState::Gone)
    {
        Some(s) => {
            cmd.cordons[s] = c;
            s
        }
        None if cmd.cordons.len() < cmd.cordons.capacity() => {
            cmd.cordons.push(c);
            cmd.cordons.len() - 1
        }
        None => return,
    };
    cmd.tracks[k].cordon = slot as u8;
    cmd.memory.cordons += 1;
    cmd.call(tick, Call::Cordon, pos, slots as u32);
}

/// Where to cordon a crowd's route short of the site it threatens: the
/// narrowest street the police can reach and barricade before the crowd's
/// front does, not too far out from the site. Returns the barrier line's
/// centre, the route's direction there, half the street's width and how
/// far along the route it is.
fn find_chokepoint(w: &World, t: &Track) -> Option<(Vec2, Vec2, f32, f32)> {
    let site = *w.police.cmd.sites.get(t.site as usize)?;
    let route = &t.route[..t.route_n as usize];
    let total = route_len(route);
    let speed = t.vel.len().max(0.5);
    let mut best: Option<(f32, Vec2, Vec2, f32, f32)> = None;
    let mut d = t.front + 20.0;
    while d < total {
        let Some((p, dir)) = along(route, d) else {
            break;
        };
        let ds = (p - site.pos).len();
        if ds < site.radius + 6.0 {
            break;
        }
        // Not where a cordon was given up, nor at one the crowd has broken
        // through: the police fall back to the next street.
        let tried = |c: Vec2| {
            w.police.cmd.abandoned.iter().any(|&(q, t)| {
                t != 0 && w.tick.wrapping_sub(t) < 6000 && (q - c).len2() < 30.0 * 30.0
            }) || w
                .police
                .cmd
                .cordons
                .iter()
                .any(|k| k.state == CordonState::Breached && (k.pos - c).len2() < 30.0 * 30.0)
        };
        if ds < site.radius + 180.0
            && let Some((c, half)) = street_across(&w.map, p, dir)
            && (2.0..=MAX_HALF_STREET).contains(&half)
            && w.map.walkable_at(c)
            && !tried(c)
        {
            // Can a squad get there and set it up first?
            let mut reach = f32::INFINITY;
            for sq in &w.police.squads {
                if sq.deployed && sq.present > 0 && sq.doctrine != Doctrine::Retreat {
                    reach = reach.min((sq.pos - c).len());
                }
            }
            let slots = (2.0 * half / BARRIER_LEN).ceil();
            let police_t = reach * 1.4 / 2.6 + slots.min(4.0) * BUILD_TICKS as f32 / TICK_HZ as f32;
            let crowd_t = (d - t.front) / speed;
            let late = (police_t - crowd_t).max(0.0);
            let score = half * 2.0 + 0.15 * (ds - site.radius - 40.0).abs() + late * 3.0;
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, c, dir, half, d));
            }
        }
        d += 4.0;
    }
    best.map(|b| (b.1, b.2, b.3, b.4))
}

/// A side street opening onto the route 18–60 m short of a cordon `at`
/// metres along it, out of sight of the approaching crowd: where an
/// ambush waits.
fn find_ambush(map: &Map, route: &[Vec2], at: f32, half: f32) -> Option<Ambush> {
    let mut best: Option<(f32, Ambush)> = None;
    let mut d = (at - 60.0).max(0.0);
    while d <= at - 18.0 {
        let Some((q, dir)) = along(route, d) else {
            d += 3.0;
            continue;
        };
        for side in [dir.perp(), -dir.perp()] {
            let deep = q + side * (half.max(4.0) + 14.0);
            if map.ray_blocked(q, deep).is_some() || !map.walkable_at(deep) {
                continue;
            }
            // Around the corner: the crowd coming up the route cannot see it.
            let watcher = q - dir * 24.0;
            if map.ray_blocked(watcher, deep).is_none() {
                continue;
            }
            let score = (d - (at - 35.0)).abs();
            if best.is_none_or(|b| score < b.0) {
                best = Some((
                    score,
                    Ambush {
                        pos: deep,
                        junction: q,
                        sprung: 0,
                        at_spring: 0.0,
                        scored: false,
                    },
                ));
            }
        }
        d += 3.0;
    }
    best.map(|b| b.1)
}

/// Take down cordons nobody needs any more: no crowd threatening their
/// site has come within 300 m of them for a while.
fn retire_cordons(w: &mut World) {
    let tick = w.tick;
    for ci in 0..w.police.cmd.cordons.len() {
        let c = w.police.cmd.cordons[ci];
        if !c.live() {
            continue;
        }
        let threatened = w.police.cmd.tracks.iter().any(|t| {
            t.cluster != NO_CLUSTER
                && t.site == c.site
                && t.count >= MIN_POLICED * 0.5
                && (t.centroid - c.pos).len() < 300.0
        });
        // Never finished: the crowd got there first, or the squads cannot
        // get to it. Give it up (the next street back is cordoned instead).
        // Civilians only: the builders stand there too.
        let front = c.pos + c.face * 3.0;
        let overrun = w.fields.sample(Layer::Weight, front.x, front.y) > 1.0
            || w.fields.sample(Layer::Weight, c.pos.x, c.pos.y) > 1.0;
        let age = tick.wrapping_sub(c.created);
        let stalled = matches!(c.state, CordonState::Planned | CordonState::Building)
            && (age > 7200 || age > 600 && overrun);
        if threatened && !stalled {
            w.police.cmd.cordons[ci].needed = tick;
            continue;
        }
        if !stalled && tick.wrapping_sub(c.needed) < CORDON_IDLE {
            continue;
        }
        // Not while a crowd still stands at it.
        let crowd = w.fields.sample(
            Layer::Weight,
            c.pos.x + c.face.x * 5.0,
            c.pos.y + c.face.y * 5.0,
        );
        if crowd > 0.5 && !stalled {
            w.police.cmd.cordons[ci].needed = tick;
            continue;
        }
        for s in 0..c.slots as usize {
            let k = c.props[s];
            if k != NO_PROP {
                let pr = &mut w.props[k as usize];
                pr.flags |= prop_flag::DESTROYED | prop_flag::REMOVED;
            }
        }
        w.props_dirty = true;
        let cmd = &mut w.police.cmd;
        cmd.cordons[ci].state = CordonState::Gone;
        cmd.dirty = true;
        cmd.call(tick, Call::Dismantle, c.pos, c.slots as u32);
        if stalled {
            let k = (c.created as usize) % cmd.abandoned.len();
            cmd.abandoned[k] = (c.pos, tick);
        }
        for t in cmd.tracks.iter_mut() {
            if t.cordon == ci as u8 {
                t.cordon = NO_CORDON;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Assignment

/// What a squad is asked for, for picking among free squads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    /// Line or riot squads, line first.
    Line,
    /// Riot squads first.
    Riot,
    /// Snatch squads only.
    Snatch,
}

/// The nearest free squad for a job at `at`, preferring the squads already
/// on this crowd or cordon.
fn pick(
    w: &World,
    taken: &[bool],
    at: Vec2,
    want: Want,
    track: u32,
    cordon: u8,
    rung: Escalation,
) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (k, sq) in w.police.squads.iter().enumerate().take(taken.len()) {
        if taken[k] {
            continue;
        }
        let mut d = (sq.pos - at).len();
        if (track != NO_TRACK && sq.track == track) || (cordon != NO_CORDON && sq.cordon == cordon)
        {
            d *= 0.2;
        }
        match (want, sq.kind) {
            (Want::Snatch, SquadKind::Snatch) => {}
            (Want::Snatch, _) => continue,
            (Want::Line, SquadKind::Riot) | (Want::Riot, SquadKind::Line) => d += 400.0,
            // Snatch squads fill lines only when nobody else can, and not
            // before arrests are allowed.
            (_, SquadKind::Snatch) if rung < Escalation::BatonsAndArrests => d += 50_000.0,
            (_, SquadKind::Snatch) => d += 5000.0,
            _ => {}
        }
        d += 300.0 * sq.fatigue + 600.0 * (0.8 - sq.morale).max(0.0);
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, k));
        }
    }
    best.map(|b| b.1)
}

/// Give every squad its job for the next replan.
pub fn assign(w: &mut World) {
    let tick = w.tick;
    let rung = w.police.escalation;
    let nsq = w.police.squads.len().min(256);
    let mut taken = [false; 256];
    let mut kept = [false; 256];
    for k in 0..nsq {
        let sq = w.police.squads[k];
        if !sq.deployed || sq.present == 0 {
            taken[k] = true;
            continue;
        }
        if sq.morale < 0.3 {
            let sq = &mut w.police.squads[k];
            set_doctrine(sq, Doctrine::Retreat, tick);
            sq.anchor = sq.home;
            sq.track = NO_TRACK;
            sq.cordon = NO_CORDON;
            taken[k] = true;
            continue;
        }
        // Jobs that outlast a replan.
        let keep = match sq.doctrine {
            Doctrine::Barricade | Doctrine::Ambush => w
                .police
                .cmd
                .cordons
                .get(sq.cordon as usize)
                .is_some_and(|c| {
                    c.live()
                        && (sq.doctrine == Doctrine::Barricade
                            || c.ambush.is_some_and(|a| a.sprung == 0))
                }),
            Doctrine::Charge => true,
            Doctrine::Snatch => sq.target_person != u32::MAX,
            _ => false,
        };
        if keep {
            taken[k] = true;
            kept[k] = true;
        }
    }
    // Cordons: builders first (a breached cordon keeps its squads, who
    // fight at the line), then extra lines where it is hot, then ambushes.
    let ambush_odds = {
        let m = w.police.cmd.memory;
        (0.45 + 0.15 * (m.ambush_wins as f32 - m.ambush_losses as f32) + 0.4 * w.police.aggression)
            .clamp(0.15, 0.95)
    };
    for ci in 0..w.police.cmd.cordons.len() {
        let c = w.police.cmd.cordons[ci];
        if !c.live() {
            continue;
        }
        let have = (0..nsq)
            .filter(|&k| {
                kept[k]
                    && w.police.squads[k].cordon == ci as u8
                    && w.police.squads[k].doctrine == Doctrine::Barricade
            })
            .count();
        // One squad holds a barricade; a breached one, or one under attack,
        // gets more, and once the police have seen barricades fall, a
        // crowd at one is enough.
        let front = c.pos + c.face * 5.0;
        let crowded = w.fields.sample(Layer::Weight, front.x, front.y) > 0.5;
        let breaches = w.police.cmd.memory.breaches;
        let mut want = 1 + usize::from(c.state == CordonState::Breached);
        if crowded && (c.violence > 4.0 || breaches > 0) && rung >= Escalation::LinesAndKettles {
            want += 1;
        }
        let want = want.min(3);
        for n in have..want {
            let kind = if n == 0 { Want::Line } else { Want::Riot };
            let Some(k) = pick(w, &taken, c.line(), kind, c.track, ci as u8, rung) else {
                break;
            };
            taken[k] = true;
            let sq = &mut w.police.squads[k];
            set_doctrine(sq, Doctrine::Barricade, tick);
            sq.cordon = ci as u8;
            sq.track = c.track;
            sq.assigned = tick;
            sq.anchor = c.line();
            sq.facing = c.face;
            if n > 0 {
                w.police.cmd.call(tick, Call::FallBack, c.line(), k as u32);
            }
        }
        // An ambush: hiding is not force, so squads take their corners as
        // soon as the cordon is planned; they charge only once batons are
        // allowed. Only a squad near enough to get round there in time.
        if let Some(a) = c.ambush
            && a.sprung == 0
            && !(0..nsq).any(|k| {
                w.police.squads[k].cordon == ci as u8
                    && w.police.squads[k].doctrine == Doctrine::Ambush
            })
            && unit(roll_key(w.seed, 0, ci as u32, c.created, Stream::Police)) < ambush_odds
            && let Some(k) = pick(w, &taken, a.pos, Want::Riot, c.track, ci as u8, rung)
            && (w.police.squads[k].pos - a.pos).len() < 220.0
        {
            taken[k] = true;
            let sq = &mut w.police.squads[k];
            set_doctrine(sq, Doctrine::Ambush, tick);
            sq.cordon = ci as u8;
            sq.track = c.track;
            sq.anchor = a.pos;
            sq.facing = (a.junction - a.pos).normalize_or_zero();
            sq.assigned = tick;
            w.police.cmd.call(tick, Call::AmbushSet, a.pos, k as u32);
        }
    }
    // Crowds, most dangerous first.
    let mut order = [0usize; MAX_TRACKS];
    let nt = w.police.cmd.tracks.len().min(MAX_TRACKS);
    for (k, o) in order.iter_mut().enumerate().take(nt) {
        *o = k;
    }
    let prio = |t: &Track| {
        let site = if t.site != NO_SITE { 2.0 } else { 1.0 };
        t.count * (0.3 + t.anger) * site + t.heat * 60.0
    };
    {
        let tracks = &w.police.cmd.tracks;
        order[..nt].sort_by(|&a, &b| {
            prio(&tracks[b])
                .total_cmp(&prio(&tracks[a]))
                .then(tracks[a].id.cmp(&tracks[b].id))
        });
    }
    // Snatch targets handed out this pass: one squad per person.
    let mut targets = [u32::MAX; 64];
    let mut ntargets = 0;
    for &k in &order[..nt] {
        let t = w.police.cmd.tracks[k];
        if t.cluster == NO_CLUSTER || t.op == OpKind::None {
            continue;
        }
        let ci = t.cluster as usize;
        let c = w.police.clusters[ci];
        match t.op {
            OpKind::Contain => contain(w, &mut taken, &t, ci, rung),
            OpKind::Disperse | OpKind::Protect | OpKind::Watch | OpKind::Cordon => {
                lines(w, &mut taken, &t, ci, rung)
            }
            OpKind::Escort => escort(w, &mut taken, &t, ci, rung),
            OpKind::None => {}
        }
        // Snatch squads follow the lines once arrests are allowed.
        if rung >= Escalation::BatonsAndArrests {
            let staging = staging_for(w, &t);
            for _ in 0..2 {
                let Some(k) = pick(w, &taken, staging, Want::Snatch, t.id, NO_CORDON, rung) else {
                    break;
                };
                taken[k] = true;
                let from = w.police.squads[k].pos;
                let near_front = (from - staging).len() < 40.0;
                let target = if near_front {
                    pick_snatch_target(w, &c, from, &targets[..ntargets], rung)
                } else {
                    u32::MAX
                };
                let sq = &mut w.police.squads[k];
                sq.track = t.id;
                sq.cluster = ci as u16;
                sq.cluster_at = c.centroid;
                sq.assigned = tick;
                if target != u32::MAX {
                    if ntargets < targets.len() {
                        targets[ntargets] = target;
                        ntargets += 1;
                    }
                    set_doctrine(sq, Doctrine::Snatch, tick);
                    sq.target_person = target;
                    let at = sq.pos;
                    w.police.cmd.call(tick, Call::Snatch, at, target);
                } else {
                    set_doctrine(sq, Doctrine::Reserve, tick);
                    sq.anchor = staging;
                    sq.facing = (c.centroid - staging).normalize_or_zero();
                }
                if target == u32::MAX {
                    break;
                }
            }
        }
    }
    // Everyone else: hold a while where they were, then stand down.
    for k in 0..nsq {
        if taken[k] {
            continue;
        }
        let sq = &mut w.police.squads[k];
        sq.cluster = u16::MAX;
        let recent = sq.cluster_at.x != f32::MAX
            && tick.saturating_sub(sq.assigned) < 1200
            && sq.doctrine != Doctrine::Reserve;
        if recent {
            if matches!(
                sq.doctrine,
                Doctrine::Snatch | Doctrine::Kettle | Doctrine::Escort | Doctrine::Barricade
            ) {
                set_doctrine(sq, Doctrine::HoldLine, tick);
                sq.anchor = sq.pos;
            }
        } else {
            set_doctrine(sq, Doctrine::Reserve, tick);
            sq.anchor = sq.home;
            sq.track = NO_TRACK;
            sq.cordon = NO_CORDON;
        }
    }
    relieve(w, &taken, nsq);
}

fn set_doctrine(sq: &mut Squad, d: Doctrine, tick: u32) {
    if sq.doctrine != d {
        sq.doctrine = d;
        sq.since = tick;
        if d != Doctrine::Snatch {
            sq.target_person = u32::MAX;
        }
        if !matches!(d, Doctrine::Barricade | Doctrine::Ambush | Doctrine::Charge) {
            sq.cordon = NO_CORDON;
        }
    }
}

/// Which way a crowd should be pushed, and from where: lines stand
/// between it and what the police protect (the site it threatens, else the
/// nearest site, else the police's own side).
fn away_for(w: &World, t: &Track) -> Vec2 {
    let target = if t.site != NO_SITE {
        Some(w.police.cmd.sites[t.site as usize].pos)
    } else {
        w.police.cmd.sites.iter().map(|s| s.pos).min_by(|a, b| {
            (*a - t.centroid)
                .len2()
                .total_cmp(&(*b - t.centroid).len2())
        })
    };
    let away = match target {
        Some(g) => (g - t.centroid).normalize_or_zero(),
        None => Vec2::new(1.0, 0.0),
    };
    if away == Vec2::ZERO {
        Vec2::new(1.0, 0.0)
    } else {
        away
    }
}

/// Where snatch squads wait for a target: just behind the nearest police
/// line in the open facing this crowd (not behind barriers, which nobody
/// can reach through), else off the crowd's flank.
fn staging_for(w: &World, t: &Track) -> Vec2 {
    let mut best: Option<(f32, Vec2)> = None;
    for sq in &w.police.squads {
        if sq.track != t.id
            || sq.present == 0
            || !matches!(
                sq.doctrine,
                Doctrine::HoldLine
                    | Doctrine::Advance
                    | Doctrine::Disperse
                    | Doctrine::Kettle
                    | Doctrine::Escort
                    | Doctrine::Charge
            )
        {
            continue;
        }
        let d = (sq.pos - t.centroid).len();
        if best.is_none_or(|b| d < b.0) {
            let behind = sq.pos + (sq.pos - t.centroid).normalize_or_zero() * 4.0;
            best = Some((d, behind));
        }
    }
    if let Some((_, p)) = best {
        return p;
    }
    let side = t.vel.normalize_or_zero().perp();
    let side = if side == Vec2::ZERO {
        away_for(w, t).perp()
    } else {
        side
    };
    t.centroid + side * (t.radius + 15.0)
}

/// Lines between a crowd and what the police protect: watch at a
/// distance, protect the site, disperse (riot squads advancing), or back a
/// cordon up.
fn lines(w: &mut World, taken: &mut [bool], t: &Track, ci: usize, rung: Escalation) {
    let tick = w.tick;
    let c = w.police.clusters[ci];
    let violent = t.violent();
    let (want, dist) = match t.op {
        OpKind::Watch => (((t.count / 600.0).ceil() as usize).clamp(1, 3), 25.0),
        OpKind::Protect => (((t.count / 450.0).ceil() as usize).clamp(1, 6), 8.0),
        OpKind::Disperse => (((t.count / 350.0).ceil() as usize).clamp(1, 8), 10.0),
        // The cordon holds the route; a line watches the crowd's flank. No
        // barricade (too many already): a line meets it instead.
        OpKind::Cordon if t.cordon == NO_CORDON => {
            (((t.count / 600.0).ceil() as usize).clamp(1, 3), 12.0)
        }
        OpKind::Cordon => (usize::from(t.count > 1500.0 || violent), 18.0),
        _ => (0, 20.0),
    };
    let away = away_for(w, t);
    let kind = if t.op == OpKind::Disperse || violent {
        Want::Riot
    } else {
        Want::Line
    };
    let mut picked = [usize::MAX; 8];
    let mut np = 0;
    for _ in 0..want {
        let Some(k) = pick(
            w,
            taken,
            c.centroid + away * c.radius,
            kind,
            t.id,
            NO_CORDON,
            rung,
        ) else {
            break;
        };
        taken[k] = true;
        picked[np] = k;
        np += 1;
    }
    for (j, &k) in picked[..np].iter().enumerate() {
        let sq = &mut w.police.squads[k];
        let side = away.perp() * ((j as f32 - (np as f32 - 1.0) * 0.5) * (sq.width + 2.0));
        let line_anchor = c.centroid + away * (c.radius + dist) + side;
        let doctrine = match sq.kind {
            SquadKind::Riot if t.op == OpKind::Disperse => Doctrine::Disperse,
            SquadKind::Riot if rung >= Escalation::BatonsAndArrests && violent => Doctrine::Advance,
            _ => Doctrine::HoldLine,
        };
        if sq.doctrine != doctrine || (sq.anchor - line_anchor).len() > 30.0 {
            sq.anchor = line_anchor;
        }
        set_doctrine(sq, doctrine, tick);
        sq.cluster = ci as u16;
        sq.cluster_at = c.centroid;
        sq.track = t.id;
        sq.assigned = tick;
        sq.facing = -away;
    }
}

/// Squads walking alongside a march, on the side nearest to them.
fn escort(w: &mut World, taken: &mut [bool], t: &Track, ci: usize, rung: Escalation) {
    let tick = w.tick;
    let c = w.police.clusters[ci];
    let heading = t.vel.normalize_or_zero();
    let want = if t.count > 1200.0 { 2 } else { 1 };
    let escorted = w
        .police
        .squads
        .iter()
        .any(|q| q.doctrine == Doctrine::Escort && q.track == t.id);
    for j in 0..want {
        let side = if j == 0 {
            heading.perp()
        } else {
            -heading.perp()
        };
        let at = c.centroid + side * (c.radius + 10.0) + heading * 6.0;
        let Some(k) = pick(w, taken, at, Want::Line, t.id, NO_CORDON, rung) else {
            break;
        };
        // Nobody walks half the city to escort a march.
        if (w.police.squads[k].pos - at).len() > 250.0 && w.police.squads[k].track != t.id {
            break;
        }
        taken[k] = true;
        if !escorted && j == 0 {
            w.police
                .cmd
                .call(tick, Call::Escort, c.centroid, t.count as u32);
        }
        // The side this squad is already on.
        let sq = &mut w.police.squads[k];
        let side = if (sq.pos - c.centroid).dot(heading.perp()) >= 0.0 {
            heading.perp()
        } else {
            -heading.perp()
        };
        let sq = &mut w.police.squads[k];
        set_doctrine(sq, Doctrine::Escort, tick);
        sq.anchor = c.centroid + side * (c.radius + 10.0) + heading * 6.0;
        sq.facing = -side;
        sq.cluster = ci as u16;
        sq.cluster_at = c.centroid;
        sq.track = t.id;
        sq.assigned = tick;
    }
}

/// Ring a crowd: enough squads to close a kettle around it with officers
/// at most 1.4 m apart.
fn contain(w: &mut World, taken: &mut [bool], t: &Track, ci: usize, rung: Escalation) {
    let tick = w.tick;
    let c = w.police.clusters[ci];
    let packed_r = (c.count / (2.5 * core::f32::consts::PI)).sqrt().max(4.0);
    let want = ((c.count / 350.0).ceil() as usize)
        .max((2.0 * core::f32::consts::PI * packed_r / 1.4 / 10.0).ceil() as usize)
        .clamp(1, 16);
    let mut picked = [usize::MAX; 16];
    let mut np = 0;
    for _ in 0..want {
        let Some(k) = pick(w, taken, c.centroid, Want::Line, t.id, NO_CORDON, rung) else {
            break;
        };
        taken[k] = true;
        picked[np] = k;
        np += 1;
    }
    // One squad cannot ring anyone: it holds a line instead.
    let ring = np >= 2 || rung >= Escalation::MassArrests;
    let away = away_for(w, t);
    let fresh = t.kettled == 0
        && !picked[..np]
            .iter()
            .any(|&k| w.police.squads[k].doctrine == Doctrine::Kettle);
    if ring && fresh {
        w.police
            .cmd
            .call(tick, Call::Kettle, c.centroid, c.count as u32);
    }
    for (j, &k) in picked[..np].iter().enumerate() {
        let sq = &mut w.police.squads[k];
        if ring {
            if sq.doctrine != Doctrine::Kettle || sq.cluster != ci as u16 {
                sq.radius = c.radius + 6.0;
            }
            set_doctrine(sq, Doctrine::Kettle, tick);
            sq.anchor = c.centroid;
            sq.ring = (j as u8, np as u8);
            sq.sweep = t.gate;
        } else {
            set_doctrine(sq, Doctrine::HoldLine, tick);
            let line_anchor = c.centroid + away * (c.radius + 10.0);
            if (sq.anchor - line_anchor).len() > 30.0 {
                sq.anchor = line_anchor;
            }
        }
        sq.cluster = ci as u16;
        sq.cluster_at = c.centroid;
        sq.track = t.id;
        sq.assigned = tick;
        sq.facing = -away;
    }
}

/// A fresh reserve squad takes over from the most worn-out squad on a
/// line, which goes back to rest.
fn relieve(w: &mut World, taken: &[bool], nsq: usize) {
    let tick = w.tick;
    let relief = w.tun.relief_fatigue;
    let mut worst: Option<(f32, usize)> = None;
    for k in 0..nsq {
        let sq = &w.police.squads[k];
        if !taken[k]
            || !matches!(
                sq.doctrine,
                Doctrine::HoldLine | Doctrine::Barricade | Doctrine::Kettle | Doctrine::Escort
            )
        {
            continue;
        }
        let worn = (sq.fatigue - relief).max(0.0) * 2.0
            + (0.5 - sq.morale).max(0.0) * 2.0
            + (0.5 - sq.present as f32 / sq.n.max(1) as f32).max(0.0);
        if worn > 0.0 && worst.is_none_or(|b| worn > b.0) {
            worst = Some((worn, k));
        }
    }
    let Some((_, tired)) = worst else { return };
    let at = w.police.squads[tired].pos;
    let mut fresh: Option<(f32, usize)> = None;
    for k in 0..nsq {
        let sq = &w.police.squads[k];
        if taken[k]
            || !sq.deployed
            || sq.kind == SquadKind::Snatch
            || sq.doctrine != Doctrine::Reserve
            || sq.fatigue > relief * 0.5
            || sq.morale < 0.6
            || (sq.present as u32) * 4 < sq.n as u32 * 3
        {
            continue;
        }
        let d = (sq.pos - at).len();
        if d < 500.0 && fresh.is_none_or(|b| d < b.0) {
            fresh = Some((d, k));
        }
    }
    let Some((_, k)) = fresh else { return };
    let old = w.police.squads[tired];
    let sq = &mut w.police.squads[k];
    sq.doctrine = old.doctrine;
    sq.since = tick;
    sq.anchor = old.anchor;
    sq.facing = old.facing;
    sq.radius = old.radius;
    sq.ring = old.ring;
    sq.cluster = old.cluster;
    sq.cluster_at = old.cluster_at;
    sq.track = old.track;
    sq.cordon = old.cordon;
    sq.sweep = old.sweep;
    sq.assigned = tick;
    let t = &mut w.police.squads[tired];
    t.doctrine = Doctrine::Reserve;
    t.since = tick;
    t.anchor = t.home;
    t.track = NO_TRACK;
    t.cordon = NO_CORDON;
    t.cluster = u16::MAX;
    w.police.cmd.memory.reliefs += 1;
    w.police.cmd.call(tick, Call::Relief, at, k as u32);
}

/// Someone for the snatch squad at `from` to arrest, nearest the squad and
/// close enough to reach through the edge of the crowd, by priority: people
/// carrying a molotov, people filmed or spotted starting violence (more so
/// recently), and known organizers (first of all once the state goes for
/// mass arrests). Never a target another squad has, or the state's own.
pub fn pick_snatch_target(
    w: &World,
    c: &Cluster,
    from: Vec2,
    taken: &[u32],
    rung: Escalation,
) -> u32 {
    const REACH: f32 = 25.0;
    let reach = c.radius + 40.0;
    let tick = w.tick;
    let mut best: Option<(f32, u32)> = None;
    let mut consider = |s: u32, priority: f32| {
        let Some(i) = w.index_of(s) else { return };
        if w.police.arrests.contains(s)
            || taken.contains(&s)
            || Gait::from_u8(w.people.pose[i][pose::GAIT]) == Gait::Restrained
            || Archetype::from_u8(w.people.stat[i][stat::ARCH]) == Archetype::Provocateur
            || (w.people.pos(i) - c.centroid).len() > reach
        {
            return;
        }
        let p = w.people.pos(i);
        let d = (p - from).len();
        if d >= REACH {
            return;
        }
        // Deep in a packed crowd is a risky grab.
        let packed = w.fields.sample(Layer::Dens, p.x, p.y);
        let score = priority / (1.0 + d * 0.15) / (1.0 + (packed - 4.0).max(0.0) * 0.3);
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, s));
        }
    };
    let organizer_priority = if rung >= Escalation::MassArrests {
        6.0
    } else {
        1.5
    };
    for &s in &w.police.organizers {
        if w.index_of(s)
            .is_some_and(|i| w.people.flags(i) & flag::KNOWN != 0)
        {
            consider(s, organizer_priority);
        }
    }
    for &(s, _) in w.police.watch.entries() {
        let cold = &w.cold[s as usize];
        let mut pr = 1.0 + cold.filmed.min(5) as f32 * 0.4;
        match Item::from_u8(cold.item) {
            Item::Molotov => pr += 3.0,
            Item::Bottle => pr += 1.0,
            _ => {}
        }
        if cold.last_act != 0 && tick.wrapping_sub(cold.last_act) < 600 {
            pr += 1.5;
        }
        consider(s, pr);
    }
    best.map_or(u32::MAX, |b| b.1)
}

// ---------------------------------------------------------------------------
// Every tick: barriers, breaches, ambushes, kettles

pub fn tick(w: &mut World) {
    build_barriers(w);
    if w.tick % 10 == 3 {
        breach(w);
    }
    if w.tick % 5 == 1 {
        spring_ambushes(w);
    }
    kettles(w);
    if w.police.cmd.dirty {
        restamp(w);
    }
}

/// Builder squads set up one barrier per officer at a time, each officer at
/// the slot it is nearest in line for.
fn build_barriers(w: &mut World) {
    let tick = w.tick;
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        if sq.doctrine != Doctrine::Barricade || sq.present == 0 {
            continue;
        }
        let ci = sq.cordon as usize;
        let Some(&c) = w.police.cmd.cordons.get(ci) else {
            continue;
        };
        if !c.live() {
            continue;
        }
        let n = sq.n.max(1) as usize;
        let mut missing = 0;
        let mut done_now = 0;
        for s in 0..c.slots as usize {
            if c.standing(&w.props, s) {
                continue;
            }
            missing += 1;
            // A cordon that keeps falling is not put back up forever.
            if c.props[s] != NO_PROP && c.rebuilt as usize >= 2 * c.slots as usize {
                continue;
            }
            let o = sq.officers[s % n];
            let Some(i) = w.index_of(o) else { continue };
            if (w.people.pos(i) - c.slot_post(s)).len() > 1.4
                || Gait::from_u8(w.people.pose[i][pose::GAIT]).is_down()
            {
                continue;
            }
            let cm = &mut w.police.cmd.cordons[ci];
            cm.progress[s] = cm.progress[s].saturating_add(1);
            if cm.progress[s] < BUILD_TICKS {
                continue;
            }
            // Never on top of someone.
            let at = c.slot_pos(s);
            let seg = 2.0 * c.half / c.slots.max(1) as f32;
            let half = Vec2::new(seg * 0.5 + 0.08, BARRIER_DEPTH);
            let px = riot_core::lanes::flat(&w.people.pos_x);
            let py = riot_core::lanes::flat(&w.people.pos_y);
            let mut clear = true;
            w.spatial
                .query_radius(px, py, at.x, at.y, half.x + 0.6, |j| {
                    let d = Vec2::new(px[j], py[j]) - at;
                    if d.dot(c.axis).abs() < half.x + 0.2 && d.dot(c.face).abs() < half.y + 0.3 {
                        clear = false;
                    }
                });
            if !clear || w.props.len() >= w.props.capacity() {
                continue;
            }
            let idx = w.props.len() as u16;
            w.props.push(Prop {
                kind: PropKind::PoliceBarrier,
                pos: at,
                half,
                ax: c.axis,
                health: 255,
                flags: 0,
                build: 255,
            });
            w.props_dirty = true;
            let cm = &mut w.police.cmd.cordons[ci];
            if cm.props[s] != NO_PROP {
                cm.rebuilt += 1;
            }
            cm.props[s] = idx;
            cm.progress[s] = 0;
            cm.wear[s] = 255.0;
            w.police.cmd.dirty = true;
            done_now += 1;
        }
        let cm = &mut w.police.cmd.cordons[ci];
        let was = cm.state;
        let left = missing - done_now;
        if left == 0 && matches!(was, CordonState::Planned | CordonState::Building) {
            cm.state = CordonState::Up;
            let (pos, slots) = (cm.pos, cm.slots);
            w.police
                .cmd
                .call(tick, Call::BarricadeUp, pos, slots as u32);
            w.outcome
                .notable(tick, NotableKind::BarricadeUp, pos, u32::MAX);
        } else if left == 0 && was == CordonState::Breached {
            cm.state = CordonState::Up;
        } else if was == CordonState::Planned && left < c.slots as usize {
            cm.state = CordonState::Building;
        }
    }
}

/// A crowd pressing on a barrier wears it down, faster the angrier and
/// harder it pushes; officers bracing it from behind take most of that off.
/// A toppled barrier opens the street: the crowd cheers.
fn breach(w: &mut World) {
    let tick = w.tick;
    let push_k = w.tun.barrier_push;
    let brace_k = w.tun.barrier_brace;
    let px = riot_core::lanes::flat(&w.people.pos_x);
    let py = riot_core::lanes::flat(&w.people.pos_y);
    for ci in 0..w.police.cmd.cordons.len() {
        let c = w.police.cmd.cordons[ci];
        if !c.live() {
            continue;
        }
        for s in 0..c.slots as usize {
            if !c.standing(&w.props, s) {
                continue;
            }
            let pr = w.props[c.props[s] as usize];
            let (mut push, mut brace, mut pinned) = (0.0f32, 0u32, 0u32);
            let crush = w.tun.crush_pressure;
            let people = &w.people;
            w.spatial
                .query_radius(px, py, pr.pos.x, pr.pos.y, pr.half.x + 1.6, |j| {
                    let st = people.stat[j];
                    if st[stat::FLAGS] & (flag::DEAD | flag::DOWN) != 0 {
                        return;
                    }
                    let d = Vec2::new(px[j], py[j]) - pr.pos;
                    if d.dot(c.axis).abs() > pr.half.x + 0.3 {
                        return;
                    }
                    let across = d.dot(c.face);
                    if Archetype::from_u8(st[stat::ARCH]).is_officer() {
                        if across < 0.0 && across > -(pr.half.y + 1.5) {
                            brace += 1;
                        }
                    } else if across > 0.0 && across < pr.half.y + 0.8 {
                        let e = people.emo[j];
                        let anger = widen_u8(e[emo::ANGER]);
                        let pressure = widen_u8(people.pose[j][pose::PRESSURE]);
                        if pressure > crush {
                            pinned += 1;
                        }
                        let mut p = pressure * anger * 2.0 + (anger - 0.4).max(0.0);
                        if matches!(
                            Action::from_u8(st[stat::ACTION]),
                            Action::Throw | Action::Barricade
                        ) {
                            p += 0.3;
                        }
                        push += p;
                    }
                });
            // Crowd safety first: people crushed against a barrier, and the
            // police take it away before anyone dies.
            if pinned >= 4 {
                let prm = &mut w.props[c.props[s] as usize];
                prm.flags |= prop_flag::DESTROYED | prop_flag::REMOVED;
                w.props_dirty = true;
                let cm = &mut w.police.cmd.cordons[ci];
                cm.state = CordonState::Breached;
                cm.breached = tick;
                w.police.cmd.dirty = true;
                w.police.cmd.call(tick, Call::Opened, pr.pos, pinned);
                continue;
            }
            if push <= 0.0 {
                continue;
            }
            let dmg = push_k * push * (1.0 - brace_k * brace as f32).max(0.15);
            let cm = &mut w.police.cmd.cordons[ci];
            cm.wear[s] = cm.wear[s].min(pr.health as f32) - dmg;
            let left = cm.wear[s];
            let prm = &mut w.props[c.props[s] as usize];
            if left > 0.0 {
                prm.health = left.ceil().min(255.0) as u8;
                continue;
            }
            // Down.
            prm.health = 0;
            prm.flags |= prop_flag::DESTROYED;
            w.props_dirty = true;
            let first = cm.state != CordonState::Breached;
            cm.state = CordonState::Breached;
            cm.breached = tick;
            w.police.cmd.dirty = true;
            if first {
                w.police.cmd.memory.breaches += 1;
                w.police.cmd.call(tick, Call::Breach, pr.pos, ci as u32);
                w.outcome
                    .notable(tick, NotableKind::BarrierBreached, pr.pos, u32::MAX);
            }
            // The crowd cheers.
            let mut cheer = [0u32; 64];
            let mut nc = 0;
            w.spatial
                .query_radius(px, py, pr.pos.x, pr.pos.y, 8.0, |j| {
                    if nc < cheer.len() {
                        cheer[nc] = j as u32;
                        nc += 1;
                    }
                });
            for &j in &cheer[..nc] {
                let j = j as usize;
                if Archetype::from_u8(w.people.stat[j][stat::ARCH]).is_officer() {
                    continue;
                }
                let e = &mut w.people.emo[j];
                e[emo::MORALE] = e[emo::MORALE].saturating_add(12);
                e[emo::ANGER] = e[emo::ANGER].saturating_add(6);
            }
        }
    }
}

/// An ambush waits until the crowd fights at the barricade (or streams past
/// its corner, angry, once arrests are allowed), then charges.
fn spring_ambushes(w: &mut World) {
    let tick = w.tick;
    let rung = w.police.escalation;
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        if sq.doctrine != Doctrine::Ambush || sq.present == 0 {
            continue;
        }
        let ci = sq.cordon as usize;
        let Some(&c) = w.police.cmd.cordons.get(ci) else {
            continue;
        };
        let Some(a) = c.ambush else { continue };
        if a.sprung != 0 {
            continue;
        }
        if (sq.pos - a.pos).len() > 15.0 {
            // Could not get round to the corner in time: forget it.
            if tick.wrapping_sub(sq.since) > 1800 {
                w.police.cmd.cordons[ci].ambush = None;
                let sq = &mut w.police.squads[k];
                sq.doctrine = Doctrine::Reserve;
                sq.since = tick;
                sq.cordon = NO_CORDON;
                sq.anchor = sq.pos;
            }
            continue;
        }
        // Civilians, not the squad's own bodies.
        let front = c.pos + c.face * 5.0;
        let at_line = w.fields.sample(Layer::Weight, front.x, front.y);
        let at_corner = w.fields.sample(Layer::Weight, a.junction.x, a.junction.y);
        let worn = (0..c.slots as usize).any(|s| c.standing(&w.props, s) && c.wear[s] < 170.0);
        let fighting = c.violence >= w.tun.ambush_violence
            || worn
            || c.state == CordonState::Breached
            || rung >= Escalation::GasAndWater;
        let angry = w.police.cmd.track(c.track).is_some_and(|t| t.anger > 0.5);
        let engaged = at_line >= 1.0 && fighting;
        let streaming = at_corner >= 1.5 && angry;
        // The crowd came round the corner onto them: no point hiding.
        let found = w.fields.sample(Layer::Weight, sq.pos.x, sq.pos.y) >= 1.0;
        if !(engaged || streaming || found) || rung < Escalation::BatonsAndArrests {
            continue;
        }
        let cm = &mut w.police.cmd.cordons[ci];
        if let Some(am) = cm.ambush.as_mut() {
            am.sprung = tick;
            am.at_spring = at_line.max(at_corner);
        }
        let sq = &mut w.police.squads[k];
        sq.doctrine = Doctrine::Charge;
        sq.since = tick;
        sq.anchor = a.junction;
        // Cut across, then sweep the rear part back down the route.
        sq.sweep = c.face;
        sq.facing = c.face;
        let (officers, n) = (sq.officers, sq.n as usize);
        for &o in &officers[..n] {
            if let Some(i) = w.index_of(o) {
                let fl = &mut w.people.stat[i][stat::FLAGS];
                *fl = (*fl & !flag::ASLEEP) | flag::URGENT;
            }
        }
        // The shock of it: shouting, batons on shields.
        w.fields.stamp(Layer::Threat, a.junction, 8.0, 12.0, &w.map);
        w.police.cmd.memory.ambushes += 1;
        w.police
            .cmd
            .call(tick, Call::AmbushSprung, a.junction, k as u32);
        w.outcome
            .notable(tick, NotableKind::AmbushSprung, a.junction, u32::MAX);
    }
    // Did it work? Half a minute on, is the barricade's front clearer?
    for ci in 0..w.police.cmd.cordons.len() {
        let c = w.police.cmd.cordons[ci];
        let Some(a) = c.ambush else { continue };
        if a.sprung == 0 || a.scored || tick.wrapping_sub(a.sprung) < 600 {
            continue;
        }
        let front = c.pos + c.face * 5.0;
        let now = w.fields.sample(Layer::Weight, front.x, front.y);
        let cmd = &mut w.police.cmd;
        if now < a.at_spring * 0.5 {
            cmd.memory.ambush_wins += 1;
        } else {
            cmd.memory.ambush_losses += 1;
        }
        if let Some(am) = cmd.cordons[ci].ambush.as_mut() {
            am.scored = true;
        }
    }
}

/// Kettles: when one closes, and when a closed one, calmer now, opens a
/// filter gate toward a way out.
fn kettles(w: &mut World) {
    let tick = w.tick;
    for k in 0..w.police.cmd.tracks.len() {
        let t = w.police.cmd.tracks[k];
        if t.op != OpKind::Contain {
            if t.kettled != 0 && t.op != OpKind::Contain {
                let t = &mut w.police.cmd.tracks[k];
                t.kettled = 0;
                t.gate = Vec2::ZERO;
            }
            continue;
        }
        let (mut any, mut closed) = (false, true);
        for sq in &w.police.squads {
            if sq.doctrine != Doctrine::Kettle || sq.track != t.id {
                continue;
            }
            any = true;
            let circ = 2.0 * core::f32::consts::PI * sq.radius;
            let total = sq.ring.1.max(1) as f32 * sq.n as f32;
            if circ / total > 1.6 || (sq.pos - t.centroid).len() > sq.radius + 3.0 {
                closed = false;
            }
        }
        if t.kettled == 0 {
            if any && closed {
                w.police.cmd.tracks[k].kettled = tick;
                w.police
                    .cmd
                    .call(tick, Call::KettleClosed, t.centroid, t.count as u32);
                w.outcome
                    .notable(tick, NotableKind::KettleClosed, t.centroid, u32::MAX);
            }
            continue;
        }
        // A minute held and calmer, or four minutes anyway: let them out
        // one by one through a gate on the side of the nearest way out.
        let held = tick.wrapping_sub(t.kettled);
        if t.gate == Vec2::ZERO && held >= 1200 && (t.anger < 0.45 || held >= 4800) {
            let mut exit: Option<(f32, Vec2)> = None;
            let away = away_for(w, &t);
            for (e, ent) in w.map.entries.iter().enumerate() {
                if w.entry_closed[e] {
                    continue;
                }
                let to = ent.pos - t.centroid;
                // Not toward what the police protect.
                let d = to.len() * if to.dot(away) > 0.0 { 3.0 } else { 1.0 };
                if exit.is_none_or(|b| d < b.0) {
                    exit = Some((d, to));
                }
            }
            let gate = exit.map_or(-away, |e| e.1.normalize_or_zero());
            w.police.cmd.tracks[k].gate = gate;
            for sq in w.police.squads.iter_mut() {
                if sq.doctrine == Doctrine::Kettle && sq.track == t.id {
                    sq.sweep = gate;
                }
            }
            let at = t.centroid + gate * t.radius;
            w.police.cmd.call(tick, Call::GateOpen, at, t.count as u32);
            w.outcome
                .notable(tick, NotableKind::KettleReleased, at, u32::MAX);
        }
    }
}

/// Rewrite `Layer::Block` from the standing barriers, and keep their chunks
/// active (DESIGN §8: the layer is lost when a chunk is released).
pub fn restamp(w: &mut World) {
    let f = &mut w.fields;
    let cmd = &mut w.police.cmd;
    for &(c, cell) in &cmd.stamped {
        let s = f.table[c as usize];
        if s != NO_SLOT {
            f.layer_mut(s as usize, Layer::Block)[cell as usize] = 0.0;
            f.blocked[s as usize] = false;
        }
    }
    cmd.stamped.clear();
    for ci in 0..cmd.cordons.len() {
        let c = cmd.cordons[ci];
        if !c.live() {
            continue;
        }
        for s in 0..c.slots as usize {
            if !c.standing(&w.props, s) {
                continue;
            }
            let pr = w.props[c.props[s] as usize];
            let ext = pr.half.x + 1.0;
            let (fx, fy) = f.geom.to_cell_f(pr.pos.x, pr.pos.y);
            let rc = (ext / FIELD_CELL).ceil() as i32 + 1;
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let dim = f.geom.dim as i32;
            for cy in (y0 - rc).max(0)..(y0 + rc + 1).min(dim) {
                for cx in (x0 - rc).max(0)..(x0 + rc + 1).min(dim) {
                    let (wx, wy) = f.geom.centre(cx as u32, cy as u32);
                    let d = Vec2::new(wx, wy) - pr.pos;
                    if d.dot(pr.ax).abs() > pr.half.x + 0.6 || d.dot(c.face).abs() > pr.half.y + 0.9
                    {
                        continue;
                    }
                    let chunk = f.chunk_index(cx as u32 / CC as u32, cy as u32 / CC as u32);
                    let Some(slot) = f.activate(chunk, &w.map) else {
                        continue;
                    };
                    let cell = (cy as usize % CC) * CC + cx as usize % CC;
                    f.layer_mut(slot, Layer::Block)[cell] = 1.0;
                    f.blocked[slot] = true;
                    if cmd.stamped.len() < cmd.stamped.capacity() {
                        cmd.stamped.push((chunk, cell as u16));
                    }
                }
            }
        }
    }
    f.block_live = !cmd.stamped.is_empty();
    cmd.dirty = false;
}

/// Keep chunks with police barriers active.
pub fn pin_barriers(w: &mut World) {
    let mut last = u32::MAX;
    for k in 0..w.police.cmd.stamped.len() {
        let c = w.police.cmd.stamped[k].0;
        if c == last {
            continue;
        }
        last = c;
        if let Some(s) = w.fields.activate(c, &w.map) {
            w.fields.pinned[s] = true;
            // Activation clears a released chunk: stamp it again.
            if !w.fields.blocked[s] {
                w.police.cmd.dirty = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Escalation and force

/// Climb the ladder in response to violence, crowds at the protected site,
/// officers hurt and barricades breached; step down after calm. Footage of
/// police violence makes the state more careful.
pub fn escalate(w: &mut World) {
    let tick = w.tick;
    let every = w.tun.director_interval.max(1.0) as u32;
    let p = &mut w.police;
    if let Some(f) = p.forced {
        p.escalation = f;
        return;
    }
    let need = w.tun.escalate_threat * (1.25 - 0.5 * p.aggression);
    let cmd = &mut p.cmd;
    let mut site = 0.0f32;
    for t in &cmd.tracks {
        if t.site == NO_SITE || t.cluster == NO_CLUSTER {
            continue;
        }
        let s = cmd.sites[t.site as usize];
        if (t.centroid - s.pos).len() < s.radius + t.radius + 60.0 {
            site += (t.count / 2000.0) * (0.4 + t.anger);
        }
    }
    let hurt = w.outcome.officers_injured.saturating_sub(cmd.seen_injured);
    cmd.seen_injured = w.outcome.officers_injured;
    let breaches = cmd.memory.breaches.saturating_sub(cmd.seen_breaches);
    cmd.seen_breaches = cmd.memory.breaches;
    let mut pressure =
        p.violence / need.max(1e-3) + site.min(1.5) + hurt as f32 * 0.2 + breaches as f32 * 0.5;
    let f = &w.outcome.footage;
    let optics = f.police.blame > f.movement.blame * 0.6 + 40.0;
    if optics {
        pressure *= 0.6;
    }
    let angry = p.clusters.iter().any(|c| c.anger > 0.55 && c.count > 300.0);
    let dwell = if pressure > 3.0 { 200 } else { 600 };
    let rested = tick.wrapping_sub(cmd.last_raise) >= dwell || cmd.last_raise == 0;
    if (pressure > 1.0 || angry && p.escalation < Escalation::LinesAndKettles)
        && p.escalation < p.cap
        && rested
    {
        p.escalation = Escalation::from_u8(p.escalation as u8 + 1);
        p.violence *= 0.5;
        p.calm_ticks = 0;
        cmd.last_raise = tick;
        cmd.call(tick, Call::Escalate, Vec2::ZERO, p.escalation as u32);
        if p.escalation > p.max_reached {
            p.max_reached = p.escalation;
            w.outcome.notable(
                tick,
                NotableKind::EscalationRaised,
                Vec2::ZERO,
                p.escalation as u32,
            );
        }
    } else if p.violence < need * 0.1 && site < 0.3 {
        p.calm_ticks += every;
        if p.calm_ticks as f32 > w.tun.deescalate_ticks && p.escalation > Escalation::Presence {
            p.escalation = Escalation::from_u8(p.escalation as u8 - 1);
            p.calm_ticks = 0;
            cmd.call(tick, Call::Deescalate, Vec2::ZERO, p.escalation as u32);
        }
    } else {
        p.calm_ticks = 0;
    }
    p.violence *= 0.9;
}

/// Gas and water cannon at the top rungs; metro closures.
pub fn use_force(w: &mut World) {
    let rung = w.police.escalation;
    let tick = w.tick;
    // Close metro stations near big crowds.
    if rung >= Escalation::GasAndWater
        && !w.police.closed_metro
        && w.police.clusters.iter().any(|c| c.count > 800.0)
    {
        w.police.closed_metro = true;
        for k in 0..w.map.entries.len() {
            if w.map.entries[k].kind == EntryKind::Metro {
                w.entry_closed[k] = true;
            }
        }
        w.retarget_exits();
        w.outcome
            .notable(tick, NotableKind::MetroClosed, Vec2::ZERO, u32::MAX);
    }
    if rung < Escalation::GasAndWater {
        return;
    }
    let mut used = 0;
    for k in 0..w.police.cmd.tracks.len() {
        if used == 4 {
            break;
        }
        let t = w.police.cmd.tracks[k];
        if t.cluster == NO_CLUSTER || !t.violent() {
            continue;
        }
        // Never into a kettle: there is nowhere to run.
        if t.op == OpKind::Contain {
            continue;
        }
        used += 1;
        if t.warned == 0
            && let Some(at) = w
                .police
                .cmd
                .warned_near(tick, t.centroid, t.radius + 60.0, 2400)
        {
            // Told already, under another name: the crowd split or merged.
            w.police.cmd.tracks[k].warned = at;
        }
        let t = w.police.cmd.tracks[k];
        if t.warned == 0 {
            warn(w, k);
            if t.heat < 8.0 {
                continue;
            }
        } else if tick.wrapping_sub(t.warned) < WARN_TICKS && t.heat < 8.0 {
            continue;
        }
        let gassed = w.police.cmd.volleys.iter().any(|&(q, at)| {
            at != 0 && tick.wrapping_sub(at) < 400 && (q - t.hot).len2() < 30.0 * 30.0
        });
        if tick >= t.gas_ready && tick >= w.police.cmd.gas_next && !gassed {
            gas_volley(w, k);
        }
        water(w, &t);
    }
}

/// A loudspeaker order to disperse: the timid start to leave.
fn warn(w: &mut World, k: usize) {
    let tick = w.tick;
    let t = w.police.cmd.tracks[k];
    w.police.cmd.tracks[k].warned = tick;
    let cmd = &mut w.police.cmd;
    cmd.warnings[cmd.memory.warnings as usize % 8] = (t.centroid, tick);
    w.police.cmd.memory.warnings += 1;
    w.police
        .cmd
        .call(tick, Call::Warning, t.centroid, t.count as u32);
    let px = riot_core::lanes::flat(&w.people.pos_x);
    let py = riot_core::lanes::flat(&w.people.pos_y);
    let mut heard = [0u32; 512];
    let mut n = 0;
    w.spatial
        .query_radius(px, py, t.centroid.x, t.centroid.y, t.radius + 20.0, |j| {
            if n < heard.len() {
                heard[n] = j as u32;
                n += 1;
            }
        });
    let people = &mut w.people;
    for &j in &heard[..n] {
        let j = j as usize;
        let st = &mut people.stat[j];
        if Archetype::from_u8(st[stat::ARCH]).is_officer() {
            continue;
        }
        st[stat::FLAGS] = (st[stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
        let e = &mut people.emo[j];
        e[emo::FEAR] = e[emo::FEAR].saturating_add(14);
    }
}

/// Up to three canisters at a crowd's violent core, upwind of it so the
/// wind carries the gas on, never on top of officers, each from a riot
/// officer who can see where it lands.
fn gas_volley(w: &mut World, k: usize) {
    let tick = w.tick;
    let t = w.police.cmd.tracks[k];
    let mut target = t.hot;
    let wind = w.hazards.wind;
    let ws = wind.len();
    if ws > 0.2 {
        target -= wind * (ws * 4.0).min(8.0) * (1.0 / ws);
    }
    // Not on the police's own heads.
    let mut nearest: Option<(f32, Vec2)> = None;
    for &o in &w.police.on_map {
        if let Some(i) = w.index_of(o) {
            let q = w.people.pos(i);
            let d = (q - target).len();
            if d < 8.0 && nearest.is_none_or(|b| d < b.0) {
                nearest = Some((d, q));
            }
        }
    }
    if let Some((d, q)) = nearest {
        target += (target - q).normalize_or_zero() * (8.0 - d);
    }
    // Not into a packed mass, where a stampede would crush people: the
    // thinner edge facing the police instead.
    let mut police_side: Option<(f32, Vec2)> = None;
    for &o in &w.police.on_map {
        if let Some(i) = w.index_of(o) {
            let q = w.people.pos(i);
            let d = (q - target).len2();
            if police_side.is_none_or(|b| d < b.0) {
                police_side = Some((d, q));
            }
        }
    }
    if let Some((_, q)) = police_side {
        let step = (q - target).normalize_or_zero() * 3.0;
        for _ in 0..6 {
            // People per 4 m² cell: 10 is 2.5 per m².
            if w.fields.sample(Layer::Dens, target.x, target.y) < 10.0 {
                break;
            }
            target += step;
        }
    }
    let shots = (1 + usize::from(t.heat > 8.0) + usize::from(t.heat > 16.0)).min(3);
    let mut throwers = [u32::MAX; 3];
    let mut nthrow = 0;
    let spread = [0.0f32, 6.0, -6.0];
    let mut fired = 0u32;
    for (shot, &off) in spread.iter().enumerate().take(shots) {
        let mut best: Option<(f32, u32, Vec2)> = None;
        for &o in &w.police.on_map {
            let Some(i) = w.index_of(o) else { continue };
            if w.people.stat[i][stat::ARCH] != Archetype::RiotOfficer as u8
                || throwers[..nthrow].contains(&o)
                || w.people.stat[i][stat::FLAGS] & flag::DOWN != 0
            {
                continue;
            }
            let from = w.people.pos(i);
            let d = (from - target).len();
            if d > 50.0 || best.is_some_and(|b| d >= b.0) {
                continue;
            }
            let aim = target + (target - from).normalize_or_zero().perp() * off;
            if w.map.ray_blocked(from, aim).is_none() {
                best = Some((d, o, aim));
            }
        }
        let Some((_, o, aim)) = best else { break };
        throwers[nthrow] = o;
        nthrow += 1;
        let from = w.people.pos(w.index_of(o).unwrap());
        let jitter =
            Vec2::from_heading(roll_key(w.seed, tick, o, shot as u32, Stream::Police) as u8)
                * (t.radius * 0.15).min(3.0);
        let to = aim + jitter;
        w.hazards
            .throw_canister(from, to, o, w.tun.throw_speed * 1.3);
        w.outcome.police_act(tick, from, to, &w.fields, 1.0, o);
        if !w.outcome.first_gas {
            w.outcome.first_gas = true;
            w.outcome.notable(tick, NotableKind::FirstGas, to, o);
        }
        fired += 1;
    }
    if fired > 0 {
        let cmd = &mut w.police.cmd;
        cmd.tracks[k].gas_ready = tick + 250 + 150 * fired;
        cmd.gas_next = tick + 100;
        cmd.volleys[cmd.memory.volleys as usize % 8] = (target, tick);
        cmd.memory.volleys += 1;
        cmd.call(tick, Call::Gas, target, fired);
    }
}

/// Water cannon trucks drive to the crowd's edge and spray its violent
/// core; they back off from fire.
fn water(w: &mut World, t: &Track) {
    let tick = w.tick;
    for v in 0..w.police.vehicles.len() {
        let veh = w.police.vehicles[v];
        if veh.kind != VehicleKind::WaterCannon {
            continue;
        }
        let fire = w.fields.sample(Layer::Fire, veh.pos.x, veh.pos.y)
            + w.fields.sample(
                Layer::Fire,
                veh.pos.x + veh.ax.x * 6.0,
                veh.pos.y + veh.ax.y * 6.0,
            );
        if fire > 0.1 && veh.state != VehicleState::Driving {
            let home = w.map.depots.get(veh.depot as usize).map(|d| d.pos);
            if let Some(h) = home {
                plan_route(w, v, h);
            }
            continue;
        }
        if veh.state != VehicleState::Parked && veh.state != VehicleState::Spraying {
            continue;
        }
        if (veh.pos - t.centroid).len() > t.radius + w.tun.water_range {
            if veh.state == VehicleState::Parked && veh.squad == NO_SQUAD {
                let dest =
                    t.centroid + (veh.pos - t.centroid).normalize_or_zero() * (t.radius + 12.0);
                plan_route(w, v, dest);
            }
        } else {
            let vm = &mut w.police.vehicles[v];
            let was = vm.state;
            vm.state = VehicleState::Spraying;
            vm.spray = t.hot;
            if was != VehicleState::Spraying {
                w.police
                    .cmd
                    .call(tick, Call::WaterCannon, veh.pos, v as u32);
            }
            if !w.outcome.first_water {
                w.outcome.first_water = true;
                w.outcome
                    .notable(tick, NotableKind::FirstWaterCannon, veh.pos, u32::MAX);
            }
        }
    }
}

/// Heading offset (in 1.4° steps) of a water cannon's sweep at `tick`: a
/// triangle wave over four seconds.
pub fn spray_sweep(tick: u32) -> i32 {
    let ph = (tick % 80) as i32;
    let tri = if ph < 40 { ph } else { 80 - ph };
    (tri - 20) / 2
}

/// Officers going for someone: the person, not their formation slot.
pub fn grab_target(w: &World, i: usize) -> Option<Vec2> {
    if Action::from_u8(w.people.stat[i][stat::ACTION]) != Action::Arrest {
        return None;
    }
    let t = aux::target(w.people.aux[i]);
    w.index_of(t).map(|j| w.people.pos(j))
}

/// How many of a squad's officers on the map have fled their posts, of how
/// many.
pub fn fled(w: &World, officers: &[u32]) -> (u32, u32) {
    let (mut n, mut broke) = (0, 0);
    for &o in officers {
        if let Some(i) = w.index_of(o) {
            n += 1;
            if Action::from_u8(w.people.stat[i][stat::ACTION]) == Action::Break {
                broke += 1;
            }
        }
    }
    (broke, n)
}

/// Slot heading in a kettle ring, kept out of an open gate's arc: officers
/// there stand at the gate's posts instead.
pub fn kettle_heading(h: u8, gate: Vec2) -> u8 {
    if gate == Vec2::ZERO {
        return h;
    }
    const GATE: i32 = 10;
    let g = gate.heading();
    let d = heading_delta(g, h);
    if d.abs() >= GATE {
        h
    } else if d >= 0 {
        g.wrapping_add(GATE as u8)
    } else {
        g.wrapping_sub(GATE as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_stays_small() {
        for t in 0..400 {
            assert!(spray_sweep(t).abs() <= 10);
        }
    }

    #[test]
    fn gate_moves_slots_to_its_posts() {
        let gate = Vec2::new(1.0, 0.0);
        assert_eq!(kettle_heading(0, gate), 10);
        assert_eq!(kettle_heading(250, gate), 246);
        assert_eq!(kettle_heading(100, gate), 100);
        assert_eq!(kettle_heading(5, Vec2::ZERO), 5);
    }

    #[test]
    fn walking_a_polyline() {
        let r = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
        ];
        let (p, d) = along(&r, 15.0).unwrap();
        assert!((p - Vec2::new(10.0, 5.0)).len() < 1e-4);
        assert!((d - Vec2::new(0.0, 1.0)).len() < 1e-4);
        assert!(along(&r, 25.0).is_none());
        assert!((route_len(&r) - 20.0).abs() < 1e-4);
    }
}
