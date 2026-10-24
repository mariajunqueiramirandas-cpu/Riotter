//! Messages: rumour propagation (DESIGN §13).
//!
//! - A message table of at most 255 live messages.
//! - Per agent, `belief` holds the message id and the fidelity they heard it
//!   at; `intent` holds what they made of it (objective + stance).
//! - Per field cell, a message layer `fidelity << 8 | id`, so a plain `max`
//!   keeps the clearest message.
//!
//! Carriers scatter what they heard (less a hop's loss), organizers speak at
//! full fidelity around them, word of mouth dilates the layer one cell every
//! few ticks, and phones deliver the latest broadcast until the internet is
//! shut down. Following a message is still a choice, made in think.

use rayon::prelude::*;
use riot_core::grid::demorton2;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::rng::{Stream, roll, roll_key, unit};
use riot_core::{StateHasher, Vec2};

use crate::archetype::{ARCHETYPES, Archetype, ArchetypeParams, Stance};
use crate::fields::{CC, CL, Fields, Layer, NO_CHUNK, NO_SLOT};
use crate::people::People;
use crate::setup::{CommsMode, NotableKind, minutes};
use crate::spatial::Spatial;
use crate::state::{Action, belief, emo, flag, intent, stat};
use crate::world::{MAX_OBJECTIVES, Objective, World};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Message {
    pub id: u8,
    /// Stable id of the organizer who started it (`u32::MAX`: announced
    /// before the riot).
    pub organizer: u32,
    pub tick: u32,
    pub objective: u8,
    pub stance: Stance,
    pub fidelity: u8,
}

pub struct Messages {
    pub table: [Option<Message>; 256],
    next_id: u8,
    /// Latest broadcast, delivered by phones.
    pub latest: u8,
    /// The pre-announced plan that arriving protesters already know.
    pub announced: u8,
    /// Organizers chanting, by stable id.
    pub chanting: Vec<u32>,
    pub count: u32,
}

impl Default for Messages {
    fn default() -> Self {
        Self::new()
    }
}

impl Messages {
    pub fn new() -> Self {
        Messages {
            table: [None; 256],
            next_id: 1,
            latest: 0,
            announced: 0,
            chanting: Vec::with_capacity(64),
            count: 0,
        }
    }

    fn create(&mut self, organizer: u32, objective: u8, stance: Stance, tick: u32) -> u8 {
        let id = self.next_id;
        self.next_id = if self.next_id == 255 {
            1
        } else {
            self.next_id + 1
        };
        self.table[id as usize] = Some(Message {
            id,
            organizer,
            tick,
            objective,
            stance,
            fidelity: 255,
        });
        self.latest = id;
        self.count += 1;
        id
    }

    /// A plan announced before the riot.
    pub fn announce(&mut self, objective: u8, stance: Stance, tick: u32) {
        if self.announced == 0 {
            self.announced = self.create(u32::MAX, objective, stance, tick);
        }
    }

    /// What an arriving person believes: the announced plan, heard at a
    /// fidelity that depends on their trust.
    pub fn arrival_belief(
        &self,
        stable: u32,
        a: Archetype,
        seed: u64,
        arch: &[ArchetypeParams; ARCHETYPES],
    ) -> (u16, u16) {
        if a.is_officer() || a == Archetype::Journalist || a == Archetype::Bystander {
            return (intent::pack(intent::NO_GOAL, Stance::Peaceful), 0);
        }
        let id = if self.latest != 0 {
            self.latest
        } else {
            self.announced
        };
        let Some(m) = self.table[id as usize] else {
            return (
                intent::pack(intent::NO_GOAL, arch[a as usize].default_stance),
                0,
            );
        };
        let r = roll(seed, 0, stable, Stream::Message);
        let fid = (150.0 + 100.0 * arch[a as usize].trust * unit(r)).min(255.0) as u8;
        let fid = if a == Archetype::Organizer { 255 } else { fid };
        let it = garble(&m, fid, a, arch, r.rotate_left(13), None);
        (it, belief::pack(id, fid))
    }

