//! Flow fields (DESIGN §9): people's general direction toward a goal.
//!
//! Routes are found on two levels, in the style of HPA* and Emerson's flow
//! field tiles:
//!
//! 1. **Regions.** A Dijkstra over region components (128 m) from the goal
//!    gives every region a cost and a next region toward the goal.
//! 2. **Corridor.** When people with this goal show up in a region, the chain
//!    of regions from there to the goal (plus a one-region margin) joins the
//!    goal's corridor.
//! 3. **Tiles.** A Dijkstra over tile components (8 m) restricted to the
//!    corridor gives each tile component a cost. Dynamic costs (density, gas, fire)
//!    enter here and are refreshed about once a second.
//! 4. **Flow tiles.** When people enter a tile, its 16 × 16 direction field is
//!    built by a cell-level Dijkstra seeded from the downhill neighbour tiles,
//!    and swapped in exactly `flow_latency` ticks after it was requested, so
//!    determinism never depends on how fast a build ran.
//!
//! A 6 km route never searches the whole map, and map size costs nothing.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use riot_core::Vec2;
use riot_core::math::{HEADING_DIRS, heading_of};
use riot_team::prelude::*;

use crate::fields::{Fields, Layer};
use crate::map::{BLOCKED, COST_PER_M, MAX_COMP, Map, OPEN, REGION, TILE, TILE_M};
use crate::tunables::Tunables;

pub const MAX_GOALS: usize = 32;
pub const MAX_TARGETS: usize = 64;
/// Objective slots map to goals `0..16`.
pub const OBJECTIVE_GOALS: usize = 16;
pub const GOAL_EXITS: u8 = 16;
pub const GOAL_WATER: u8 = 17;
pub const GOAL_MEDIC: u8 = 18;
/// Police goals are `GOAL_POLICE..MAX_GOALS`.
pub const GOAL_POLICE: u8 = 19;
pub const NO_GOAL: u8 = u8::MAX;
pub const NONE: u32 = u32::MAX;
pub const PENDING: u32 = u32::MAX - 1;
pub const INF: u32 = u32::MAX;
/// Per-tile entries in a cost page: 256 tiles × 4 components.
const PAGE_COST: usize = (REGION * REGION) as usize * 4;
const PAGE_TILES: usize = (REGION * REGION) as usize;
/// Cost of crossing one open tile (8 m at 20 units per metre).
const TILE_STEP: u32 = 160;
/// Buckets in Dial's queue: a power of two, more than the largest tile step
/// cost (a diagonal step at the largest penalty, 3620).
const DIAL_BUCKETS: usize = 4096;
const _: () = assert!(DIAL_BUCKETS.is_power_of_two());
const _: () = assert!(MAX_STEP as usize * DIAG_256 as usize / 256 < DIAL_BUCKETS);
/// Largest tile step cost (`TILE_STEP` plus the largest penalty): crowds,
/// gas and fire make a tile up to 4× as long, police barriers up to 16×.
const MAX_STEP: u32 = TILE_STEP * 16;
/// Largest penalty from crowds, gas and fire (in tenths of a step).
const MAX_SOFT_PEN: f32 = 24.0;
/// Most flow tiles built per tick; further requests wait a tick.
const MAX_JOBS_PER_TICK: usize = 96;
/// Most corridor recomputes per tick.
const MAX_RECOMPUTES_PER_TICK: usize = 2;
/// A node's anchor is goal target `k` when this bit is set (`ANCHOR_TARGET | k`).
const ANCHOR_TARGET: u32 = 1 << 31;
/// √2 in 1/256ths, for diagonal tile steps.
const DIAG_256: u32 = 362;
/// Neighbours in the tile search: orthogonal first, then diagonal.
const TILE_STEPS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (-1, 1),
    (1, -1),
    (-1, -1),
];
/// Route cost units (0.1 m) by which a straight line may exceed a turn and
/// still win: rounding, not geometry, decides closer calls.
const STRAIGHT_SLACK: u32 = 2;

#[derive(Clone, Debug)]
pub struct Goal {
    pub active: bool,
    pub targets: Vec<Vec2>,
    pub radius: f32,
    /// Target generation: flow tiles from another generation are useless.
    pub generation: u32,
    /// Recompute epoch; tiles record the epoch their costs last changed.
    pub version: u32,
    /// Last tick anyone asked for or used this goal's flow.
    pub last_used: u32,
    pub rcost: Vec<u32>,
    pub rnext: Vec<u32>,
    pub region_ready: bool,
    pub corridor: Vec<bool>,
    /// Region → cost page.
    pub pages: Vec<u32>,
    pub page_list: Vec<u32>,
    pub want_regions: Vec<u32>,
    pub dirty: bool,
    pub last_refresh: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct FlowMeta {
    pub goal: u8,
    pub in_use: bool,
    /// Built but not yet swapped in.
    pub pending: bool,
    pub rebuild_queued: bool,
    pub tile: u32,
    pub generation: u32,
    pub version: u32,
    pub last_used: u32,
}

#[derive(Clone, Copy, Debug)]
struct Job {
    goal: u8,
    tile: u32,
    slot: u32,
    ready: u32,
}

/// Result of looking up a flow tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Ready(u32),
    /// Usable, but built from older costs.
    Stale(u32),
    Missing,
}

/// A request or usage record from steering, packed for the per-chunk buffers.
#[inline]
pub fn record(goal: u8, tile: u32, request: bool) -> u64 {
    ((request as u64) << 63) | ((goal as u64) << 32) | tile as u64
}

pub struct FlowSystem {
    pub goals: Vec<Goal>,
    pub page_cost: Vec<u32>,
    /// Anchor of each tile component (Theta*'s parent): the point its route
    /// heads straight for. A node key `(tile << 2) | comp`, `ANCHOR_TARGET |
    /// k` for goal target `k`, or `NONE`.
    pub page_anchor: Vec<u32>,
    /// Costs before the latest recompute, to detect what changed.
    page_prev: Vec<u32>,
    /// Cost of entering each tile, refreshed with the costs.
    page_step: Vec<u32>,
    /// Epoch at which each tile's costs (or a neighbour's) last changed
    /// noticeably: its flow tile is stale if built before that.
    pub page_epoch: Vec<u32>,
    page_region: Vec<u32>,
    /// Dial's bucket queue (DESIGN §9).
    dial: Vec<Vec<u32>>,
    pub page_flow: Vec<u32>,
    pub page_free: Vec<u32>,
    pub meta: Vec<FlowMeta>,
    pub dirs: Vec<u8>,
    tile_free: Vec<u32>,
    clock: usize,
    jobs: Vec<Job>,
    new_jobs: Vec<Job>,
    job_bufs: Vec<JobBuf>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
    merged: Vec<u64>,
    /// Requests and usage from steering, one buffer per agent chunk.
    pub records: Vec<Vec<u64>>,
    pub stats: FlowStats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FlowStats {
    pub built: u64,
    pub evicted: u64,
    pub recomputes: u64,
    pub page_overflow: u64,
    pub tile_overflow: u64,
    pub heap_overflow: u64,
}

impl FlowSystem {
    pub fn new(map: &Map, flow_tiles: usize, pages: usize, agent_chunks: usize) -> Self {
        let regions = (map.rdim * map.rdim) as usize;
        let rn = map.rnodes.len();
        let make_goal = || Goal {
            active: false,
            targets: Vec::with_capacity(MAX_TARGETS),
            radius: 4.0,
            generation: 0,
            version: 0,
            last_used: 0,
            rcost: vec![INF; rn],
            rnext: vec![NONE; rn],
            region_ready: false,
            corridor: vec![false; regions],
            pages: vec![NONE; regions],
            page_list: Vec::with_capacity(regions),
            want_regions: Vec::with_capacity(regions),
            dirty: false,
            last_refresh: 0,
        };
        let meta = FlowMeta {
            goal: 0,
            in_use: false,
            pending: false,
            rebuild_queued: false,
            tile: 0,
            generation: 0,
            version: 0,
            last_used: 0,
        };
        FlowSystem {
            // One at a time: cloning a Vec does not keep its capacity.
            goals: (0..MAX_GOALS).map(|_| make_goal()).collect(),
            page_cost: vec![INF; pages * PAGE_COST],
            page_anchor: vec![NONE; pages * PAGE_COST],
            page_prev: vec![INF; pages * PAGE_COST],
            page_step: vec![TILE_STEP; pages * PAGE_TILES],
            page_epoch: vec![0; pages * PAGE_TILES],
            page_region: vec![NONE; pages],
            dial: (0..DIAL_BUCKETS).map(|_| Vec::with_capacity(128)).collect(),
            page_flow: vec![NONE; pages * PAGE_TILES],
            page_free: (0..pages as u32).rev().collect(),
            meta: vec![meta; flow_tiles],
            dirs: vec![0; flow_tiles * 256],
            tile_free: (0..flow_tiles as u32).rev().collect(),
            clock: 0,
            jobs: Vec::with_capacity(MAX_JOBS_PER_TICK * 64),
            new_jobs: Vec::with_capacity(MAX_JOBS_PER_TICK),
            job_bufs: (0..MAX_JOBS_PER_TICK)
                .map(|_| JobBuf {
                    dirs: [0; 256],
                    heap: SmallHeap::new(),
                })
                .collect(),
            heap: BinaryHeap::with_capacity(map.rnodes.len() * 8 + 64),
            merged: Vec::with_capacity(agent_chunks * 512),
            records: (0..agent_chunks).map(|_| Vec::with_capacity(512)).collect(),
            stats: FlowStats::default(),
        }
    }

