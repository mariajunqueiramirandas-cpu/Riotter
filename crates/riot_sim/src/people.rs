//! The agent table (DESIGN §5, §6). Civilians and officers share one SoA
//! table with the same body layout, so they share one body hash and one
//! contact solve (DESIGN §21 "officer index": shared, tagged by archetype).
//!
//! Hot + warm state is exactly 40 bytes per agent (DESIGN §3 hard rules):
//!
//! | field | bytes |
//! |---|---|
//! | `pos_x`, `pos_y`, `vel_x`, `vel_y` (f32) | 16 |
//! | `pose` `[heading, gait, stride, pressure]` | 4 |
//! | `emo` `[fear, anger, morale, fatigue]` | 4 |
//! | `stat` `[health, action, archetype, flags]` | 4 |
//! | `intent` u16, `belief` u16 | 4 |
//! | `stable_id` u32 | 4 |
//! | `aux` u32: 24-bit target + 8-bit stillness counter | 4 |

use riot_core::Vec2;
use riot_core::lanes::{self, CHUNK_AGENTS, F32x8};
use riot_team::prelude::*;

use crate::state::{aux, flag, stat};

pub const STATE_BYTES_PER_AGENT: usize = 40;

#[derive(Clone)]
pub struct People {
    pub pos_x: Vec<F32x8>,
    pub pos_y: Vec<F32x8>,
    pub vel_x: Vec<F32x8>,
    pub vel_y: Vec<F32x8>,
    pub pose: Vec<[u8; 4]>,
    pub emo: Vec<[u8; 4]>,
    pub stat: Vec<[u8; 4]>,
    pub intent: Vec<u16>,
    pub belief: Vec<u16>,
    pub stable_id: Vec<u32>,
    pub aux: Vec<u32>,
    /// Live agents occupy `0..len` (some may be flagged dead until the next
    /// compaction); lanes past `len` are inert padding.
    pub len: usize,
    /// Allocated agents, a multiple of `CHUNK_AGENTS`.
    pub cap: usize,
}

/// Everything needed to put a new agent in the table.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    pub pos: Vec2,
    pub heading: u8,
    pub emo: [u8; 4],
    pub health: u8,
    pub action: u8,
    pub archetype: u8,
    pub flags: u8,
    pub intent: u16,
    pub belief: u16,
    pub stable_id: u32,
}

const DEAD_STAT: [u8; 4] = [0, 0, 0, flag::DEAD];

impl People {
    pub fn new(max_agents: usize) -> Self {
        let cap = lanes::chunk_padded(max_agents.max(1));
        People {
            pos_x: lanes::zeroed(cap),
            pos_y: lanes::zeroed(cap),
            vel_x: lanes::zeroed(cap),
            vel_y: lanes::zeroed(cap),
            pose: vec![[0; 4]; cap],
            emo: vec![[0; 4]; cap],
            stat: vec![DEAD_STAT; cap],
            intent: vec![0; cap],
            belief: vec![0; cap],
            stable_id: vec![u32::MAX; cap],
            aux: vec![aux::NO_TARGET; cap],
            len: 0,
            cap,
        }
    }

    /// Number of parallel chunks covering `0..len`.
    #[inline]
    pub fn chunks(&self) -> usize {
        self.len.div_ceil(CHUNK_AGENTS)
    }

    #[inline]
    pub fn pos(&self, i: usize) -> Vec2 {
        Vec2::new(lanes::get(&self.pos_x, i), lanes::get(&self.pos_y, i))
    }
    #[inline]
    pub fn set_pos(&mut self, i: usize, p: Vec2) {
        lanes::set(&mut self.pos_x, i, p.x);
        lanes::set(&mut self.pos_y, i, p.y);
    }
    #[inline]
    pub fn vel(&self, i: usize) -> Vec2 {
        Vec2::new(lanes::get(&self.vel_x, i), lanes::get(&self.vel_y, i))
    }
    #[inline]
    pub fn set_vel(&mut self, i: usize, v: Vec2) {
        lanes::set(&mut self.vel_x, i, v.x);
        lanes::set(&mut self.vel_y, i, v.y);
    }
    #[inline]
    pub fn flags(&self, i: usize) -> u8 {
        self.stat[i][stat::FLAGS]
    }
    #[inline]
    pub fn is_dead(&self, i: usize) -> bool {
        self.stat[i][stat::FLAGS] & flag::DEAD != 0
    }