    pub fn hash(&self, h: &mut StateHasher) {
        for m in self.table.iter().flatten() {
            h.write_u64(
                m.id as u64
                    | (m.objective as u64) << 8
                    | (m.stance as u64) << 16
                    | (m.tick as u64) << 32,
            );
        }
        h.write_u32(self.latest as u32);
        for &c in &self.chanting {
            h.write_u32(c);
        }
    }
}

/// What someone makes of a message heard at fidelity `fid` (DESIGN §13
/// garbling): at low fidelity the objective can be swapped for a nearby one
/// and the stance drifts toward the archetype's default.
pub fn garble(
    m: &Message,
    fid: u8,
    a: Archetype,
    arch: &[ArchetypeParams; ARCHETYPES],
    r: u64,
    objectives: Option<&[Objective]>,
) -> u16 {
    let threshold = 140.0;
    let mut obj = m.objective;
    let mut stance = m.stance;
    if (fid as f32) < threshold {
        let p = (threshold - fid as f32) / threshold;
        if unit(r) < p {
            stance = arch[a as usize].default_stance;
        }
        if let Some(objs) = objectives
            && unit(r.rotate_left(21)) < p * 0.5
            && let Some(o) = objs.get(m.objective as usize)
        {
            // Swap for the nearest other active objective.
            let mut best: Option<(f32, u8)> = None;
            for (k, q) in objs.iter().enumerate() {
                if k as u8 == m.objective || !q.active {
                    continue;
                }
                let d = (q.pos - o.pos).len2();
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, k as u8));
                }
            }
            if let Some((_, k)) = best {
                obj = k;
            }
        }
    }
    intent::pack(obj, stance)
}

/// The player's broadcast: an organizer starts a message.
pub fn broadcast(w: &mut World, organizer: u32, objective: u8) {
    let Some(i) = w.index_of(organizer) else {
        return;
    };
    if w.people.stat[i][stat::ARCH] != Archetype::Organizer as u8
        || objective as usize >= MAX_OBJECTIVES
    {
        return;
    }
    if !w.objectives[objective as usize].active {
        return;
    }
    let id = w.msgs.create(organizer, objective, w.stance, w.tick);
    w.people.belief[i] = belief::pack(id, 255);
    w.people.intent[i] = intent::pack(objective, w.stance);
    w.people.stat[i][stat::FLAGS] |= flag::URGENT;
    w.people.stat[i][stat::FLAGS] &= !flag::ASLEEP;
}

pub fn set_chant(w: &mut World, organizer: u32, on: bool) {
    let Some(i) = w.index_of(organizer) else {
        return;
    };
    if w.people.stat[i][stat::ARCH] != Archetype::Organizer as u8 {
        return;
    }
    let list = &mut w.msgs.chanting;
    let pos = list.iter().position(|&s| s == organizer);
    match (on, pos) {
        (true, None) if list.len() < list.capacity() => list.push(organizer),
        (false, Some(k)) => {
            list.remove(k);
        }
        _ => {}
    }
    w.people.stat[i][stat::FLAGS] |= flag::URGENT;
    w.people.stat[i][stat::FLAGS] &= !flag::ASLEEP;
}

/// Chants raise morale in a radius around chanting organizers, at the cost
/// of their own stamina (DESIGN §2.4).
pub fn stamp_chants(w: &mut World) {
    for slot in 0..w.fields.pool {
        if w.fields.slot_chunk[slot] != NO_CHUNK {
            w.fields.layer_mut(slot, Layer::Chant).fill(0.0);
        }
    }
    let r = w.tun.chant_radius;
    for k in 0..w.msgs.chanting.len() {
        let s = w.msgs.chanting[k];
        let Some(i) = w.index_of(s) else { continue };
        if Action::from_u8(w.people.stat[i][stat::ACTION]) != Action::Chant {
            continue;
        }
        let p = w.people.pos(i);
        let fat = riot_core::widen_u8(w.people.emo[i][emo::FATIGUE]);
        let strength = 1.0 - fat * 0.7;
        w.fields.stamp(Layer::Chant, p, r, strength, &w.map);
        w.people.emo[i][emo::FATIGUE] = riot_core::narrow_u8(fat + w.tun.chant_cost);
    }
}

