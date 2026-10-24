//! Phase 2: body hash, neighbour lists, field index and the periodic spatial
//! reorder (DESIGN §7.2–§7.4).
//!
//! - The **body hash** is a dense spatial hash of 2N buckets over cells one
//!   reach wide (Müller, *Ten Minute Physics*), built with a stable counting
//!   sort. Cost follows the number of people, never the map size.
//! - **Neighbour lists** hold up to K = 8 people within touching reach plus a
//!   skin, as `u16` indices, in bucket order. Steering, contacts and
//!   interactions all read them; nothing scans the hash again in the tick.
//! - The **field index** sorts agents by the Morton code of their 2 m field
//!   cell with a two-pass radix sort, so each cell's agents are a contiguous
//!   run and field scatters become segmented reductions (DESIGN §8).

use rayon::prelude::*;
use riot_core::grid::{GridGeom, morton2};
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::{Buckets, RadixSort24};

use crate::archetype::{ARCHETYPES, ArchetypeParams};
use crate::people::People;
use crate::state::{Action, Gait, flag, pose, stat};

/// Neighbour list capacity (DESIGN §7.3).
pub const K: usize = 8;
/// Field index key of removed agents: sorts last.
pub const DEAD_KEY: u32 = 0x00ff_ffff;
/// Field cell size (DESIGN §8).
pub const FIELD_CELL: f32 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// Morton code of the field cell.
    pub key: u32,
    /// `[start, end)` into the field order.
    pub start: u32,
    pub end: u32,
}

/// The runs of one field chunk (64 × 64 cells), which are contiguous because
/// the Morton code's high 12 bits are the chunk's Morton code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkRuns {
    /// Morton code of the chunk.
    pub chunk: u32,
    /// `[start, end)` into `runs`.
    pub start: u32,
    pub end: u32,
}

#[inline]
pub fn body_bucket(cx: i32, cy: i32, mask: u32) -> u32 {
    ((cx as u32).wrapping_mul(0x9E37_79B1) ^ (cy as u32).wrapping_mul(0x85EB_CA77)) & mask
}

pub struct Spatial {
    /// Body cell size: the largest possible contact reach.
    pub cell: f32,
    pub inv_cell: f32,
    pub half: f32,
    pub mask: u32,
    pub sentinel: u32,
    pub skin: f32,
    // --- per-agent scratch, rebuilt every tick (not state)
    pub bucket_of: Vec<u32>,
    pub cell_key: Vec<u32>,
    pub radius: Vec<f32>,
    pub inv_mass: Vec<f32>,
    pub nbr: Vec<[u16; K]>,
    pub nbr_n: Vec<u8>,
    pub field_key: Vec<u32>,
    pub buckets: Buckets,
    // --- field index
    pub fgeom: GridGeom,
    pub fsort: RadixSort24,
    pub runs: Vec<Run>,
    pub chunk_runs: Vec<ChunkRuns>,
    // --- awake agents per parallel chunk (DESIGN §7.6)
    pub awake: Vec<u16>,
    pub awake_n: Vec<u16>,
    pub awake_total: u32,
    /// Live (not dead) agents at the last rebuild.
    pub live: u32,
}

impl Spatial {
    pub fn new(cap: usize, world_size: f32, max_radius: f32, skin: f32) -> Self {
        let cell = 2.0 * max_radius + skin;
        let buckets = (2 * cap).next_power_of_two();
        let chunks = cap / CHUNK_AGENTS;
        Spatial {
            cell,
            inv_cell: 1.0 / cell,
            half: world_size * 0.5,
            mask: buckets as u32 - 1,
            sentinel: buckets as u32,
            skin,
            bucket_of: vec![0; cap],
            cell_key: vec![u32::MAX; cap],
            radius: vec![0.0; cap],
            inv_mass: vec![0.0; cap],
            nbr: vec![[0; K]; cap],
            nbr_n: vec![0; cap],
            field_key: vec![DEAD_KEY; cap],
            buckets: Buckets::new(buckets + 1, cap),
            fgeom: GridGeom::new(world_size, FIELD_CELL),
            fsort: RadixSort24::new(cap),
            runs: Vec::with_capacity(cap),
            chunk_runs: Vec::with_capacity(cap),
            awake: vec![0; cap],
            awake_n: vec![0; chunks],
            awake_total: 0,
            live: 0,
        }
    }