    /// Append an agent. The caller makes sure there is room.
    pub fn push(&mut self, s: Spawn) -> usize {
        let i = self.len;
        assert!(i < self.cap, "agent table full");
        self.len += 1;
        self.set_pos(i, s.pos);
        self.set_vel(i, Vec2::ZERO);
        self.pose[i] = [s.heading, 0, 0, 0];
        self.emo[i] = s.emo;
        self.stat[i] = [s.health, s.action, s.archetype, s.flags & !flag::DEAD];
        self.intent[i] = s.intent;
        self.belief[i] = s.belief;
        self.stable_id[i] = s.stable_id;
        self.aux[i] = aux::NO_TARGET;
        i
    }

    /// Gather `src[order[k]]` into `self[k]` for `k < order.len()`, and make
    /// everything after that padding. Parallel over fixed chunks.
    pub fn permute_from(&mut self, src: &People, order: &[u32]) {
        let n = order.len();
        gather(
            lanes::flat_mut(&mut self.pos_x),
            lanes::flat(&src.pos_x),
            order,
            0.0,
        );
        gather(
            lanes::flat_mut(&mut self.pos_y),
            lanes::flat(&src.pos_y),
            order,
            0.0,
        );
        gather(
            lanes::flat_mut(&mut self.vel_x),
            lanes::flat(&src.vel_x),
            order,
            0.0,
        );
        gather(
            lanes::flat_mut(&mut self.vel_y),
            lanes::flat(&src.vel_y),
            order,
            0.0,
        );
        gather(&mut self.pose, &src.pose, order, [0; 4]);
        gather(&mut self.emo, &src.emo, order, [0; 4]);
        gather(&mut self.stat, &src.stat, order, DEAD_STAT);
        gather(&mut self.intent, &src.intent, order, 0);
        gather(&mut self.belief, &src.belief, order, 0);
        gather(&mut self.stable_id, &src.stable_id, order, u32::MAX);
        gather(&mut self.aux, &src.aux, order, aux::NO_TARGET);
        self.len = n;
    }

    /// Bytes of hot and warm state for `n` agents.
    pub const fn state_bytes(n: usize) -> usize {
        n * STATE_BYTES_PER_AGENT
    }
}

fn gather<T: Copy + Send + Sync>(dst: &mut [T], src: &[T], order: &[u32], pad: T) {
    let n = order.len();
    dst.par_chunks_mut(CHUNK_AGENTS)
        .enumerate()
        .for_each(|(c, out)| {
            let base = c * CHUNK_AGENTS;
            for (k, d) in out.iter_mut().enumerate() {
                let g = base + k;
                *d = if g < n { src[order[g] as usize] } else { pad };
            }
        });
}

/// Rarely-touched data, indexed by `stable_id` (DESIGN §6.2 "cold").
#[derive(Clone, Copy, Debug, Default)]
pub struct Cold {
    pub appearance: u32,
    /// Entry this person arrived through, and leaves through.
    pub home: u16,
    /// [`crate::state::Item`] held.
    pub item: u8,
    pub archetype: u8,
    /// Tick of the last violent act (throw, strike), for cooldowns.
    pub last_act: u32,
    /// Tick this person arrived, or `u32::MAX` if never.
    pub arrived: u32,
    /// Tick this person left (arrested, home, dead), or `u32::MAX`.
    pub left: u32,
    /// Times this person was filmed starting violence.
    pub filmed: u16,
    /// Hits taken.
    pub hits: u16,
    /// [`crate::outcome::Harm`] of the last health lost.
    pub last_harm: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_40_bytes() {
        let per = 4 * 4 + 4 + 4 + 4 + 2 + 2 + 4 + 4;
        assert_eq!(per, STATE_BYTES_PER_AGENT);
    }

    const _: () = assert!(STATE_BYTES_PER_AGENT <= 40);

    #[test]
    fn push_and_permute() {
        let mut p = People::new(10);
        assert_eq!(p.cap, CHUNK_AGENTS);
        for k in 0..5u32 {
            p.push(Spawn {
                pos: Vec2::new(k as f32, 0.0),
                heading: 0,
                emo: [k as u8; 4],
                health: 255,
                action: 0,
                archetype: 0,
                flags: 0,
                intent: 0,
                belief: 0,
                stable_id: 100 + k,
            });
        }
        let src = p.clone();
        p.permute_from(&src, &[4, 2, 0]);
        assert_eq!(p.len, 3);
        assert_eq!(p.stable_id[..4], [104, 102, 100, u32::MAX]);
        assert_eq!(p.pos(1), Vec2::new(2.0, 0.0));
        assert!(p.is_dead(3));
    }
}