    /// Point goal `g` at new targets. Everything built for it is discarded.
    pub fn set_targets(&mut self, g: u8, targets: &[Vec2], radius: f32, tick: u32) {
        self.release_goal(g);
        let goal = &mut self.goals[g as usize];
        goal.active = !targets.is_empty();
        goal.targets.clear();
        goal.targets
            .extend(targets.iter().take(MAX_TARGETS).copied());
        goal.radius = radius.max(1.0);
        goal.generation = goal.generation.wrapping_add(1);
        goal.region_ready = false;
        goal.dirty = true;
        goal.last_refresh = tick;
    }

    pub fn clear_goal(&mut self, g: u8) {
        self.set_targets(g, &[], 1.0, 0);
    }

    fn release_goal(&mut self, g: u8) {
        let goal = &mut self.goals[g as usize];
        for &p in &goal.page_list {
            let base = p as usize * PAGE_TILES;
            for e in &mut self.page_flow[base..base + PAGE_TILES] {
                if *e != NONE && *e != PENDING {
                    self.meta[*e as usize].in_use = false;
                    self.tile_free.push(*e);
                }
                *e = NONE;
            }
            self.page_free.push(p);
        }
        goal.page_list.clear();
        goal.pages.fill(NONE);
        goal.corridor.fill(false);
        goal.want_regions.clear();
        // Pending jobs for this goal die at swap time (generation mismatch).
    }

    /// Look up the flow tile of goal `g` for tile `t`.
    #[inline]
    pub fn lookup(&self, map: &Map, g: u8, t: u32) -> Lookup {
        let goal = &self.goals[g as usize];
        let r = map.region_of_tile(t);
        let page = goal.pages[r as usize];
        if page == NONE {
            return Lookup::Missing;
        }
        let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
        let local = ((ty % REGION) * REGION + tx % REGION) as usize;
        let at = page as usize * PAGE_TILES + local;
        let s = self.page_flow[at];
        if s == NONE || s == PENDING {
            return Lookup::Missing;
        }
        if self.meta[s as usize].version >= self.page_epoch[at] {
            Lookup::Ready(s)
        } else {
            Lookup::Stale(s)
        }
    }

    /// Direction byte of a nav cell in a flow tile: 0 = none, else heading + 1.
    #[inline]
    pub fn dir(&self, slot: u32, local_cell: usize) -> u8 {
        self.dirs[slot as usize * 256 + local_cell]
    }

    /// Cost-to-goal of a tile component, if settled.
    #[inline]
    pub fn tile_cost(&self, map: &Map, g: u8, t: u32, comp: u8) -> u32 {
        let goal = &self.goals[g as usize];
        let page = goal.pages[map.region_of_tile(t) as usize];
        if page == NONE {
            return INF;
        }
        let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
        let local = ((ty % REGION) * REGION + tx % REGION) as usize;
        self.page_cost[page as usize * PAGE_COST + local * 4 + comp as usize]
    }

    /// Anchor of a tile component (see [`FlowSystem::page_anchor`]), `NONE`
    /// when it has none.
    #[inline]
    pub fn tile_anchor(&self, map: &Map, g: u8, t: u32, comp: u8) -> u32 {
        let goal = &self.goals[g as usize];
        let page = goal.pages[map.region_of_tile(t) as usize];
        if page == NONE {
            return NONE;
        }
        let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
        let local = ((ty % REGION) * REGION + tx % REGION) as usize;
        self.page_anchor[page as usize * PAGE_COST + local * 4 + comp as usize]
    }

    /// Cost of entering tile `t` (`TILE_STEP` plus penalties), as of the
    /// goal's last recompute; `TILE_STEP` outside its corridor.
    #[inline]
    fn tile_step(&self, map: &Map, g: u8, t: u32) -> u32 {
        let goal = &self.goals[g as usize];
        let page = goal.pages[map.region_of_tile(t) as usize];
        if page == NONE {
            return TILE_STEP;
        }
        let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
        let local = ((ty % REGION) * REGION + tx % REGION) as usize;
        self.page_step[page as usize * PAGE_TILES + local]
    }

    /// Where an anchor is and the radius of the goal disc around it, if
    /// routes can head straight for it: a goal target, or the centre of a
    /// fully open tile.
    #[inline]
    pub fn anchor_point(&self, map: &Map, g: u8, a: u32) -> Option<(Vec2, f32)> {
        anchor_at(map, &self.goals[g as usize], a)
    }

    /// The route someone at `from` takes to goal `g`: the turning points of
    /// its any-angle path through the goal's corridor, `out[0] = from`, up
    /// to where it enters the goal's disc. Stops early where the corridor
    /// does not reach (yet) or when `out` is full. Returns the number of
    /// points written. Reads only settled tile costs, so it works before
    /// any flow tile is built.
    pub fn route(&self, map: &Map, g: u8, from: Vec2, out: &mut [Vec2]) -> usize {
        if out.is_empty() {
            return 0;
        }
        out[0] = from;
        let goal = &self.goals[g as usize];
        if !goal.active {
            return 1;
        }
        let (mut n, mut p, mut cost) = (1, from, INF);
        while n < out.len() {
            let Some((cx, cy)) = map.cell_of(p) else {
                break;
            };
            let Some(c) = map.comp_of(cx, cy) else { break };
            let t = map.tile_index(cx >> 4, cy >> 4);
            let here = self.tile_cost(map, g, t, c);
            // Downhill only: never loop.
            if here == 0 || here == INF || here >= cost {
                break;
            }
            cost = here;
            let a = self.tile_anchor(map, g, t, c);
            if a == NONE {
                break;
            }
            let next = match anchor_at(map, goal, a) {
                Some((q, r)) if a & ANCHOR_TARGET != 0 => {
                    // Into the goal's disc, and done.
                    let d = q - p;
                    let len = d.len();
                    out[n] = if len > r {
                        p + d * ((len - r) / len)
                    } else {
                        q
                    };
                    return n + 1;
                }
                Some((q, _)) if a >> 2 != t => q,
                // A tile with walls heads for itself: step to the cheapest
                // neighbour instead, at its nearest open spot.
                _ => {
                    let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
                    let mut best: Option<(u32, u32)> = None;
                    for (dx, dy) in TILE_STEPS {
                        let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= map.tdim as i32 || ny >= map.tdim as i32 {
                            continue;
                        }
                        let nt = map.tile_index(nx as u32, ny as u32);
                        for nc in 0..map.tile_ncomp(nt) {
                            let k = self.tile_cost(map, g, nt, nc);
                            if k < here && best.is_none_or(|b| k < b.0) {
                                best = Some((k, nt));
                            }
                        }
                    }
                    let Some((_, nt)) = best else { break };
                    let c = tile_centre(map, nt);
                    map.nearest_open(c, TILE_M * 0.5).unwrap_or(c)
                }
            };
            if (next - p).len2() < 1e-4 {
                break;
            }
            // Keep turning points only: a step straight on extends the last.
            let straight = n >= 2 && {
                let (a, b) = (
                    (p - out[n - 2]).normalize_or_zero(),
                    (next - p).normalize_or_zero(),
                );
                a.dot(b) > 0.995
            };
            if straight {
                out[n - 1] = next;
            } else {
                out[n] = next;
                n += 1;
            }
            p = next;
        }
        n
    }