    /// Field cell Morton key for every agent (DEAD_KEY for removed ones).
    pub fn compute_field_keys(&mut self, p: &People) {
        let len = p.len;
        let g = self.fgeom;
        let (px, py) = (lanes::flat(&p.pos_x), lanes::flat(&p.pos_y));
        self.field_key[..len]
            .par_chunks_mut(CHUNK_AGENTS)
            .enumerate()
            .for_each(|(c, keys)| {
                let base = c * CHUNK_AGENTS;
                for (k, key) in keys.iter_mut().enumerate() {
                    let i = base + k;
                    *key = if p.stat[i][stat::FLAGS] & flag::DEAD != 0 {
                        DEAD_KEY
                    } else {
                        let (cx, cy) = g.cell_of(px[i], py[i]);
                        morton2(cx, cy).min(DEAD_KEY - 1)
                    };
                }
            });
    }

    /// Sort agents by field key. Returns the number of live agents, which come
    /// first in `fsort.order`.
    pub fn sort_field(&mut self, len: usize) -> usize {
        self.fsort.sort(&self.field_key[..len]);
        let keys = &self.field_key;
        self.fsort
            .order
            .partition_point(|&i| keys[i as usize] != DEAD_KEY)
    }

    /// Rebuild everything for this tick.
    pub fn rebuild(&mut self, p: &People, arch: &[ArchetypeParams; ARCHETYPES]) {
        let len = p.len;
        let chunks = p.chunks();
        let span = chunks * CHUNK_AGENTS;
        let (inv_cell, half, mask, sentinel) = (self.inv_cell, self.half, self.mask, self.sentinel);
        let (px, py) = (lanes::flat(&p.pos_x), lanes::flat(&p.pos_y));

        // Per-agent scratch.
        self.bucket_of[..span]
            .par_chunks_mut(CHUNK_AGENTS)
            .zip(self.cell_key[..span].par_chunks_mut(CHUNK_AGENTS))
            .zip(self.radius[..span].par_chunks_mut(CHUNK_AGENTS))
            .zip(self.inv_mass[..span].par_chunks_mut(CHUNK_AGENTS))
            .enumerate()
            .for_each(|(c, (((bucket, key), rad), inv_m))| {
                let base = c * CHUNK_AGENTS;
                for k in 0..bucket.len() {
                    let i = base + k;
                    let st = p.stat[i];
                    if i >= len || st[stat::FLAGS] & flag::DEAD != 0 {
                        bucket[k] = sentinel;
                        key[k] = u32::MAX;
                        rad[k] = 0.0;
                        inv_m[k] = 0.0;
                        continue;
                    }
                    let a = &arch[st[stat::ARCH] as usize];
                    let down = Gait::from_u8(p.pose[i][pose::GAIT]).is_down();
                    rad[k] = a.radius;
                    // Fallen people are hard to shift; sleepers are moved by
                    // nobody; a snatch squad shoves its way in with shields.
                    inv_m[k] = if st[stat::FLAGS] & flag::ASLEEP != 0 {
                        0.0
                    } else if down {
                        1.0 / (a.mass * 3.0)
                    } else if st[stat::ACTION] == Action::Arrest as u8 {
                        1.0 / (a.mass * 4.0)
                    } else {
                        1.0 / a.mass
                    };
                    let cx = ((px[i] + half) * inv_cell) as i32;
                    let cy = ((py[i] + half) * inv_cell) as i32;
                    key[k] = (cx as u32 & 0xffff) | ((cy as u32) << 16);
                    bucket[k] = body_bucket(cx, cy, mask);
                }
            });
        self.buckets.rebuild(&self.bucket_of[..len]);

        // Field index.
        let live = self.sort_field(len);
        self.live = live as u32;
        self.runs.clear();
        self.chunk_runs.clear();
        let order = &self.fsort.order;
        let mut s = 0usize;
        while s < live {
            let key = self.field_key[order[s] as usize];
            let mut e = s + 1;
            while e < live && self.field_key[order[e] as usize] == key {
                e += 1;
            }
            let chunk = key >> 12;
            if self.chunk_runs.last().is_none_or(|c| c.chunk != chunk) {
                let r = self.runs.len() as u32;
                self.chunk_runs.push(ChunkRuns {
                    chunk,
                    start: r,
                    end: r,
                });
            }
            self.runs.push(Run {
                key,
                start: s as u32,
                end: e as u32,
            });
            self.chunk_runs.last_mut().unwrap().end = self.runs.len() as u32;
            s = e;
        }

        // Neighbour lists (DESIGN §7.3).
        let skin = self.skin;
        let buckets = &self.buckets;
        let cell_key = &self.cell_key;
        let radius = &self.radius;
        self.nbr[..span]
            .par_chunks_mut(CHUNK_AGENTS)
            .zip(self.nbr_n[..span].par_chunks_mut(CHUNK_AGENTS))
            .enumerate()
            .for_each(|(c, (nb, nn))| {
                let base = c * CHUNK_AGENTS;
                for k in 0..nb.len() {
                    let i = base + k;
                    nn[k] = 0;
                    if i >= len || p.stat[i][stat::FLAGS] & (flag::DEAD | flag::ASLEEP) != 0 {
                        continue;
                    }
                    let key = cell_key[i];
                    let (cx, cy) = ((key & 0xffff) as i32, (key >> 16) as i32);
                    let (xi, yi, ri) = (px[i], py[i], radius[i]);
                    let mut n = 0usize;
                    'scan: for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (x, y) = (cx + dx, cy + dy);
                            let want = (x as u32 & 0xffff) | ((y as u32) << 16);
                            for &j in buckets.bucket(body_bucket(x, y, mask)) {
                                let j = j as usize;
                                if j == i || cell_key[j] != want {
                                    continue;
                                }
                                let (ddx, ddy) = (px[j] - xi, py[j] - yi);
                                let reach = ri + radius[j] + skin;
                                if ddx * ddx + ddy * ddy < reach * reach {
                                    nb[k][n] = j as u16;
                                    n += 1;
                                    if n == K {
                                        break 'scan;
                                    }
                                }
                            }
                        }
                    }
                    nn[k] = n as u8;
                }
            });

        // Awake lists per chunk.
        self.awake[..span]
            .par_chunks_mut(CHUNK_AGENTS)
            .zip(self.awake_n[..chunks].par_iter_mut())
            .enumerate()
            .for_each(|(c, (list, count))| {
                let base = c * CHUNK_AGENTS;
                let mut n = 0;
                for k in 0..CHUNK_AGENTS {
                    let i = base + k;
                    if i < len && p.stat[i][stat::FLAGS] & (flag::DEAD | flag::ASLEEP) == 0 {
                        list[n] = k as u16;
                        n += 1;
                    }
                }
                *count = n as u16;
            });
        self.awake_n[chunks..].fill(0);
        self.awake_total = self.awake_n.iter().map(|&n| n as u32).sum();
    }

    /// Awake agents (local indices) of parallel chunk `c`.
    #[inline]
    pub fn awake_in(&self, c: usize) -> &[u16] {
        &self.awake[c * CHUNK_AGENTS..c * CHUNK_AGENTS + self.awake_n[c] as usize]
    }

    #[inline]
    pub fn neighbours(&self, i: usize) -> &[u16] {
        &self.nbr[i][..self.nbr_n[i] as usize]
    }

    /// Every agent within `r` of `(x, y)`, through the body hash. For rare
    /// queries (interactions), not hot loops.
    pub fn query_radius(
        &self,
        px: &[f32],
        py: &[f32],
        x: f32,
        y: f32,
        r: f32,
        mut f: impl FnMut(usize),
    ) {
        let span = (r * self.inv_cell) as i32 + 1;
        let cx = ((x + self.half) * self.inv_cell) as i32;
        let cy = ((y + self.half) * self.inv_cell) as i32;
        for dy in -span..=span {
            for dx in -span..=span {
                let (gx, gy) = (cx + dx, cy + dy);
                let want = (gx as u32 & 0xffff) | ((gy as u32) << 16);
                for &j in self.buckets.bucket(body_bucket(gx, gy, self.mask)) {
                    let j = j as usize;
                    if self.cell_key[j] != want {
                        continue;
                    }
                    let (ddx, ddy) = (px[j] - x, py[j] - y);
                    if ddx * ddx + ddy * ddy <= r * r {
                        f(j);
                    }
                }
            }
        }
    }
}