/// Phase 5.
pub fn spread(w: &mut World) {
    let tick = w.tick;
    // Internet shutdown at the scheduled minute.
    if w.internet
        && let Some(m) = w.setup.comms.shutdown_min
        && tick >= minutes(m)
    {
        w.internet = false;
        w.outcome
            .notable(tick, NotableKind::InternetShutdown, Vec2::ZERO, u32::MAX);
    }
    let mode = w.setup.comms.mode;
    let dilate_every: u32 = match mode {
        CommsMode::Phones => 2,
        CommsMode::Degraded | CommsMode::Shutdown => 3,
    };
    let hop = w.tun.hop_loss.clamp(0.0, 255.0) as u16;

    // 1. Carriers scatter (fidelity - hop loss, id): a segmented max.
    scatter(&mut w.fields, &w.spatial, &w.people, hop);
    // 2. Organizers speak at full fidelity around themselves.
    let voice = w.tun.organizer_voice;
    for k in 0..w.police.organizers.len() {
        let s = w.police.organizers[k];
        let Some(i) = w.index_of(s) else { continue };
        let b = w.people.belief[i];
        if belief::id(b) == 0 {
            continue;
        }
        let v = belief::pack(belief::id(b), 255);
        stamp_max(&mut w.fields, w.people.pos(i), voice, v);
    }
    // 3. Word of mouth: dilate by one cell.
    if tick % dilate_every == 0 {
        dilate(&mut w.fields, hop);
    }
    // 4 + 5. Adoption and phones.
    let phone_rate = if !w.internet || mode == CommsMode::Shutdown {
        0.0
    } else if mode == CommsMode::Degraded {
        w.tun.phone_rate * 0.3
    } else {
        w.tun.phone_rate
    };
    let phone_fid = (255.0
        * w.tun.phone_fidelity
        * if mode == CommsMode::Degraded {
            0.7
        } else {
            1.0
        })
    .clamp(0.0, 255.0) as u8;
    let ctx = AdoptCtx {
        fields: &w.fields,
        msgs: &w.msgs,
        arch: &w.arch,
        objectives: &w.objectives,
        seed: w.seed,
        tick,
        hear_rate: w.tun.hear_rate,
        phone_rate,
        phone_fid,
    };
    adopt(&mut w.people, &ctx);
}

fn scatter(f: &mut Fields, sp: &Spatial, p: &People, hop: u16) {
    let slot_runs = &f.slot_runs;
    let slot_chunk = &f.slot_chunk;
    f.msg.par_chunks_mut(CL).enumerate().for_each(|(slot, m)| {
        if slot_chunk[slot] == NO_CHUNK {
            return;
        }
        m.fill(0);
        let [r0, r1] = slot_runs[slot];
        for run in &sp.runs[r0 as usize..r1 as usize] {
            let (lx, ly) = demorton2(run.key & 0xfff);
            let mut best = 0u16;
            for &i in &sp.fsort.order[run.start as usize..run.end as usize] {
                let b = p.belief[i as usize];
                if belief::id(b) == 0 {
                    continue;
                }
                let fid = (belief::fidelity(b) as u16).saturating_sub(hop);
                if fid == 0 {
                    continue;
                }
                best = best.max(belief::pack(belief::id(b), fid as u8));
            }
            m[ly as usize * CC + lx as usize] = best;
        }
    });
}

fn stamp_max(f: &mut Fields, p: Vec2, r: f32, v: u16) {
    let (fx, fy) = f.geom.to_cell_f(p.x, p.y);
    let rc = r / crate::spatial::FIELD_CELL;
    let dim = f.geom.dim as i32;
    for cy in ((fy - rc).floor() as i32).max(0)..((fy + rc).ceil() as i32).min(dim) {
        for cx in ((fx - rc).floor() as i32).max(0)..((fx + rc).ceil() as i32).min(dim) {
            let (dx, dy) = (cx as f32 + 0.5 - fx, cy as f32 + 0.5 - fy);
            if dx * dx + dy * dy > rc * rc {
                continue;
            }
            if let Some(s) = f.slot_of_chunk(cx / CC as i32, cy / CC as i32) {
                let cell = s * CL + (cy as usize % CC) * CC + cx as usize % CC;
                f.msg[cell] = f.msg[cell].max(v);
            }
        }
    }
}