    /// Phase 10: swap in finished tiles, take requests, recompute corridors,
    /// build the requested tiles.
    pub fn update(&mut self, map: &Map, fields: &Fields, tun: &Tunables, tick: u32) {
        let latency = tun.flow_latency.max(0.0) as u32;
        let refresh = tun.flow_refresh.max(1.0) as u32;

        // 1. Swap in jobs that are due.
        let mut k = 0;
        while k < self.jobs.len() {
            let j = self.jobs[k];
            if j.ready > tick {
                k += 1;
                continue;
            }
            self.jobs.swap_remove(k);
            self.finish_job(map, j);
        }
        // swap_remove reorders; keep the queue in a canonical order.
        self.jobs
            .sort_unstable_by_key(|j| (j.ready, j.goal, j.tile));

        // 2. Merge records from all chunks, in chunk order.
        self.merged.clear();
        for r in &mut self.records {
            self.merged.extend_from_slice(r);
            r.clear();
        }
        self.merged.sort_unstable();
        self.merged.dedup();
        self.new_jobs.clear();
        let merged = std::mem::take(&mut self.merged);
        for &rec in &merged {
            let request = rec >> 63 != 0;
            let g = ((rec >> 32) & 0xff) as u8;
            let t = rec as u32;
            if !self.goals[g as usize].active {
                continue;
            }
            self.goals[g as usize].last_used = tick;
            let r = map.region_of_tile(t);
            let page = self.goals[g as usize].pages[r as usize];
            if page == NONE {
                let goal = &mut self.goals[g as usize];
                if !goal.corridor[r as usize]
                    && !goal.want_regions.contains(&r)
                    && goal.want_regions.len() < goal.want_regions.capacity()
                {
                    goal.want_regions.push(r);
                }
                continue;
            }
            let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
            let local =
                page as usize * PAGE_TILES + ((ty % REGION) * REGION + tx % REGION) as usize;
            let s = self.page_flow[local];
            if !request {
                if s != NONE && s != PENDING {
                    self.meta[s as usize].last_used = tick;
                }
                continue;
            }
            if s == PENDING || self.new_jobs.len() >= MAX_JOBS_PER_TICK {
                continue;
            }
            if s != NONE {
                let m = self.meta[s as usize];
                if m.rebuild_queued || m.version >= self.page_epoch[local] {
                    continue;
                }
            }
            let Some(slot) = self.alloc_tile(map, tick) else {
                self.stats.tile_overflow += 1;
                continue;
            };
            let goal = &self.goals[g as usize];
            self.meta[slot as usize] = FlowMeta {
                goal: g,
                in_use: true,
                pending: true,
                rebuild_queued: false,
                tile: t,
                generation: goal.generation,
                version: goal.version,
                last_used: tick,
            };
            if s == NONE {
                self.page_flow[local] = PENDING;
            } else {
                self.meta[s as usize].rebuild_queued = true;
            }
            self.new_jobs.push(Job {
                goal: g,
                tile: t,
                slot,
                ready: tick + latency,
            });
        }
        self.merged = merged;

        // 3. Corridors and tile costs.
        let mut recomputes = 0;
        for g in 0..MAX_GOALS {
            let goal = &self.goals[g];
            if !goal.active {
                continue;
            }
            // Refresh dynamic costs about once a second, while anyone uses it.
            let used = tick.wrapping_sub(goal.last_used) < refresh * 4;
            let due = used
                && !goal.page_list.is_empty()
                && tick.wrapping_sub(goal.last_refresh) >= refresh;
            if (goal.dirty || !goal.want_regions.is_empty() || due)
                && recomputes < MAX_RECOMPUTES_PER_TICK
            {
                self.recompute(map, fields, tun, g as u8, tick);
                recomputes += 1;
            }
        }

        // 4. Build the new jobs in parallel, then copy them into their slots.
        let n = self.new_jobs.len();
        let mut out = std::mem::take(&mut self.job_bufs);
        {
            let this = &*self;
            out[..n]
                .par_iter_mut()
                .zip(this.new_jobs.par_iter())
                .for_each(|(dst, j)| {
                    build_tile(
                        this,
                        map,
                        fields,
                        tun,
                        j.goal,
                        j.tile,
                        &mut dst.dirs,
                        &mut dst.heap,
                    );
                });
        }
        self.job_bufs = out;
        for k in 0..n {
            let j = self.new_jobs[k];
            let s = j.slot as usize;
            self.dirs[s * 256..(s + 1) * 256].copy_from_slice(&self.job_bufs[k].dirs);
            self.stats.built += 1;
            if latency == 0 {
                self.finish_job(map, j);
            } else if self.jobs.len() < self.jobs.capacity() {
                self.jobs.push(j);
            } else {
                self.finish_job(map, j);
            }
        }
    }

    fn finish_job(&mut self, map: &Map, j: Job) {
        let goal = &self.goals[j.goal as usize];
        let m = self.meta[j.slot as usize];
        let page = goal.pages[map.region_of_tile(j.tile) as usize];
        if m.generation != goal.generation || !goal.active || page == NONE {
            // The goal moved on: drop the result.
            self.free_tile(j.slot);
            return;
        }
        let (tx, ty) = (j.tile & (map.tdim - 1), j.tile >> map.tshift);
        let local = page as usize * PAGE_TILES + ((ty % REGION) * REGION + tx % REGION) as usize;
        let cur = self.page_flow[local];
        if cur != NONE && cur != PENDING && cur != j.slot {
            self.free_tile(cur);
        }
        self.page_flow[local] = j.slot;
        self.meta[j.slot as usize].pending = false;
    }

    fn free_tile(&mut self, s: u32) {
        let m = &mut self.meta[s as usize];
        if m.in_use {
            m.in_use = false;
            m.pending = false;
            self.tile_free.push(s);
        }
    }

    /// A free flow tile slot, evicting the least recently used (CLOCK) when
    /// the pool is full.
    fn alloc_tile(&mut self, map: &Map, tick: u32) -> Option<u32> {
        if let Some(s) = self.tile_free.pop() {
            return Some(s);
        }
        let n = self.meta.len();
        for _ in 0..n {
            let s = self.clock;
            self.clock = (self.clock + 1) % n;
            let m = self.meta[s];
            if !m.in_use || m.pending || tick.wrapping_sub(m.last_used) < 20 {
                continue;
            }
            // Unlink from its page.
            let goal = &self.goals[m.goal as usize];
            let page = goal.pages[map.region_of_tile(m.tile) as usize];
            if page != NONE {
                let (tx, ty) = (m.tile & (map.tdim - 1), m.tile >> map.tshift);
                let local =
                    page as usize * PAGE_TILES + ((ty % REGION) * REGION + tx % REGION) as usize;
                if self.page_flow[local] == s as u32 {
                    self.page_flow[local] = NONE;
                }
            }
            self.meta[s].in_use = false;
            self.stats.evicted += 1;
            return Some(s as u32);
        }
        None
    }

    /// Region Dijkstra (if needed), corridor growth and the tile Dijkstra.
    fn recompute(&mut self, map: &Map, fields: &Fields, tun: &Tunables, g: u8, tick: u32) {
        self.stats.recomputes += 1;
        let gi = g as usize;
        if !self.goals[gi].region_ready {
            self.region_dijkstra(map, g);
        }
        // Grow the corridor along each wanted region's chain to the goal.
        let wants = std::mem::take(&mut self.goals[gi].want_regions);
        for &r in &wants {
            let first = map.region_first[r as usize];
            let last = map.region_first[r as usize + 1];
            for n in first..last {
                let mut cur = n;
                let mut steps = 0;
                while cur != NONE && steps < 4096 {
                    let reg = map.rnodes[cur as usize].region;
                    self.add_corridor_region(map, g, reg);
                    let next = self.goals[gi].rnext[cur as usize];
                    if next == cur {
                        break;
                    }
                    cur = next;
                    steps += 1;
                }
            }
            // A region with no route still gets a page so it stops asking.
            self.add_corridor_region(map, g, r);
        }
        let mut wants = wants;
        wants.clear();
        self.goals[gi].want_regions = wants;
        let goal = &mut self.goals[gi];
        goal.version += 1;
        goal.dirty = false;
        goal.last_refresh = tick;
        self.tile_dijkstra(map, fields, tun, g);
    }

    fn add_corridor_region(&mut self, map: &Map, g: u8, reg: u32) {
        let rdim = map.rdim as i32;
        let (rx, ry) = ((reg % map.rdim) as i32, (reg / map.rdim) as i32);
        for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            let (x, y) = (rx + dx, ry + dy);
            if x < 0 || y < 0 || x >= rdim || y >= rdim {
                continue;
            }
            let r = (y * rdim + x) as usize;
            let goal = &mut self.goals[g as usize];
            if goal.corridor[r] {
                continue;
            }
            let Some(p) = self.page_free.pop() else {
                self.stats.page_overflow += 1;
                return;
            };
            goal.corridor[r] = true;
            goal.pages[r] = p;
            goal.page_list.push(p);
            self.page_region[p as usize] = r as u32;
            self.page_prev[p as usize * PAGE_COST..(p as usize + 1) * PAGE_COST].fill(INF);
            self.page_epoch[p as usize * PAGE_TILES..(p as usize + 1) * PAGE_TILES].fill(0);
            self.page_cost[p as usize * PAGE_COST..(p as usize + 1) * PAGE_COST].fill(INF);
            self.page_anchor[p as usize * PAGE_COST..(p as usize + 1) * PAGE_COST].fill(NONE);
            self.page_flow[p as usize * PAGE_TILES..(p as usize + 1) * PAGE_TILES].fill(NONE);
        }
    }

    /// Tile components touching the goal's target discs, with the index of
    /// the target.
    fn for_each_source(map: &Map, goal: &Goal, mut f: impl FnMut(u32, u8, u32)) {
        for (k, &t) in goal.targets.iter().enumerate() {
            let r = goal.radius;
            let (x0, y0) = map.nav.to_cell_f(t.x - r, t.y - r);
            let (x1, y1) = map.nav.to_cell_f(t.x + r, t.y + r);
            let dim = map.nav.dim as f32;
            let (x0, y0) = (
                x0.clamp(0.0, dim - 1.0) as u32,
                y0.clamp(0.0, dim - 1.0) as u32,
            );
            let (x1, y1) = (
                x1.clamp(0.0, dim - 1.0) as u32,
                y1.clamp(0.0, dim - 1.0) as u32,
            );
            for ty in y0 >> 4..=y1 >> 4 {
                for tx in x0 >> 4..=x1 >> 4 {
                    let tile = map.tile_index(tx, ty);
                    let mut seen = [false; 4];
                    for cy in (ty * TILE).max(y0)..=((ty + 1) * TILE - 1).min(y1) {
                        for cx in (tx * TILE).max(x0)..=((tx + 1) * TILE - 1).min(x1) {
                            let (wx, wy) = map.nav.centre(cx, cy);
                            if (Vec2::new(wx, wy) - t).len2() > r * r {
                                continue;
                            }
                            if let Some(c) = map.comp_of(cx, cy)
                                && !seen[c as usize]
                            {
                                seen[c as usize] = true;
                                f(tile, c, k as u32);
                            }
                        }
                    }
                }
            }
        }
    }

