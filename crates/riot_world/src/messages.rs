//! The message table (DESIGN §13): what organizers said, what was announced
//! before the riot, who is chanting, and what someone makes of a message
//! they heard. How messages spread is in `riot_messages`.

use riot_core::StateHasher;
use riot_core::rng::{Stream, roll, unit};

use riot_defs::archetype::{ARCHETYPES, Archetype, ArchetypeParams, Stance};
use riot_defs::state::{belief, intent};
use riot_defs::targets::Objective;

use crate::world::MAX_OBJECTIVES;

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
    /// Plans announced before the riot, with their objectives' weights:
    /// arriving protesters know one of them.
    pub announced: [(u8, f32); MAX_OBJECTIVES],
    pub n_announced: u8,
    /// Organizers chanting, by stable id.
    pub chanting: Vec<u32>,
    pub count: u32,
    /// The chant layer holds something and must be cleared.
    pub chant_live: bool,
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
            announced: [(0, 0.0); MAX_OBJECTIVES],
            n_announced: 0,
            chanting: Vec::with_capacity(64),
            count: 0,
            chant_live: false,
        }
    }

    pub fn create(&mut self, organizer: u32, objective: u8, stance: Stance, tick: u32) -> u8 {
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

    /// A plan announced before the riot; `weight` is how much the crowd
    /// wants it.
    pub fn announce(&mut self, objective: u8, stance: Stance, weight: f32, tick: u32) {
        let n = self.n_announced as usize;
        if n < MAX_OBJECTIVES {
            let id = self.create(u32::MAX, objective, stance, tick);
            self.announced[n] = (id, weight);
            self.n_announced += 1;
        }
    }

    /// The announced plan `stable` came for: drawn by objective weight times
    /// how well its stance suits their archetype, so radicals lean to the
    /// confrontational plan and workers to the peaceful one.
    fn pick_announced(&self, own: Stance, r: u64) -> u8 {
        let n = self.n_announced as usize;
        let level = |s: Stance| s as u8 as f32 * 0.5;
        let fit = |id: u8| match self.table[id as usize] {
            Some(m) => 0.5 + 1.0 - (level(m.stance) - level(own)).abs(),
            None => 0.0,
        };
        let total: f32 = self.announced[..n].iter().map(|&(id, w)| w * fit(id)).sum();
        let mut x = unit(r) * total;
        for &(id, w) in &self.announced[..n] {
            x -= w * fit(id);
            if x < 0.0 {
                return id;
            }
        }
        self.announced[..n].last().map_or(0, |a| a.0)
    }

    /// What an arriving person believes: the latest broadcast, else one of
    /// the announced plans, heard at a fidelity that depends on their trust.
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
        let r = roll(seed, 0, stable, Stream::Message);
        let broadcast = self.table[self.latest as usize].is_some_and(|m| m.organizer != u32::MAX);
        let id = if broadcast || self.n_announced == 0 {
            self.latest
        } else {
            self.pick_announced(arch[a as usize].default_stance, r.rotate_left(29))
        };
        let Some(m) = self.table[id as usize] else {
            return (
                intent::pack(intent::NO_GOAL, arch[a as usize].default_stance),
                0,
            );
        };
        let fid = (150.0 + 100.0 * arch[a as usize].trust * unit(r)).min(255.0) as u8;
        let fid = if a == Archetype::Organizer { 255 } else { fid };
        let it = garble(&m, fid, a, arch, r.rotate_left(13), None);
        (it, belief::pack(id, fid))
    }

    pub fn hash_into(&self, h: &mut StateHasher) {
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
#[inline]
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