/// 3 × 3 max filter with a hop's fidelity loss, across chunk borders.
fn dilate(f: &mut Fields, hop: u16) {
    let lose = |v: u16| -> u16 {
        let fid = (v >> 8).saturating_sub(hop);
        if fid == 0 { 0 } else { (fid << 8) | (v & 0xff) }
    };
    let cdim = f.cdim;
    // Rows: msg → msg_tmp.
    {
        let (msg, table, slot_chunk) = (&f.msg, &f.table, &f.slot_chunk);
        f.msg_tmp
            .par_chunks_mut(CL)
            .enumerate()
            .for_each(|(slot, out)| {
                let c = slot_chunk[slot];
                if c == NO_CHUNK {
                    return;
                }
                let (cx, cy) = (c % cdim, c / cdim);
                let nb = |dx: i32| {
                    let x = cx as i32 + dx;
                    if x < 0 || x >= cdim as i32 {
                        return None;
                    }
                    let s = table[(cy * cdim + x as u32) as usize];
                    (s != NO_SLOT).then_some(s as usize)
                };
                let (l, r) = (nb(-1), nb(1));
                let me = &msg[slot * CL..(slot + 1) * CL];
                for y in 0..CC {
                    for x in 0..CC {
                        let left = if x > 0 {
                            me[y * CC + x - 1]
                        } else {
                            l.map_or(0, |s| msg[s * CL + y * CC + CC - 1])
                        };
                        let right = if x + 1 < CC {
                            me[y * CC + x + 1]
                        } else {
                            r.map_or(0, |s| msg[s * CL + y * CC])
                        };
                        out[y * CC + x] = me[y * CC + x].max(lose(left)).max(lose(right));
                    }
                }
            });
    }
    // Columns: msg_tmp → msg.
    let (tmp, table, slot_chunk) = (&f.msg_tmp, &f.table, &f.slot_chunk);
    f.msg
        .par_chunks_mut(CL)
        .enumerate()
        .for_each(|(slot, out)| {
            let c = slot_chunk[slot];
            if c == NO_CHUNK {
                return;
            }
            let (cx, cy) = (c % cdim, c / cdim);
            let nb = |dy: i32| {
                let y = cy as i32 + dy;
                if y < 0 || y >= cdim as i32 {
                    return None;
                }
                let s = table[(y as u32 * cdim + cx) as usize];
                (s != NO_SLOT).then_some(s as usize)
            };
            let (u, d) = (nb(-1), nb(1));
            let me = &tmp[slot * CL..(slot + 1) * CL];
            for y in 0..CC {
                for x in 0..CC {
                    let up = if y > 0 {
                        me[(y - 1) * CC + x]
                    } else {
                        u.map_or(0, |s| tmp[s * CL + (CC - 1) * CC + x])
                    };
                    let down = if y + 1 < CC {
                        me[(y + 1) * CC + x]
                    } else {
                        d.map_or(0, |s| tmp[s * CL + x])
                    };
                    out[y * CC + x] = me[y * CC + x].max(lose(up)).max(lose(down));
                }
            }
        });
}

struct AdoptCtx<'a> {
    fields: &'a Fields,
    msgs: &'a Messages,
    arch: &'a [ArchetypeParams; ARCHETYPES],
    objectives: &'a [Objective],
    seed: u64,
    tick: u32,
    hear_rate: f32,
    phone_rate: f32,
    phone_fid: u8,
}