    fn region_dijkstra(&mut self, map: &Map, g: u8) {
        let gi = g as usize;
        let goal = &mut self.goals[gi];
        goal.rcost.fill(INF);
        goal.rnext.fill(NONE);
        let heap = &mut self.heap;
        heap.clear();
        {
            let goal = &self.goals[gi];
            let mut sources = [0u32; 256];
            let mut ns = 0;
            Self::for_each_source(map, goal, |t, c, _| {
                if let Some(n) = map.rnode_of(t, c)
                    && ns < sources.len()
                    && !sources[..ns].contains(&n)
                {
                    sources[ns] = n;
                    ns += 1;
                }
            });
            let goal = &mut self.goals[gi];
            for &n in &sources[..ns] {
                if goal.rcost[n as usize] != 0 {
                    goal.rcost[n as usize] = 0;
                    goal.rnext[n as usize] = n;
                    heap.push(Reverse((0, n)));
                }
            }
        }
        let goal = &mut self.goals[gi];
        while let Some(Reverse((c, n))) = heap.pop() {
            if c > goal.rcost[n as usize] {
                continue;
            }
            for &(m, w) in map.region_neighbours(n) {
                let nc = c.saturating_add(w);
                if nc < goal.rcost[m as usize] {
                    goal.rcost[m as usize] = nc;
                    goal.rnext[m as usize] = n;
                    if heap.len() < heap.capacity() {
                        heap.push(Reverse((nc, m)));
                    } else {
                        self.stats.heap_overflow += 1;
                    }
                }
            }
        }
        goal.region_ready = true;
    }

    /// Any-angle Dijkstra over tile components restricted to the corridor,
    /// with Dial's bucket queue over integer costs (DESIGN §9).
    ///
    /// Costs are Theta*-style (Nash, Daniel, Koenig and Felner, "Theta*:
    /// Any-Angle Path Planning on Grids", 2007): every node keeps an anchor,
    /// the last point its route turns at, and a node that sees its
    /// neighbour's anchor across open tiles takes the straight-line cost to
    /// it instead of a grid step. Line of sight is propagated, not traced: a
    /// node sees anchor `A` when the tile its centre-to-`A` line enters
    /// first is fully open and already sees `A`, so the test costs one
    /// lookup. The straight segment is charged each tile's own penalty, so
    /// crowds and gas still bend routes. Steps are 8-connected where the
    /// four tiles involved are open.
    ///
    /// Tiles whose costs changed noticeably, and their neighbours, get the
    /// new epoch so their flow tiles are rebuilt; everything else keeps its
    /// flow.
    fn tile_dijkstra(&mut self, map: &Map, fields: &Fields, tun: &Tunables, g: u8) {
        let gi = g as usize;
        let epoch = self.goals[gi].version;
        for &p in &self.goals[gi].page_list {
            let (a, b) = (p as usize * PAGE_COST, (p as usize + 1) * PAGE_COST);
            self.page_prev[a..b].copy_from_slice(&self.page_cost[a..b]);
            self.page_cost[a..b].fill(INF);
            self.page_anchor[a..b].fill(NONE);
            let r = self.page_region[p as usize];
            let (rx, ry) = (r % map.rdim, r / map.rdim);
            for local in 0..PAGE_TILES as u32 {
                let t = map.tile_index(rx * REGION + local % REGION, ry * REGION + local / REGION);
                self.page_step[p as usize * PAGE_TILES + local as usize] =
                    tile_step_cost(map, fields, tun, t, g < GOAL_POLICE);
            }
        }
        let goal = &self.goals[gi];
        let page_cost = &mut self.page_cost;
        let page_anchor = &mut self.page_anchor;
        let page_step = &self.page_step;
        let dial = &mut self.dial;
        for b in dial.iter_mut() {
            b.clear();
        }
        let page_of = |t: u32| -> Option<(usize, usize)> {
            let page = goal.pages[map.region_of_tile(t) as usize];
            if page == NONE {
                return None;
            }
            let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
            Some((
                page as usize,
                ((ty % REGION) * REGION + tx % REGION) as usize,
            ))
        };
        let idx = |t: u32, c: u8| page_of(t).map(|(p, l)| p * PAGE_COST + l * 4 + c as usize);
        let mut pending = 0usize;
        Self::for_each_source(map, goal, |t, c, k| {
            if let Some(i) = idx(t, c)
                && page_cost[i] != 0
            {
                page_cost[i] = 0;
                page_anchor[i] = ANCHOR_TARGET | k;
                dial[0].push((t << 2) | c as u32);
                pending += 1;
            }
        });
        let tdim = map.tdim;
        let in_map = |x: i32, y: i32| x >= 0 && y >= 0 && x < tdim as i32 && y < tdim as i32;
        // A fully open tile in the corridor: lines of sight may cross it.
        let clear = |x: i32, y: i32| {
            in_map(x, y) && {
                let t = map.tile_index(x as u32, y as u32);
                map.tiles[t as usize] == OPEN && page_of(t).is_some()
            }
        };
        let mut cur = 0u32;
        while pending > 0 {
            let b = cur as usize & (DIAL_BUCKETS - 1);
            if dial[b].is_empty() {
                cur += 1;
                continue;
            }
            let items = std::mem::take(&mut dial[b]);
            for &key in &items {
                pending -= 1;
                let (t, c) = (key >> 2, (key & 3) as u8);
                let Some(i) = idx(t, c) else { continue };
                if page_cost[i] != cur {
                    continue;
                }
                let (tx, ty) = (t & (tdim - 1), t >> map.tshift);
                let here_open = map.tiles[t as usize] == OPEN;
                let anchor = page_anchor[i];
                let sight = anchor_at(map, goal, anchor);
                // A grid step's anchor: this tile's centre when routes can
                // head for it, otherwise the neighbour itself.
                let step_anchor = if here_open { key } else { NONE };
                // Diagonal steps matter where routes turn, near their
                // anchor; along a straight run the neighbours' own lines of
                // sight already give the diagonal tiles their cost.
                let turning = here_open
                    && sight.is_none_or(|(a, _)| {
                        (a - tile_centre(map, t)).len2() < (1.5 * TILE_M) * (1.5 * TILE_M)
                    });
                let steps = if turning {
                    &TILE_STEPS[..]
                } else {
                    &TILE_STEPS[..4]
                };
                for &(dx, dy) in steps {
                    let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
                    if !in_map(nx, ny) {
                        continue;
                    }
                    let diagonal = dx != 0 && dy != 0;
                    let nt = map.tile_index(nx as u32, ny as u32);
                    let Some((np, nl)) = page_of(nt) else {
                        continue;
                    };
                    let nt_open = map.tiles[nt as usize] == OPEN;
                    // An open tile has one component; once it is expanded
                    // nothing here can improve it.
                    let j0 = np * PAGE_COST + nl * 4;
                    if nt_open && page_cost[j0] <= cur {
                        continue;
                    }
                    if diagonal
                        && !(nt_open
                            && clear(tx as i32 + dx, ty as i32)
                            && clear(tx as i32, ty as i32 + dy))
                    {
                        continue;
                    }
                    let step = page_step[np * PAGE_TILES + nl].min(MAX_STEP);
                    let grid = cur
                        + if diagonal {
                            step * DIAG_256 / 256
                        } else {
                            step
                        };
                    // Straight on from this node's anchor, when the
                    // neighbour sees it. A neighbour already heading for
                    // the same anchor has exactly that cost.
                    let straight = match sight {
                        Some((a, r)) if nt_open && page_anchor[j0] != anchor => {
                            let target = anchor & ANCHOR_TARGET != 0;
                            // The cost and distance to the anchor of the
                            // tile the line enters first.
                            let via = match toward(map, nx as u32, ny as u32, a) {
                                None => target.then_some((0, 0.0)),
                                Some((ex, ey)) => {
                                    let (px, py) = (nx + ex, ny + ey);
                                    let corner_ok = ex == 0
                                        || ey == 0
                                        || (clear(nx + ex, ny) && clear(nx, ny + ey));
                                    if corner_ok && clear(px, py) {
                                        let pt = map.tile_index(px as u32, py as u32);
                                        idx(pt, 0)
                                            .filter(|&q| {
                                                (page_anchor[q] == anchor || pt << 2 == anchor)
                                                    && page_cost[q] <= cur
                                            })
                                            .map(|q| {
                                                // A goal tile costs 0 however
                                                // far its centre is from the disc.
                                                let d = if target && page_cost[q] == 0 {
                                                    0.0
                                                } else {
                                                    anchor_dist(a, r, tile_centre(map, pt))
                                                };
                                                (page_cost[q], d)
                                            })
                                    } else {
                                        None
                                    }
                                }
                            };
                            via.map(|(base, from)| {
                                // Metres × 20 units/m × step/160, each end
                                // rounded on its own: along a uniform line the
                                // roundings cancel instead of adding up.
                                let units = |m: f32| (m * step as f32 * 0.125 + 0.5) as u32;
                                let to = units(anchor_dist(a, r, tile_centre(map, nt)));
                                // Never into the bucket being drained.
                                (base + to.saturating_sub(units(from))).max(cur + 1)
                            })
                        }
                        _ => None,
                    };
                    // The neighbour's components this one touches; open
                    // tiles have one component and always connect.
                    let links = if diagonal || (here_open && nt_open) {
                        1
                    } else {
                        link_mask(map, t, c, dx, dy)
                    };
                    for nc in 0..MAX_COMP as u8 {
                        if links & (1 << nc) == 0 {
                            continue;
                        }
                        let j = np * PAGE_COST + nl * 4 + nc as usize;
                        let node = (nt << 2) | nc as u32;
                        let (new, a) = match straight {
                            Some(s) if s <= grid + STRAIGHT_SLACK => (s, anchor),
                            _ if step_anchor == NONE => (grid, node),
                            _ => (grid, step_anchor),
                        };
                        // Within rounding, a straight line beats a turn,
                        // even one already queued (but not yet expanded).
                        let old = page_cost[j];
                        let straighter = a == anchor
                            && page_anchor[j] != anchor
                            && old > cur
                            && old != INF
                            && new <= old + STRAIGHT_SLACK;
                        if new < old || straighter {
                            page_cost[j] = new;
                            page_anchor[j] = a;
                            if new != old {
                                dial[new as usize & (DIAL_BUCKETS - 1)].push(node);
                                pending += 1;
                            }
                        }
                    }
                }
            }
            let mut items = items;
            items.clear();
            dial[b] = items;
            cur += 1;
        }
        // Mark tiles whose costs changed noticeably, and their neighbours.
        for k in 0..goal.page_list.len() {
            let p = goal.page_list[k] as usize;
            let r = self.page_region[p];
            let (rx, ry) = (r % map.rdim, r / map.rdim);
            for local in 0..PAGE_TILES {
                let base = p * PAGE_COST + local * 4;
                let changed = (0..4).any(|c| {
                    let (old, new) = (self.page_prev[base + c], self.page_cost[base + c]);
                    if old == INF || new == INF {
                        return old != new;
                    }
                    old.abs_diff(new) > old.max(new) / 25 + 40
                });
                if !changed {
                    continue;
                }
                let (tx, ty) = (
                    rx * REGION + local as u32 % REGION,
                    ry * REGION + local as u32 / REGION,
                );
                // Flow tiles are seeded from all eight neighbours.
                for (dx, dy) in [
                    (0i32, 0i32),
                    (1, 0),
                    (-1, 0),
                    (0, 1),
                    (0, -1),
                    (1, 1),
                    (-1, 1),
                    (1, -1),
                    (-1, -1),
                ] {
                    let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= tdim as i32 || ny >= tdim as i32 {
                        continue;
                    }
                    if let Some((np, nl)) = page_of(map.tile_index(nx as u32, ny as u32)) {
                        self.page_epoch[np * PAGE_TILES + nl] = epoch;
                    }
                }
            }
        }
    }
}

