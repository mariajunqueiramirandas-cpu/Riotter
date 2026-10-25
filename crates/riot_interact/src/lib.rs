//! Phase 8: interactions → events → resolve (DESIGN §11.3).
//!
//! A parallel pass over agent chunks emits small events into per-chunk
//! buffers (which already hold the body phase's wake events). The buffers
//! are then resolved sequentially in chunk order: conflicts, such as two
//! officers grabbing the same person, are settled by that order.

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{narrow_u8, widen_u8};
use riot_core::rng::{Stream, roll, roll_key, unit};
use riot_team::prelude::*;

use riot_defs::archetype::Archetype;
use riot_defs::event::Event;
use riot_defs::harm::Harm;
use riot_defs::mapdef::{EntryKind, PropKind};
use riot_defs::setup::{NotableKind, SupplyKind};
use riot_defs::state::{Action, Gait, Item, aux, emo, flag, health, intent, pose, stat};
use riot_fields::{Layer, field_cell_pos};
use riot_snapshot::{Appearance, prop_flag};
use riot_world::World;
use riot_world::harm::{damage, die};
use riot_world::hazards::ProjectileKind;
use riot_world::police::Arrest;
use riot_world::world::Prop;

/// Bit set in a helper's `aux` target while carrying someone.
pub const CARRYING: u32 = 1 << 23;

/// Phase 8.
pub fn interact(w: &mut World) {
    emit(w);
    resolve(w);
    riot_hazards::projectiles(w);
    carry(w);
}