fn adopt(p: &mut People, cx: &AdoptCtx) {
    let len = p.len;
    let chunks = p.chunks();
    let span = chunks * CHUNK_AGENTS;
    let People {
        pos_x,
        pos_y,
        belief: beliefs,
        intent: intents,
        stat,
        stable_id,
        ..
    } = p;
    let (px, py) = (lanes::flat(pos_x), lanes::flat(pos_y));
    let stat = &mut stat[..span];
    beliefs[..span]
        .par_chunks_mut(CHUNK_AGENTS)
        .zip(intents[..span].par_chunks_mut(CHUNK_AGENTS))
        .zip(stat.par_chunks_mut(CHUNK_AGENTS))
        .enumerate()
        .for_each(|(c, ((bs, is), ss))| {
            let base = c * CHUNK_AGENTS;
            for k in 0..bs.len() {
                let i = base + k;
                if i >= len {
                    break;
                }
                let st = ss[k];
                let fl = st[stat::FLAGS];
                if fl & flag::DEAD != 0 {
                    continue;
                }
                let a = Archetype::from_u8(st[stat::ARCH]);
                if a.is_officer() || a == Archetype::Organizer && belief::id(bs[k]) != 0 {
                    continue;
                }
                let own = bs[k];
                let stable = stable_id[i];
                let r = roll(cx.seed, cx.tick, stable, Stream::Message);
                // Word of mouth: read one cell.
                let (gx, gy) = cx.fields.geom.cell_of(px[i], py[i]);
                let heard = match cx
                    .fields
                    .slot_of_chunk((gx / CC as u32) as i32, (gy / CC as u32) as i32)
                {
                    Some(s) => cx.fields.msg[s * CL + (gy as usize % CC) * CC + gx as usize % CC],
                    None => 0,
                };
                let mut take: Option<u16> = None;
                if belief::id(heard) != 0 && belief::id(heard) != belief::id(own) {
                    let newer = is_newer(cx.msgs, belief::id(heard), belief::id(own));
                    let clearer =
                        belief::fidelity(heard) > belief::fidelity(own).saturating_add(24);
                    if belief::id(own) == 0 || newer || clearer {
                        let trust = cx.arch[a as usize].trust;
                        if unit(r) < cx.hear_rate * (0.25 + trust) {
                            take = Some(heard);
                        }
                    }
                } else if belief::id(heard) == belief::id(own)
                    && belief::fidelity(heard) > belief::fidelity(own).saturating_add(24)
                {
                    // Hearing it again, more clearly.
                    if unit(r) < cx.hear_rate {
                        take = Some(heard);
                    }
                }
                // Phones: group chats bring the latest broadcast.
                if take.is_none() && fl & flag::PHONE != 0 && cx.phone_rate > 0.0 {
                    let latest = cx.msgs.latest;
                    if latest != 0
                        && latest != belief::id(own)
                        && unit(r.rotate_left(29)) < cx.phone_rate
                    {
                        take = Some(belief::pack(latest, cx.phone_fid));
                    }
                }
                if let Some(v) = take
                    && let Some(m) = cx.msgs.table[belief::id(v) as usize]
                {
                    bs[k] = v;
                    is[k] = garble(
                        &m,
                        belief::fidelity(v),
                        a,
                        cx.arch,
                        roll_key(cx.seed, cx.tick, stable, 7, Stream::Garble),
                        Some(cx.objectives),
                    );
                    if intent::objective(is[k]) != intent::objective(own_intent_of(own, cx.msgs)) {
                        ss[k][stat::FLAGS] |= flag::URGENT;
                    }
                    if fl & flag::ASLEEP != 0 {
                        ss[k][stat::FLAGS] = (ss[k][stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
                    }
                }
            }
        });
}

#[inline]
fn own_intent_of(b: u16, msgs: &Messages) -> u16 {
    match msgs.table[belief::id(b) as usize] {
        Some(m) => intent::pack(m.objective, m.stance),
        None => intent::pack(intent::NO_GOAL, Stance::Peaceful),
    }
}

/// Is message `a` newer than message `b`? Ids wrap, so compare ticks.
#[inline]
fn is_newer(msgs: &Messages, a: u8, b: u8) -> bool {
    match (msgs.table[a as usize], msgs.table[b as usize]) {
        (Some(x), Some(y)) => x.tick > y.tick || (x.tick == y.tick && a > b),
        (Some(_), None) => true,
        _ => false,
    }
}