/// Centre of tile `t` in map coordinates.
#[inline]
pub fn tile_centre(map: &Map, t: u32) -> Vec2 {
    let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
    Vec2::new(
        tx as f32 * TILE_M - map.half + TILE_M * 0.5,
        ty as f32 * TILE_M - map.half + TILE_M * 0.5,
    )
}

/// Where anchor `a` of `goal` is and the radius of the goal disc around it,
/// if routes can head straight for it: a goal target, or the centre of a
/// fully open tile.
#[inline]
fn anchor_at(map: &Map, goal: &Goal, a: u32) -> Option<(Vec2, f32)> {
    if a == NONE {
        None
    } else if a & ANCHOR_TARGET != 0 {
        goal.targets
            .get((a & !ANCHOR_TARGET) as usize)
            .map(|&p| (p, goal.radius))
    } else {
        let t = a >> 2;
        (map.tiles[t as usize] == OPEN).then(|| (tile_centre(map, t), 0.0))
    }
}

/// Distance (m) from `p` to an anchor at `a` with goal radius `r`.
#[inline]
fn anchor_dist(a: Vec2, r: f32, p: Vec2) -> f32 {
    ((a - p).len() - r).max(0.0)
}

/// The neighbour of tile `(tx, ty)` that the straight line from its centre
/// to `a` crosses first, as an offset: `(±1, 0)`, `(0, ±1)`, or diagonal
/// when the line leaves through a corner. `None` when `a` is in the tile.
#[inline]
fn toward(map: &Map, tx: u32, ty: u32, a: Vec2) -> Option<(i32, i32)> {
    let d = (a - tile_centre(map, map.tile_index(tx, ty))) * (1.0 / TILE_M);
    let (ax, ay) = (d.x.abs(), d.y.abs());
    if ax <= 0.5 && ay <= 0.5 {
        return None;
    }
    let sx = if d.x > 0.0 { 1 } else { -1 };
    let sy = if d.y > 0.0 { 1 } else { -1 };
    Some(if ax > ay * 1.001 {
        (sx, 0)
    } else if ay > ax * 1.001 {
        (0, sy)
    } else {
        (sx, sy)
    })
}

/// Cost of entering tile `t`: its length plus crowd, gas and fire penalties,
/// and, for everyone but the police (who open their own), police barriers
/// across it.
fn tile_step_cost(map: &Map, fields: &Fields, tun: &Tunables, t: u32, barriers: bool) -> u32 {
    let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
    let cx = tx as f32 * TILE_M - map.half + TILE_M * 0.5;
    let cy = ty as f32 * TILE_M - map.half + TILE_M * 0.5;
    let dens = fields.sample(Layer::Dens, cx, cy);
    let gas = fields.sample(Layer::Gas, cx, cy);
    let fire = fields.sample(Layer::Fire, cx, cy);
    let pen = (tun.flow_density_cost * dens + tun.flow_gas_cost * gas + tun.flow_fire_cost * fire)
        .clamp(0.0, MAX_SOFT_PEN)
        + if barriers {
            tun.flow_block_cost * tile_block(fields, cx, cy)
        } else {
            0.0
        };
    (TILE_STEP + (TILE_STEP as f32 * pen * 0.1) as u32).min(MAX_STEP)
}

/// The most blocked of the 4 × 4 field cells of the tile centred on
/// `(cx, cy)`: a barrier is a thin line, which the centre alone would miss.
#[inline]
fn tile_block(fields: &Fields, cx: f32, cy: f32) -> f32 {
    if !fields.block_live {
        return 0.0;
    }
    let (fx, fy) = fields.geom.to_cell_f(cx - TILE_M * 0.5, cy - TILE_M * 0.5);
    let (x0, y0) = ((fx + 0.5).floor() as i32, (fy + 0.5).floor() as i32);
    let cc = crate::fields::CC as i32;
    match fields.slot_of_chunk(x0.div_euclid(cc), y0.div_euclid(cc)) {
        Some(s) if fields.blocked[s] => {}
        _ => return 0.0,
    }
    let mut m = 0.0f32;
    for dy in 0..4 {
        for dx in 0..4 {
            m = m.max(fields.cell_value(Layer::Block, x0 + dx, y0 + dy));
        }
    }
    m
}

/// The components of the orthogonal neighbour at `dx, dy` of tile `t` that
/// component `c` of `t` connects to: bit `b` for component `b`.
#[inline]
fn link_mask(map: &Map, t: u32, c: u8, dx: i32, dy: i32) -> u16 {
    let dir = match (dx, dy) {
        (1, _) => 0,
        (-1, _) => 1,
        (_, 1) => 2,
        _ => 3,
    };
    (map.tile_links(t, dir) >> (c * 4)) & 0xf
}

/// Output and scratch of one flow tile build, reused every tick.
struct JobBuf {
    dirs: [u8; 256],
    heap: SmallHeap,
}

/// A small fixed-capacity binary min-heap, so tile builds never allocate.
struct SmallHeap {
    items: [(u32, u16); 4096],
    len: usize,
}

impl SmallHeap {
    fn new() -> Self {
        SmallHeap {
            items: [(0, 0); 4096],
            len: 0,
        }
    }
    fn push(&mut self, cost: u32, cell: u16) {
        if self.len == self.items.len() {
            return;
        }
        let mut i = self.len;
        self.items[i] = (cost, cell);
        self.len += 1;
        while i > 0 {
            let p = (i - 1) / 2;
            if self.items[p] <= self.items[i] {
                break;
            }
            self.items.swap(p, i);
            i = p;
        }
    }
    fn pop(&mut self) -> Option<(u32, u16)> {
        if self.len == 0 {
            return None;
        }
        let top = self.items[0];
        self.len -= 1;
        self.items[0] = self.items[self.len];
        let mut i = 0;
        loop {
            let (l, r) = (2 * i + 1, 2 * i + 2);
            let mut m = i;
            if l < self.len && self.items[l] < self.items[m] {
                m = l;
            }
            if r < self.len && self.items[r] < self.items[m] {
                m = r;
            }
            if m == i {
                break;
            }
            self.items.swap(m, i);
            i = m;
        }
        Some(top)
    }
}