fn emit(w: &mut World) {
    let len = w.people.len;
    let chunks = w.people.chunks();
    let tun = &w.tun;
    let p = &w.people;
    let px = lanes::flat(&p.pos_x);
    let py = lanes::flat(&p.pos_y);
    let (tick, seed) = (w.tick, w.seed);
    let sp = &w.spatial;
    let slot_of = &w.slot_of;
    let cold = &w.cold;
    let drops = &w.drops;
    let props = &w.props;
    let fields = &w.fields;
    let entries = &w.map.entries;
    let closed = &w.entry_closed;
    let objectives = &w.objectives;
    let crush = tun.crush_pressure;
    let fallp = tun.fall_pressure;
    w.events[..chunks]
        .par_iter_mut()
        .enumerate()
        .for_each(|(c, ev)| {
            let base = c * CHUNK_AGENTS;
            let mut push = |e: Event| {
                if ev.len() < ev.capacity() {
                    ev.push(e);
                }
            };
            for k in 0..CHUNK_AGENTS {
                let i = base + k;
                if i >= len {
                    break;
                }
                let st = p.stat[i];
                let fl = st[stat::FLAGS];
                if fl & flag::DEAD != 0 {
                    continue;
                }
                let iu = i as u32;
                if st[stat::HEALTH] == 0 {
                    push(Event::Die { who: iu });
                    continue;
                }
                let stable = p.stable_id[i];
                let gait = Gait::from_u8(p.pose[i][pose::GAIT]);
                let a = Archetype::from_u8(st[stat::ARCH]);
                if fl & flag::ASLEEP != 0 {
                    continue;
                }
                let pressure = widen_u8(p.pose[i][pose::PRESSURE]);
                if gait == Gait::Fallen {
                    if pressure < 0.2
                        && st[stat::HEALTH] >= health::INCAPACITATED
                        && unit(roll(seed, tick, stable, Stream::Body)) < tun.getup_chance
                    {
                        push(Event::GetUp { who: iu });
                    }
                    continue;
                }
                if gait.is_down() {
                    continue;
                }
                // Crush, falls and trips (DESIGN §2.6).
                if !a.is_officer() || pressure > crush * 1.3 {
                    if pressure > crush {
                        push(Event::Crush {
                            who: iu,
                            amount: narrow_u8((pressure - crush) / (1.0 - crush)),
                        });
                    }
                    let r = unit(roll_key(seed, tick, stable, 1, Stream::Body));
                    if pressure > fallp && r < tun.fall_chance * (pressure - fallp) / (1.0 - fallp)
                    {
                        push(Event::Fall { who: iu });
                    } else if pressure > 0.25 {
                        let fallen = sp
                            .neighbours(i)
                            .iter()
                            .filter(|&&j| p.pose[j as usize][pose::GAIT] == Gait::Fallen as u8)
                            .count();
                        if fallen > 0 && r < tun.trip_chance * fallen as f32 * pressure {
                            push(Event::Fall { who: iu });
                        }
                    }
                }
                let pos = Vec2::new(px[i], py[i]);
                let act = Action::from_u8(st[stat::ACTION]);
                let target = aux::target(p.aux[i]);
                let person = |s: u32| -> Option<usize> {
                    let j = *slot_of.get(s as usize)?;
                    (j != u32::MAX).then_some(j as usize)
                };
                match act {
                    Action::Throw => {
                        let ready = tick.wrapping_sub(cold[stable as usize].last_act)
                            >= tun.throw_cooldown as u32
                            || cold[stable as usize].last_act == 0;
                        if let (true, Some(j)) = (ready, person(target)) {
                            let q = Vec2::new(px[j], py[j]);
                            let d = (q - pos).len();
                            let facing = Vec2::from_heading(p.pose[i][pose::HEADING])
                                .dot((q - pos).normalize_or_zero())
                                > 0.6;
                            if d < sp.radius[i] + sp.radius[j] + 0.5 {
                                push(Event::Strike {
                                    by: iu,
                                    target: j as u32,
                                });
                            } else if d <= tun.throw_range && facing {
                                // Lead the target a little.
                                let lead = p.vel(j) * (d / tun.throw_speed);
                                let aim = q + lead;
                                push(Event::Throw {
                                    by: iu,
                                    x: aim.x,
                                    y: aim.y,
                                });
                            }
                        }
                    }
                    Action::Strike => {
                        let ready = tick.wrapping_sub(cold[stable as usize].last_act) >= 20
                            || cold[stable as usize].last_act == 0;
                        if let (true, Some(j)) = (ready, person(target)) {
                            let d = (Vec2::new(px[j], py[j]) - pos).len();
                            if d <= sp.radius[i] + sp.radius[j] + tun.strike_range {
                                push(Event::Strike {
                                    by: iu,
                                    target: j as u32,
                                });
                            }
                        }
                    }
                    Action::Arrest => {
                        if let Some(j) = person(target)
                            && (Vec2::new(px[j], py[j]) - pos).len() < 1.2
                        {
                            push(Event::Grab {
                                by: iu,
                                target: j as u32,
                            });
                        }
                    }
                    Action::Help => {
                        if target & CARRYING == 0
                            && let Some(j) = person(target)
                            && (Vec2::new(px[j], py[j]) - pos).len() < 1.1
                        {
                            push(Event::Lift {
                                by: iu,
                                target: j as u32,
                            });
                        }
                    }
                    Action::SeekSupply => {
                        if (tick + stable).is_multiple_of(10) {
                            let want = if target == 1 {
                                SupplyKind::Medic
                            } else {
                                SupplyKind::Water
                            };
                            for (d, drop) in drops.iter().enumerate() {
                                if drop.amount > 0
                                    && drop.kind == want
                                    && (drop.pos - pos).len2() < 4.0
                                {
                                    push(Event::Pickup {
                                        by: iu,
                                        drop: d as u32,
                                    });
                                    break;
                                }
                            }
                        }
                    }
                    Action::Barricade => {
                        if (tick + stable).is_multiple_of(10) && target != aux::NO_TARGET {
                            let spot = field_cell_pos(fields, target);
                            if (spot - pos).len2() < 4.0 {
                                let mut found = u32::MAX;
                                for (k2, pr) in props.iter().enumerate() {
                                    if pr.kind == PropKind::Barricade
                                        && pr.flags & prop_flag::DESTROYED == 0
                                        && pr.build < 255
                                        && (pr.pos - pos).len2() < 9.0
                                    {
                                        found = k2 as u32;
                                        break;
                                    }
                                }
                                push(Event::Build {
                                    by: iu,
                                    prop: found,
                                });
                            }
                        }
                    }
                    Action::GoHome | Action::March | Action::Flee
                        if (tick + stable).is_multiple_of(10) =>
                    {
                        for (e, ent) in entries.iter().enumerate() {
                            if closed[e] {
                                continue;
                            }
                            let r = tun.exit_radius.max(ent.radius * 0.5);
                            if (ent.pos - pos).len2() > r * r {
                                continue;
                            }
                            // Whoever has fled all the way to a way out takes it.
                            let leaving = act == Action::GoHome || act == Action::Flee;
                            let metro = act == Action::March && ent.kind == EntryKind::Metro && {
                                let slot = intent::objective(p.intent[i]);
                                objectives
                                    .get(slot as usize)
                                    .is_some_and(|o| o.active && (o.pos - pos).len() > 1200.0)
                            };
                            if leaving || metro {
                                push(Event::Leave {
                                    who: iu,
                                    entry: e as u32,
                                });
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
}

fn resolve(w: &mut World) {
    let chunks = w.events.len();
    for c in 0..chunks {
        let n = w.events[c].len();
        for k in 0..n {
            let e = w.events[c][k];
            apply(w, e);
        }
    }
}

fn apply(w: &mut World, e: Event) {
    let tick = w.tick;
    let alive = |w: &World, i: u32| w.people.stat[i as usize][stat::FLAGS] & flag::DEAD == 0;
    match e {
        Event::Wake { target } => {
            let st = &mut w.people.stat[target as usize];
            if st[stat::FLAGS] & flag::ASLEEP != 0 {
                st[stat::FLAGS] = (st[stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
                w.people.aux[target as usize] = aux::with_still(w.people.aux[target as usize], 0);
            }
        }
        Event::Strike { by, target } => {
            if !alive(w, by) || !alive(w, target) {
                return;
            }
            let (bi, ti) = (by as usize, target as usize);
            let attacker = w.people.stable_id[bi];
            let police = Archetype::from_u8(w.people.stat[bi][stat::ARCH]).is_officer();
            let victim_officer = Archetype::from_u8(w.people.stat[ti][stat::ARCH]).is_officer();
            if police == victim_officer {
                return;
            }
            w.cold[attacker as usize].last_act = tick.max(1);
            // Shields block blows from the front.
            let dir = (w.people.pos(ti) - w.people.pos(bi)).normalize_or_zero();
            let facing = Vec2::from_heading(w.people.pose[ti][pose::HEADING]);
            let blocked =
                w.people.stat[ti][stat::FLAGS] & flag::SHIELD != 0 && facing.dot(-dir) > 0.3;
            if !blocked {
                let cause = if police { Harm::Baton } else { Harm::Blow };
                damage(w, ti, narrow_u8(w.tun.strike_damage), attacker, cause);
            }
            let pos = w.people.pos(ti);
            if police {
                // Hitting someone who was not violent is what footage cares about.
                let victim_act = Action::from_u8(w.people.stat[ti][stat::ACTION]);
                let peaceful = !matches!(victim_act, Action::Throw | Action::Barricade);
                let sev = if peaceful { 2.0 } else { 0.6 };
                w.outcome
                    .police_act(tick, w.people.pos(bi), pos, &w.fields, sev, attacker);
            } else {
                movement_act(w, bi, pos, 1.5);
            }
        }
        Event::Grab { by, target } => {
            if !alive(w, by) || !alive(w, target) {
                return;
            }
            let ts = w.people.stable_id[target as usize];
            let os = w.people.stable_id[by as usize];
            let restrained =
                Gait::from_u8(w.people.pose[target as usize][pose::GAIT]) == Gait::Restrained;
            if restrained || w.police.arrests.contains(ts) || w.police.escorting.contains(os) {
                return;
            }
            w.police.arrests.insert(
                ts,
                Arrest {
                    officer: os,
                    progress: 0,
                    lost: 0,
                },
            );
            let st = &mut w.people.stat[target as usize];
            st[stat::FLAGS] = (st[stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
            w.outcome.police_act(
                tick,
                w.people.pos(by as usize),
                w.people.pos(target as usize),
                &w.fields,
                0.4,
                os,
            );
        }
        Event::Throw { by, x, y } => {
            if !alive(w, by) {
                return;
            }
            let bi = by as usize;
            let s = w.people.stable_id[bi];
            let item = Item::from_u8(w.cold[s as usize].item);
            let kind = match item {
                Item::Molotov => ProjectileKind::Molotov,
                _ => ProjectileKind::Stone,
            };
            if matches!(item, Item::Bottle | Item::Molotov) {
                set_item(w, bi, Item::None);
            }
            let from = w.people.pos(bi);
            w.hazards
                .launch(kind, from, Vec2::new(x, y), s, w.tun.throw_speed);
            w.cold[s as usize].last_act = tick.max(1);
            let sev = if kind == ProjectileKind::Molotov {
                3.0
            } else {
                1.0
            };
            movement_act(w, bi, from, sev);
        }
        Event::Lift { by, target } => {
            if !alive(w, by) || !alive(w, target) {
                return;
            }
            let (bs, ts) = (
                w.people.stable_id[by as usize],
                w.people.stable_id[target as usize],
            );
            if w.carried.contains(ts) || w.carried.entries().iter().any(|&(_, c)| c == bs) {
                return;
            }
            if !Gait::from_u8(w.people.pose[target as usize][pose::GAIT]).is_down()
                && w.people.stat[target as usize][stat::HEALTH] >= health::INCAPACITATED
            {
                return;
            }
            if w.carried.insert(ts, bs) {
                w.people.pose[target as usize][pose::GAIT] = Gait::Carried as u8;
                w.people.stat[target as usize][stat::FLAGS] |= flag::DOWN;
                w.people.aux[by as usize] =
                    aux::with_target(w.people.aux[by as usize], ts | CARRYING);
            }
        }
        Event::Pickup { by, drop } => {
            let (bi, d) = (by as usize, drop as usize);
            if !alive(w, by) || d >= w.drops.len() || w.drops[d].amount == 0 {
                return;
            }
            w.drops[d].amount -= 1;
            let kind = w.drops[d].kind;
            match kind {
                SupplyKind::Water => {
                    let f = widen_u8(w.people.emo[bi][emo::FATIGUE]) - w.tun.water_recover;
                    w.people.emo[bi][emo::FATIGUE] = narrow_u8(f);
                    w.people.emo[bi][emo::MORALE] = w.people.emo[bi][emo::MORALE].saturating_add(8);
                }
                SupplyKind::Medic => {
                    let h = &mut w.people.stat[bi][stat::HEALTH];
                    *h = h.saturating_add(90);
                }
                SupplyKind::Shields => {
                    set_item(w, bi, Item::Shield);
                    w.people.stat[bi][stat::FLAGS] |= flag::SHIELD;
                }
                SupplyKind::Barricade => set_item(w, bi, Item::Plank),
            }
            w.people.stat[bi][stat::FLAGS] |= flag::URGENT;
            if w.drops[d].amount == 0 && matches!(kind, SupplyKind::Water | SupplyKind::Medic) {
                w.retarget_supply(kind);
            }
        }
        Event::Build { by, prop } => {
            let bi = by as usize;
            if !alive(w, by) {
                return;
            }
            let plank = Item::from_u8(w.cold[w.people.stable_id[bi] as usize].item) == Item::Plank;
            let add: u8 = if plank { 64 } else { 6 };
            if plank {
                set_item(w, bi, Item::None);
            }
            if prop == u32::MAX {
                if w.props.len() == w.props.capacity() {
                    return;
                }
                let h = Vec2::from_heading(w.people.pose[bi][pose::HEADING]);
                let at = w.people.pos(bi) + h * 1.2;
                if !w.map.walkable_at(at) {
                    return;
                }
                w.props.push(Prop {
                    kind: PropKind::Barricade,
                    pos: at,
                    half: Vec2::new(0.4, 1.6),
                    ax: h,
                    health: 255,
                    flags: prop_flag::BUILDING,
                    build: add,
                });
                w.props_dirty = true;
            } else if let Some(pr) = w.props.get_mut(prop as usize) {
                pr.build = pr.build.saturating_add(add);
                if pr.build == 255 {
                    pr.flags &= !prop_flag::BUILDING;
                    // A finished barricade is wider.
                    pr.half = Vec2::new(0.5, 2.4);
                    w.props_dirty = true;
                }
            }
            let pos = w.people.pos(bi);
            movement_act(w, bi, pos, 0.3);
        }
        Event::Crush { who, amount } => {
            if !alive(w, who) {
                return;
            }
            let i = who as usize;
            // Health points per tick, rounded stochastically so slight excess
            // pressure hurts slowly rather than not at all or at once.
            let stable = w.people.stable_id[i];
            let r = unit(roll_key(w.seed, tick, stable, 2, Stream::Body));
            let hp = w.tun.crush_damage * widen_u8(amount) * 255.0;
            let dmg = (hp + r).floor().clamp(0.0, 255.0) as u8;
            if dmg > 0 {
                damage(w, i, dmg, u32::MAX, Harm::Crush);
            }
            let fear = w.tun.crush_fear * widen_u8(amount) * 255.0;
            let df = (fear + r).floor().clamp(0.0, 255.0) as u8;
            let f = &mut w.people.emo[i][emo::FEAR];
            *f = f.saturating_add(df);
            if !w.outcome.crush_noted && amount > 128 {
                w.outcome.crush_noted = true;
                let p = w.people.pos(i);
                w.outcome
                    .notable(tick, NotableKind::Crush, p, w.people.stable_id[i]);
            }
        }
        Event::Fall { who } => {
            if !alive(w, who) {
                return;
            }
            let i = who as usize;
            if !Gait::from_u8(w.people.pose[i][pose::GAIT]).is_down() {
                w.people.pose[i][pose::GAIT] = Gait::Fallen as u8;
                w.people.stat[i][stat::FLAGS] |= flag::DOWN;
                w.people.emo[i][emo::FEAR] = w.people.emo[i][emo::FEAR].saturating_add(40);
                let h = &mut w.people.stat[i][stat::HEALTH];
                let before = *h;
                *h = h.saturating_sub(12);
                w.outcome.harm[Harm::Crush as usize] += (before - *h) as u32;
                let s = w.people.stable_id[i] as usize;
                w.cold[s].last_harm = Harm::Crush as u8;
                let h = w.people.stat[i][stat::HEALTH];
                if before >= health::INJURED && h < health::INJURED {
                    w.outcome
                        .injured(Archetype::from_u8(w.people.stat[i][stat::ARCH]).is_officer());
                }
            }
        }
        Event::GetUp { who } => {
            if !alive(w, who) {
                return;
            }
            let i = who as usize;
            if Gait::from_u8(w.people.pose[i][pose::GAIT]) == Gait::Fallen {
                w.people.pose[i][pose::GAIT] = Gait::Stand as u8;
                w.people.stat[i][stat::FLAGS] =
                    (w.people.stat[i][stat::FLAGS] & !flag::DOWN) | flag::URGENT;
            }
        }
        Event::Die { who } => {
            if !alive(w, who) {
                return;
            }
            die(w, who as usize);
        }
        Event::Leave { who, entry } => {
            if !alive(w, who) {
                return;
            }
            let i = who as usize;
            let act = Action::from_u8(w.people.stat[i][stat::ACTION]);
            if act == Action::March {
                riot_hazards::try_board(w, i, entry as usize);
            } else {
                w.outcome.went_home += 1;
                w.remove(i);
            }
        }
    }
}

/// A violent act by a protester: footage blames the movement, police notice.
/// Officers within 25 m who can see it (evidence gatherers and spotters)
/// identify whoever did it, as a camera would.
fn movement_act(w: &mut World, i: usize, pos: Vec2, severity: f32) {
    let s = w.people.stable_id[i];
    let filmed = w.outcome.movement_act(w.tick, pos, &w.fields, severity, s);
    w.police.violence += severity;
    w.police.cmd.note_violence(w.tick, pos, severity);
    let spotted = !filmed
        && w.police.on_map.iter().any(|&o| {
            w.index_of(o).is_some_and(|j| {
                let q = w.people.pos(j);
                (q - pos).len2() < 625.0
                    && w.people.stat[j][stat::ACTION] != Action::Lurk as u8
                    && w.map.ray_blocked(q, pos).is_none()
            })
        });
    let seen = filmed || spotted;
    w.fields.stamp(
        Layer::Threat,
        pos,
        6.0,
        w.tun.threat_violence * severity,
        &w.map,
    );
    if seen {
        let c = &mut w.cold[s as usize];
        c.filmed = c.filmed.saturating_add(1);
        c.last_act = w.tick.max(1);
        if !w.police.watch.contains(s) {
            w.police.watch.insert(s, 1);
        }
        w.people.stat[i][stat::FLAGS] |= flag::KNOWN;
    }
}

fn set_item(w: &mut World, i: usize, item: Item) {
    let s = w.people.stable_id[i] as usize;
    w.cold[s].item = item as u8;
    w.cold[s].appearance = Appearance(w.cold[s].appearance).with_item(item as u8).0;
    if item == Item::None {
        w.people.stat[i][stat::FLAGS] &= !flag::ITEM;
    } else {
        w.people.stat[i][stat::FLAGS] |= flag::ITEM;
    }
}

/// Carried people follow their carriers; a medic post treats them.
fn carry(w: &mut World) {
    let heal = narrow_u8(w.tun.medic_heal).max(1);
    let mut k = 0;
    while k < w.carried.len() {
        let (ts, cs) = w.carried.entries()[k];
        let (Some(ti), Some(ci)) = (w.index_of(ts), w.index_of(cs)) else {
            if let Some(ti) = w.index_of(ts) {
                put_down(w, ti);
            }
            w.carried.remove(ts);
            continue;
        };
        let cp = w.people.pos(ci);
        if Action::from_u8(w.people.stat[ci][stat::ACTION]) != Action::Help
            || Gait::from_u8(w.people.pose[ci][pose::GAIT]).is_down()
        {
            put_down(w, ti);
            w.carried.remove(ts);
            continue;
        }
        let back = Vec2::from_heading(w.people.pose[ci][pose::HEADING]) * -0.55;
        let at = cp + back;
        if w.map.walkable_at(at) {
            w.people.set_pos(ti, at);
        }
        w.people.set_vel(ti, w.people.vel(ci));
        // At a medic post: treat, then let go.
        let medic = w.drops.iter().position(|d| {
            d.kind == SupplyKind::Medic && d.amount > 0 && (d.pos - cp).len2() < 16.0
        });
        if let Some(d) = medic {
            let h = &mut w.people.stat[ti][stat::HEALTH];
            *h = h.saturating_add(heal);
            if *h >= health::INJURED {
                w.drops[d].amount -= 1;
                if w.drops[d].amount == 0 {
                    w.retarget_supply(SupplyKind::Medic);
                }
                put_down(w, ti);
                w.carried.remove(ts);
                w.people.aux[ci] = aux::with_target(w.people.aux[ci], aux::NO_TARGET);
                w.people.stat[ci][stat::FLAGS] |= flag::URGENT;
                continue;
            }
        }
        k += 1;
    }
}

fn put_down(w: &mut World, i: usize) {
    let healthy = w.people.stat[i][stat::HEALTH] >= health::INCAPACITATED;
    w.people.pose[i][pose::GAIT] = if healthy {
        Gait::Stand as u8
    } else {
        Gait::Fallen as u8
    };
    if healthy {
        w.people.stat[i][stat::FLAGS] &= !flag::DOWN;
    }
    w.people.stat[i][stat::FLAGS] |= flag::URGENT;
}