/// Side of the grid a flow tile is solved on: its 16 × 16 cells inside a
/// ring of the neighbouring tiles' cells.
const RING: usize = TILE as usize + 2;
const RING_CELLS: usize = RING * RING;
/// A nav cell's eight neighbours, in heading order (heading `k × 32`).
const NBR8: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// Build the direction field of tile `t` for goal `g` (DESIGN §9). Pure
/// function of its inputs.
///
/// The arrival-time field is solved with the Fast Marching Method (Sethian,
/// "A fast marching level set method for monotonically advancing fronts",
/// 1996): the Eikonal equation `|∇T| = slowness`, slowness being the step
/// cost of crowding, gas and fire. Its boundary is the goal's cells and a
/// ring of cells from the downhill neighbour tiles, valued by the distance
/// to the neighbour's anchor, so the field inside is the true straight-line
/// distance to wherever the route next turns, not a grid distance. Each
/// cell's direction is the field's downhill gradient at any of 256 angles.
/// A fully open tile whose whole boundary heads for one anchor skips the
/// solve: every cell points straight at the anchor.
fn build_tile(
    fs: &FlowSystem,
    map: &Map,
    fields: &Fields,
    tun: &Tunables,
    g: u8,
    t: u32,
    out: &mut [u8; 256],
    heap: &mut SmallHeap,
) {
    out.fill(0);
    heap.len = 0;
    let goal = &fs.goals[g as usize];
    let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
    let (cx0, cy0) = ((tx * TILE) as i32 - 1, (ty * TILE) as i32 - 1);
    // Ring coordinates: interior cell (lx, ly) is (lx + 1, ly + 1).
    let mut open = [false; RING_CELLS];
    for (r, o) in open.iter_mut().enumerate() {
        let (gx, gy) = (cx0 + (r % RING) as i32, cy0 + (r / RING) as i32);
        *o = gx >= 0 && gy >= 0 && map.walkable(gx as u32, gy as u32);
    }
    let ring = |i: usize| (i >> 4) * RING + (i & 15) + RING + 1;
    let mut comp = [u8::MAX; 256];
    for (i, c) in comp.iter_mut().enumerate() {
        if let Some(k) = map.comp_of(tx * TILE + (i as u32 & 15), ty * TILE + (i as u32 >> 4)) {
            *c = k;
        }
    }
    // Slowness: the cost of one cell step (10 on open ground), from
    // crowding, gas and fire per 2 m field cell.
    let mut slow = [10.0f32; 256];
    let fx0 = tx as f32 * TILE_M - map.half;
    let fy0 = ty as f32 * TILE_M - map.half;
    let mut uniform = true;
    for qy in 0..4 {
        for qx in 0..4 {
            let p = Vec2::new(fx0 + qx as f32 * 2.0 + 1.0, fy0 + qy as f32 * 2.0 + 1.0);
            let dens = fields.value_at(Layer::Dens, p);
            let gas = fields.value_at(Layer::Gas, p);
            let fire = fields.value_at(Layer::Fire, p);
            let block = if fields.block_live && g < GOAL_POLICE {
                fields.value_at(Layer::Block, p)
            } else {
                0.0
            };
            let pen = (tun.flow_density_cost * dens
                + tun.flow_gas_cost * gas
                + tun.flow_fire_cost * fire)
                .clamp(0.0, MAX_SOFT_PEN)
                + tun.flow_block_cost * block;
            let f = 10.0 + pen * 10.0;
            uniform &= f == 10.0;
            for ly in 0..4 {
                for lx in 0..4 {
                    slow[(qy * 4 + ly) * 16 + qx * 4 + lx] = f;
                }
            }
        }
    }
    let mut time = [f32::INFINITY; RING_CELLS];
    let mut done = [false; RING_CELLS];
    let mut goal_cell = [false; 256];
    let r2 = goal.radius * goal.radius;
    let mut goals = false;
    for i in 0..256usize {
        if comp[i] == u8::MAX {
            continue;
        }
        let (wx, wy) = map
            .nav
            .centre(tx * TILE + (i as u32 & 15), ty * TILE + (i as u32 >> 4));
        let p = Vec2::new(wx, wy);
        if goal.targets.iter().any(|&q| (q - p).len2() <= r2) {
            goal_cell[i] = true;
            goals = true;
            time[ring(i)] = 0.0;
            heap.push(0, ring(i) as u16);
        }
    }

    // The ring: cells of downhill neighbour tiles, valued from their tile's
    // cost and anchor. `one` is the anchor they all share, if they do.
    const UNSET: u32 = u32::MAX - 2;
    let mut one = UNSET;
    let mut own_cost = [INF; 4];
    for (c, o) in own_cost
        .iter_mut()
        .enumerate()
        .take(map.tile_ncomp(t) as usize)
    {
        *o = fs.tile_cost(map, g, t, c as u8);
    }
    // What the ring needs from each neighbour tile component, looked up once:
    // cost, anchor, anchor position and radius, step factor, and the anchor
    // distance its cost stands for.
    #[derive(Clone, Copy)]
    struct Nb {
        cost: u32,
        anchor: u32,
        at: Option<(Vec2, f32)>,
        factor: f32,
        from: f32,
    }
    let mut nbs: [[Option<Nb>; MAX_COMP]; 9] = [[None; MAX_COMP]; 9];
    for r in 0..RING_CELLS {
        let (x, y) = (r % RING, r / RING);
        if !open[r] || (x > 0 && x < RING - 1 && y > 0 && y < RING - 1) {
            continue;
        }
        let (gx, gy) = ((cx0 + x as i32) as u32, (cy0 + y as i32) as u32);
        let Some(nc) = map.comp_of(gx, gy) else {
            continue;
        };
        // Downhill from the tile components this cell borders.
        let mut own = INF;
        for (dx, dy) in [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            let (ix, iy) = (
                (x as i32 + dx).clamp(1, RING as i32 - 2) - 1,
                (y as i32 + dy).clamp(1, RING as i32 - 2) - 1,
            );
            let c = comp[(iy * 16 + ix) as usize];
            if c != u8::MAX {
                own = own.min(own_cost[c as usize]);
            }
        }
        let nt = map.tile_index(gx >> 4, gy >> 4);
        // Which of the 3 × 3 tiles around this one the cell is in.
        let third = |v: usize| {
            if v == 0 {
                0
            } else if v == RING - 1 {
                2
            } else {
                1
            }
        };
        let nb = *nbs[third(y) * 3 + third(x)][nc as usize].get_or_insert_with(|| {
            let cost = fs.tile_cost(map, g, nt, nc);
            let anchor = fs.tile_anchor(map, g, nt, nc);
            let at = fs.anchor_point(map, g, anchor);
            let from = match at {
                // A goal tile costs 0 however far its centre is from the disc.
                Some(_) if anchor & ANCHOR_TARGET != 0 && cost == 0 => 0.0,
                Some((ap, ar)) => anchor_dist(ap, ar, tile_centre(map, nt)),
                None => 0.0,
            };
            Nb {
                cost,
                anchor,
                at,
                factor: fs.tile_step(map, g, nt) as f32 * (1.0 / TILE_STEP as f32),
                from,
            }
        });
        let theirs = nb.cost;
        if theirs == INF || theirs >= own {
            continue;
        }
        let v = match nb.at {
            Some((ap, ar)) => {
                let (wx, wy) = map.nav.centre(gx, gy);
                let d = anchor_dist(ap, ar, Vec2::new(wx, wy)) - nb.from;
                one = if one == UNSET || one == nb.anchor {
                    nb.anchor
                } else {
                    NONE
                };
                // Heap keys are float bits: never -0.0.
                let v = theirs as f32 + d * COST_PER_M * nb.factor;
                if v > 0.0 { v } else { 0.0 }
            }
            None => {
                one = NONE;
                (theirs + TILE_STEP / 2) as f32 - 10.0
            }
        };
        if v < time[r] {
            time[r] = v;
            heap.push(v.to_bits(), r as u16);
        }
    }

    // Open ground heading for one anchor: straight at it.
    if !goals
        && uniform
        && one != UNSET
        && map.tiles[t as usize] == OPEN
        && let Some((ap, _)) = fs.anchor_point(map, g, one)
    {
        for (i, o) in out.iter_mut().enumerate() {
            let (wx, wy) = map
                .nav
                .centre(tx * TILE + (i as u32 & 15), ty * TILE + (i as u32 >> 4));
            let d = ap - Vec2::new(wx, wy);
            *o = heading_of(d.x, d.y).wrapping_add(1).max(1);
        }
        heap.len = 0;
        return;
    }

    // Fast Marching: accept the smallest tentative time, update its
    // interior neighbours from their accepted neighbours.
    let known = |done: &[bool; RING_CELLS], time: &[f32; RING_CELLS], k: usize| {
        if done[k] { time[k] } else { f32::INFINITY }
    };
    while let Some((bits, r)) = heap.pop() {
        let r = r as usize;
        if done[r] || f32::from_bits(bits) > time[r] {
            continue;
        }
        done[r] = true;
        let (x, y) = ((r % RING) as i32, (r / RING) as i32);
        for &(dx, dy) in &NBR8 {
            let (nx, ny) = (x + dx, y + dy);
            if !(1..=TILE as i32).contains(&nx) || !(1..=TILE as i32).contains(&ny) {
                continue;
            }
            let n = ny as usize * RING + nx as usize;
            if done[n] || !open[n] {
                continue;
            }
            let f = slow[(ny as usize - 1) * 16 + nx as usize - 1];
            let a = known(&done, &time, n - 1).min(known(&done, &time, n + 1));
            let b = known(&done, &time, n - RING).min(known(&done, &time, n + RING));
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let mut v = if hi - lo >= f {
                lo + f
            } else {
                // Both axes: the front arrives at an angle.
                0.5 * (lo + hi + (2.0 * f * f - (hi - lo) * (hi - lo)).sqrt())
            };
            for &(ex, ey) in &[(1i32, 1i32), (-1, 1), (-1, -1), (1, -1)] {
                let d = (ny + ey) as usize * RING + (nx + ex) as usize;
                if done[d]
                    && open[ny as usize * RING + (nx + ex) as usize]
                    && open[(ny + ey) as usize * RING + nx as usize]
                {
                    v = v.min(time[d] + f * std::f32::consts::SQRT_2);
                }
            }
            if v < time[n] {
                time[n] = v;
                heap.push(v.to_bits(), n as u16);
            }
        }
    }

    // Directions: the downhill (upwind) gradient.
    for i in 0..256usize {
        if comp[i] == u8::MAX || goal_cell[i] {
            continue;
        }
        let r = ring(i);
        let here = time[r];
        if !here.is_finite() {
            continue;
        }
        let (l, rt, u, d) = (time[r - 1], time[r + 1], time[r - RING], time[r + RING]);
        let mut gx = if rt < l && rt < here {
            here - rt
        } else if l < here {
            l - here
        } else {
            0.0
        };
        let mut gy = if d < u && d < here {
            here - d
        } else if u < here {
            u - here
        } else {
            0.0
        };
        // Never cut across a blocked corner.
        if gx != 0.0 && gy != 0.0 {
            let (sx, sy) = (if gx > 0.0 { 1 } else { -1 }, if gy > 0.0 { 1 } else { -1 });
            if !open[(r as i32 + sy * RING as i32 + sx) as usize] {
                if gx.abs() < gy.abs() {
                    gx = 0.0;
                } else {
                    gy = 0.0;
                }
            }
        }
        out[i] = if gx != 0.0 || gy != 0.0 {
            heading_of(gx, gy).wrapping_add(1).max(1)
        } else {
            // Degenerate gradient: the steepest walkable neighbour.
            let mut best = (here, 0u8);
            for (k, &(dx, dy)) in NBR8.iter().enumerate() {
                let n = (r as i32 + dy * RING as i32 + dx) as usize;
                let corner = dx == 0
                    || dy == 0
                    || (open[(r as i32 + dx) as usize]
                        && open[(r as i32 + dy * RING as i32) as usize]);
                if corner && time[n] < best.0 {
                    best = (time[n], (k as u8 * 32).wrapping_add(1));
                }
            }
            best.1
        };
    }
    heap.len = 0;
}

/// Decode a direction byte into a unit vector (zero for "no direction").
#[inline]
pub fn decode_dir(d: u8) -> Option<Vec2> {
    if d == 0 {
        None
    } else {
        let [c, s] = HEADING_DIRS[d.wrapping_sub(1) as usize];
        Some(Vec2::new(c, s))
    }
}

/// Result of sampling a goal's flow at a position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlowSample {
    /// Blended direction (unit length).
    Dir(Vec2),
    /// Standing in the goal.
    Arrived,
    /// No flow here yet: requested.
    Missing,
}

/// Bilinear sample of goal `g`'s flow at `p` (DESIGN §9 sampling). Pushes a
/// usage record, or a request when a tile is missing or stale, into `rec`
/// (deduplicated against the last record, since agents are in Morton order).
pub fn sample_flow(fs: &FlowSystem, map: &Map, g: u8, p: Vec2, rec: &mut Vec<u64>) -> FlowSample {
    let (fx, fy) = map.nav.to_cell_f(p.x, p.y);
    let (fx, fy) = (fx - 0.5, fy - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (wx, wy) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let mut acc = Vec2::ZERO;
    let mut any = false;
    let mut last_tile = u32::MAX;
    let mut last_slot = None;
    let (own_x, own_y) = ((fx + 0.5).floor() as i32, (fy + 0.5).floor() as i32);
    let mut own_arrived = false;
    let push = |rec: &mut Vec<u64>, r: u64| {
        if rec.last() != Some(&r) && rec.len() < rec.capacity() {
            rec.push(r);
        }
    };
    for (dx, dy, w) in [
        (0, 0, (1.0 - wx) * (1.0 - wy)),
        (1, 0, wx * (1.0 - wy)),
        (0, 1, (1.0 - wx) * wy),
        (1, 1, wx * wy),
    ] {
        let (cx, cy) = (x0 + dx, y0 + dy);
        if cx < 0 || cy < 0 || cx >= map.nav.dim as i32 || cy >= map.nav.dim as i32 {
            continue;
        }
        let (cx, cy) = (cx as u32, cy as u32);
        let t = map.tile_index(cx >> 4, cy >> 4);
        if t != last_tile {
            last_tile = t;
            last_slot = match fs.lookup(map, g, t) {
                Lookup::Ready(s) => {
                    push(rec, record(g, t, false));
                    Some(s)
                }
                Lookup::Stale(s) => {
                    push(rec, record(g, t, true));
                    Some(s)
                }
                Lookup::Missing => {
                    if map.tiles[t as usize] != BLOCKED {
                        push(rec, record(g, t, true));
                    }
                    None
                }
            };
        }
        let Some(s) = last_slot else { continue };
        let local = ((cy & 15) * 16 + (cx & 15)) as usize;
        match decode_dir(fs.dir(s, local)) {
            Some(d) => {
                acc += d * w;
                any = true;
            }
            None => {
                if cx as i32 == own_x && cy as i32 == own_y && map.walkable(cx, cy) {
                    own_arrived = true;
                }
            }
        }
    }
    if own_arrived
        && fs.goals[g as usize].targets.iter().any(|&q| {
            (q - p).len2() <= {
                let r = fs.goals[g as usize].radius;
                r * r * 1.2
            }
        })
    {
        return FlowSample::Arrived;
    }
    if any {
        let d = acc.normalize_or_zero();
        if d != Vec2::ZERO {
            return FlowSample::Dir(d);
        }
    }
    match coarse_dir(fs, map, g, p) {
        Some(d) => FlowSample::Dir(d),
        None => FlowSample::Missing,
    }
}

/// The way to goal `g` from `p` while its flow tile is still being built,
/// from the tile costs: straight at the tile's anchor on open ground,
/// otherwise across the border into the cheapest neighbouring tile. `None`
/// when the tile has no cost yet.
fn coarse_dir(fs: &FlowSystem, map: &Map, g: u8, p: Vec2) -> Option<Vec2> {
    let (cx, cy) = map.cell_of(p)?;
    let c = map.comp_of(cx, cy)?;
    let t = map.tile_index(cx >> 4, cy >> 4);
    let own = fs.tile_cost(map, g, t, c);
    if own == INF {
        return None;
    }
    if map.tiles[t as usize] == OPEN
        && let Some((a, _)) = fs.anchor_point(map, g, fs.tile_anchor(map, g, t, c))
    {
        let d = (a - p).normalize_or_zero();
        if d != Vec2::ZERO {
            return Some(d);
        }
    }
    let mut best = (own, Vec2::ZERO);
    for (dir, step) in [
        Vec2::new(1.0, 0.0),
        Vec2::new(-1.0, 0.0),
        Vec2::new(0.0, 1.0),
        Vec2::new(0.0, -1.0),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(nt) = map.neighbour_tile(t, dir) else {
            continue;
        };
        let row = (map.tile_links(t, dir) >> (c * 4)) & 0xf;
        for b in 0..4u8 {
            if row & (1 << b) != 0 {
                let cost = fs.tile_cost(map, g, nt, b);
                if cost < best.0 {
                    best = (cost, step);
                }
            }
        }
    }
    (best.1 != Vec2::ZERO).then_some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapdef::{MapDef, Polygon, SolidKind};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon {
        Polygon {
            kind: SolidKind::Building,
            points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        }
    }

    /// Follow the flow from `start` and return where we end up.
    fn walk(
        fs: &mut FlowSystem,
        map: &Map,
        fields: &Fields,
        g: u8,
        start: Vec2,
        ticks: u32,
    ) -> (Vec2, bool) {
        let w = walk_path(fs, map, fields, g, start, ticks, 0.0);
        (w.end, w.arrived)
    }

    struct Walk {
        end: Vec2,
        arrived: bool,
        /// Distance walked (m).
        length: f32,
        /// Every point visited, one per step.
        path: Vec<Vec2>,
    }

    /// Follow the flow in 0.1 m steps, sliding along walls like a body
    /// would, with tiles swapped in after `latency` ticks.
    fn walk_path(
        fs: &mut FlowSystem,
        map: &Map,
        fields: &Fields,
        g: u8,
        start: Vec2,
        ticks: u32,
        latency: f32,
    ) -> Walk {
        let tun = Tunables {
            flow_latency: latency,
            ..Default::default()
        };
        let mut p = start;
        let mut rec = Vec::with_capacity(512);
        let mut w = Walk {
            end: start,
            arrived: false,
            length: 0.0,
            path: vec![start],
        };
        for tick in 0..ticks {
            rec.clear();
            let s = sample_flow(fs, map, g, p, &mut rec);
            fs.records[0].extend_from_slice(&rec);
            fs.update(map, fields, &tun, tick);
            match s {
                FlowSample::Dir(d) => {
                    let step = d * 0.1;
                    for np in [
                        p + step,
                        p + Vec2::new(step.x, 0.0),
                        p + Vec2::new(0.0, step.y),
                    ] {
                        if np != p && map.walkable_at(np) {
                            w.length += (np - p).len();
                            p = np;
                            w.path.push(p);
                            break;
                        }
                    }
                }
                FlowSample::Arrived => {
                    w.arrived = true;
                    break;
                }
                FlowSample::Missing => {}
            }
        }
        w.end = p;
        w
    }

    fn open_map(size: f32, solids: Vec<Polygon>) -> (Map, Fields, FlowSystem) {
        let def = MapDef {
            name: "test".into(),
            width: size,
            height: size,
            solids,
            ..Default::default()
        };
        let map = Map::build(&def, 0.45).unwrap();
        let fields = Fields::new(map.size, 4);
        let fs = FlowSystem::new(&map, 4096, 256, 1);
        (map, fields, fs)
    }

    #[test]
    fn open_ground_routes_are_straight() {
        // Any-angle: across open ground people walk the straight line, not
        // an 8-direction or Manhattan staircase, at every angle.
        for (k, start) in [
            Vec2::new(-100.0, -60.0),
            Vec2::new(-90.0, 20.0),
            Vec2::new(30.0, -110.0),
            Vec2::new(-20.0, 95.0),
        ]
        .into_iter()
        .enumerate()
        {
            let (map, fields, mut fs) = open_map(512.0, vec![]);
            let target = Vec2::new(100.0, 60.0) * if k == 3 { -1.0 } else { 1.0 };
            let r = 3.0;
            fs.set_targets(0, &[target], r, 0);
            let w = walk_path(&mut fs, &map, &fields, 0, start, 6000, 0.0);
            assert!(w.arrived, "stuck at {:?}", w.end);
            let ideal = (target - start).len() - r;
            let ratio = w.length / ideal;
            eprintln!(
                "open route {k}: walked {:.1} m, ideal {ideal:.1} m, ratio {ratio:.4}",
                w.length
            );
            // And it stays near the line the whole way.
            let dir = (target - start).normalize_or_zero();
            let off = w
                .path
                .iter()
                .map(|&q| (q - start).cross(dir).abs())
                .fold(0.0f32, f32::max);
            eprintln!("open route {k}: largest offset from the line {off:.2} m");
            assert!(ratio < 1.01, "route {k}: {ratio}");
            assert!(off < 1.0, "route {k} strays {off} m");
        }
    }

    #[test]
    fn city_routes_arrive_without_detours() {
        // The test district: 64 m blocks, 11.5 m streets, a square in the
        // middle. Walkers cross it from every side to the square.
        let def = crate::testkit::district(512.0, 64.0);
        let map = Map::build(&def, 0.45).unwrap();
        let fields = Fields::new(map.size, 4);
        let target = Vec2::new(0.0, 0.0);
        let mut total = (0.0, 0.0);
        for start in [
            Vec2::new(-224.0, -160.0),
            Vec2::new(224.0, -96.0),
            Vec2::new(-96.0, 224.0),
            Vec2::new(160.0, 160.0),
            Vec2::new(-224.0, 32.0),
            Vec2::new(32.0, -224.0),
        ] {
            let start = map.nearest_open(start, 20.0).unwrap();
            let mut fs = FlowSystem::new(&map, 4096, 256, 1);
            fs.set_targets(0, &[target], 6.0, 0);
            let w = walk_path(&mut fs, &map, &fields, 0, start, 20_000, 0.0);
            assert!(w.arrived, "from {start:?} stuck at {:?}", w.end);
            let ratio = w.length / ((target - start).len() - 6.0);
            eprintln!(
                "city route from {start:?}: walked {:.1} m, {ratio:.3} × straight",
                w.length
            );
            total.0 += w.length;
            total.1 += (target - start).len() - 6.0;
        }
        let ratio = total.0 / total.1;
        eprintln!("city routes: {ratio:.3} × straight overall");
        assert!(ratio < 1.18, "{ratio}");
    }

    /// `cargo test -p riot_sim --lib dump_anchors -- --ignored --nocapture`
    /// prints the tile anchors around a goal: the cost error in route units
    /// for tiles heading straight for the goal, else the offset (in tiles)
    /// of the tile they turn at.
    #[test]
    #[ignore]
    fn dump_anchors() {
        let (map, fields, mut fs) = open_map(512.0, vec![]);
        let target = Vec2::new(100.0, 60.0);
        fs.set_targets(0, &[target], 3.0, 0);
        let _ = walk_path(
            &mut fs,
            &map,
            &fields,
            0,
            Vec2::new(-100.0, -60.0),
            3000,
            0.0,
        );
        for ty in 20..40u32 {
            let mut line = String::new();
            for tx in 18..47u32 {
                let t = map.tile_index(tx, ty);
                let a = fs.tile_anchor(&map, 0, t, 0);
                let c = fs.tile_cost(&map, 0, t, 0);
                let exact = ((anchor_dist(target, 3.0, tile_centre(&map, t))) * 20.0) as i64;
                line.push_str(&if a == NONE {
                    "   .  ".to_string()
                } else if a & ANCHOR_TARGET != 0 {
                    format!("{:+5} ", c as i64 - exact)
                } else {
                    let (ax, ay) = (
                        ((a >> 2) & (map.tdim - 1)) as i32,
                        ((a >> 2) >> map.tshift) as i32,
                    );
                    format!("{:>2},{:<2} ", ax - tx as i32, ay - ty as i32)
                });
            }
            eprintln!("{line}");
        }
    }

    #[test]
    fn routes_bend_at_the_corner() {
        // A building between start and goal: the shortest way passes its
        // nearer (south) corners, so walkers should cut close to them.
        let (map, fields, mut fs) = open_map(512.0, vec![rect(-10.0, -50.0, 10.0, 20.0)]);
        let (start, target, r) = (Vec2::new(-60.0, 0.0), Vec2::new(60.0, 0.0), 3.0);
        fs.set_targets(0, &[target], r, 0);
        let w = walk_path(&mut fs, &map, &fields, 0, start, 6000, 0.0);
        assert!(w.arrived, "stuck at {:?}", w.end);
        // Geodesic around the corners (-10, 20) and (10, 20), ending at the
        // goal disc.
        let c0 = Vec2::new(-10.0, 20.0);
        let c1 = Vec2::new(10.0, 20.0);
        let ideal = (c0 - start).len() + 20.0 + (target - c1).len() - r;
        let ratio = w.length / ideal;
        let widest = w.path.iter().map(|q| q.y).fold(0.0f32, f32::max);
        eprintln!(
            "corner route: walked {:.1} m, geodesic {ideal:.1} m, ratio {ratio:.4}, widest y {widest:.1}",
            w.length
        );
        assert!(ratio < 1.03, "{ratio}");
        assert!(
            widest < 26.0,
            "swings {widest} m wide of a corner at y = 20"
        );
    }

    #[test]
    fn pending_tiles_do_not_walk_into_walls() {
        // While flow tiles are pending (latency 10), the walker follows the
        // coarse route instead of a straight line into the building.
        let (map, fields, mut fs) = open_map(512.0, vec![rect(-10.0, -50.0, 10.0, 20.0)]);
        let (start, target) = (Vec2::new(-30.0, 0.0), Vec2::new(60.0, 0.0));
        fs.set_targets(0, &[target], 3.0, 0);
        let w = walk_path(&mut fs, &map, &fields, 0, start, 6000, 10.0);
        assert!(w.arrived, "stuck at {:?}", w.end);
        let ideal =
            (Vec2::new(-10.0, 20.0) - start).len() + 20.0 + (target - Vec2::new(10.0, 20.0)).len()
                - 3.0;
        eprintln!(
            "pending route: walked {:.1} m, geodesic {ideal:.1} m, ratio {:.4}",
            w.length,
            w.length / ideal
        );
        assert!(w.length / ideal < 1.05);
    }

    #[test]
    fn routes_around_a_wall_across_regions() {
        // 512 m map, a long wall at x = 0 with a gap only near the bottom.
        let def = MapDef {
            name: "wall".into(),
            width: 512.0,
            height: 512.0,
            solids: vec![
                rect(-2.0, -256.0, 2.0, 200.0),
                rect(60.0, -60.0, 80.0, 60.0),
            ],
            ..Default::default()
        };
        let map = Map::build(&def, 0.45).unwrap();
        let fields = Fields::new(map.size, 4);
        let mut fs = FlowSystem::new(&map, 4096, 256, 1);
        let target = Vec2::new(120.0, 0.0);
        fs.set_targets(0, &[target], 3.0, 0);
        let (end, arrived) = walk(&mut fs, &map, &fields, 0, Vec2::new(-100.0, 0.0), 12_000);
        assert!(arrived, "stuck at {end:?}");
        assert!((end - target).len() < 6.0);
        assert!(fs.stats.built > 0);
    }

    #[test]
    fn retarget_discards_old_tiles() {
        let def = MapDef {
            name: "open".into(),
            width: 256.0,
            height: 256.0,
            ..Default::default()
        };
        let map = Map::build(&def, 0.45).unwrap();
        let fields = Fields::new(map.size, 4);
        let mut fs = FlowSystem::new(&map, 256, 64, 1);
        fs.set_targets(3, &[Vec2::new(50.0, 50.0)], 3.0, 0);
        let (_, a) = walk(&mut fs, &map, &fields, 3, Vec2::new(0.0, 0.0), 4000);
        assert!(a);
        fs.set_targets(3, &[Vec2::new(-50.0, -50.0)], 3.0, 0);
        let (end, a) = walk(&mut fs, &map, &fields, 3, Vec2::new(40.0, 40.0), 8000);
        assert!(a, "{end:?}");
        assert!((end - Vec2::new(-50.0, -50.0)).len() < 6.0);
    }
}
