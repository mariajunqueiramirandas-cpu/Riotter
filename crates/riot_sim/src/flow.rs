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
//!    corridor gives each tile component a cost. Dynamic costs (density, gas)
//!    enter here and are refreshed about once a second.
//! 4. **Flow tiles.** When people enter a tile, its 16 × 16 direction field is
//!    built by a cell-level Dijkstra seeded from the downhill neighbour tiles,
//!    and swapped in exactly `flow_latency` ticks after it was requested, so
//!    determinism never depends on how fast a build ran.
//!
//! A 6 km route never searches the whole map, and map size costs nothing.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rayon::prelude::*;
use riot_core::Vec2;
use riot_core::math::HEADING_DIRS;

use crate::fields::{Fields, Layer};
use crate::map::{BLOCKED, Map, REGION, TILE, TILE_M};
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
/// Buckets in Dial's queue: more than the largest tile step cost.
const DIAL_BUCKETS: usize = 1024;
/// Largest tile step cost (`TILE_STEP` plus the largest penalty).
const MAX_STEP: u32 = TILE_STEP * 4;
/// Most flow tiles built per tick; further requests wait a tick.
const MAX_JOBS_PER_TICK: usize = 96;
/// Most corridor recomputes per tick.
const MAX_RECOMPUTES_PER_TICK: usize = 2;

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
            page_prev: vec![INF; pages * PAGE_COST],
            page_step: vec![TILE_STEP; pages * PAGE_TILES],
            page_epoch: vec![0; pages * PAGE_TILES],
            page_region: vec![NONE; pages],
            dial: (0..DIAL_BUCKETS).map(|_| Vec::with_capacity(256)).collect(),
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
            self.page_flow[p as usize * PAGE_TILES..(p as usize + 1) * PAGE_TILES].fill(NONE);
        }
    }

    /// Tile components touching the goal's target discs.
    fn for_each_source(map: &Map, goal: &Goal, mut f: impl FnMut(u32, u8)) {
        for &t in &goal.targets {
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
                                f(tile, c);
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
            Self::for_each_source(map, goal, |t, c| {
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

    /// Dijkstra over tile components restricted to the corridor, with
    /// Dial's bucket queue over integer costs (DESIGN §9). Tiles whose costs
    /// changed noticeably, and their neighbours, get the new epoch so their
    /// flow tiles are rebuilt; everything else keeps its flow.
    fn tile_dijkstra(&mut self, map: &Map, fields: &Fields, tun: &Tunables, g: u8) {
        let gi = g as usize;
        let epoch = self.goals[gi].version;
        for &p in &self.goals[gi].page_list {
            let (a, b) = (p as usize * PAGE_COST, (p as usize + 1) * PAGE_COST);
            self.page_prev[a..b].copy_from_slice(&self.page_cost[a..b]);
            self.page_cost[a..b].fill(INF);
            let r = self.page_region[p as usize];
            let (rx, ry) = (r % map.rdim, r / map.rdim);
            for local in 0..PAGE_TILES as u32 {
                let t = map.tile_index(rx * REGION + local % REGION, ry * REGION + local / REGION);
                self.page_step[p as usize * PAGE_TILES + local as usize] =
                    tile_step_cost(map, fields, tun, t);
            }
        }
        let goal = &self.goals[gi];
        let page_cost = &mut self.page_cost;
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
        Self::for_each_source(map, goal, |t, c| {
            if let Some(i) = idx(t, c)
                && page_cost[i] != 0
            {
                page_cost[i] = 0;
                dial[0].push((t << 2) | c as u32);
                pending += 1;
            }
        });
        let tdim = map.tdim;
        let nb = dial.len();
        let mut cur = 0u32;
        while pending > 0 {
            let b = cur as usize % nb;
            let items = std::mem::take(&mut dial[b]);
            for &key in &items {
                pending -= 1;
                let (t, c) = (key >> 2, (key & 3) as u8);
                let Some(i) = idx(t, c) else { continue };
                if page_cost[i] != cur {
                    continue;
                }
                let (tx, ty) = (t & (tdim - 1), t >> map.tshift);
                for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                    let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= tdim as i32 || ny >= tdim as i32 {
                        continue;
                    }
                    let nt = map.tile_index(nx as u32, ny as u32);
                    let Some((np, nl)) = page_of(nt) else {
                        continue;
                    };
                    let step = page_step[np * PAGE_TILES + nl].min(MAX_STEP);
                    let mut done = [false; 4];
                    for_each_link(map, t, c, nt, dx, dy, |nc| {
                        if std::mem::replace(&mut done[nc as usize], true) {
                            return;
                        }
                        let j = np * PAGE_COST + nl * 4 + nc as usize;
                        let new = cur + step;
                        if new < page_cost[j] {
                            page_cost[j] = new;
                            dial[new as usize % nb].push((nt << 2) | nc as u32);
                            pending += 1;
                        }
                    });
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
                for (dx, dy) in [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)] {
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

/// Cost of entering tile `t`: its length plus crowd and gas penalties.
fn tile_step_cost(map: &Map, fields: &Fields, tun: &Tunables, t: u32) -> u32 {
    let (tx, ty) = (t & (map.tdim - 1), t >> map.tshift);
    let cx = tx as f32 * TILE_M - map.half + TILE_M * 0.5;
    let cy = ty as f32 * TILE_M - map.half + TILE_M * 0.5;
    let dens = fields.sample(Layer::Dens, cx, cy);
    let gas = fields.sample(Layer::Gas, cx, cy);
    let pen = (tun.flow_density_cost * dens + tun.flow_gas_cost * gas).clamp(0.0, 24.0);
    TILE_STEP + (TILE_STEP as f32 * pen * 0.1) as u32
}

/// Call `f(neighbour_comp)` for every component of tile `nt` (offset
/// `dx, dy` from `t`) that component `c` of `t` connects to.
#[inline]
fn for_each_link(map: &Map, t: u32, c: u8, _nt: u32, dx: i32, dy: i32, mut f: impl FnMut(u8)) {
    let dir = match (dx, dy) {
        (1, _) => 0,
        (-1, _) => 1,
        (_, 1) => 2,
        _ => 3,
    };
    let row = (map.tile_links(t, dir) >> (c * 4)) & 0xf;
    for b in 0..4u8 {
        if row & (1 << b) != 0 {
            f(b);
        }
    }
}

/// Output and scratch of one flow tile build, reused every tick.
struct JobBuf {
    dirs: [u8; 256],
    heap: SmallHeap,
}

/// A small fixed-capacity binary min-heap, so tile builds never allocate.
struct SmallHeap {
    items: [(u32, u16); 2304],
    len: usize,
}

impl SmallHeap {
    fn new() -> Self {
        SmallHeap {
            items: [(0, 0); 2304],
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

const NBR8: [(i32, i32, u32); 8] = [
    (1, 0, 10),
    (1, 1, 14),
    (0, 1, 10),
    (-1, 1, 14),
    (-1, 0, 10),
    (-1, -1, 14),
    (0, -1, 10),
    (1, -1, 14),
];

/// Build the direction field of tile `t` for goal `g` (DESIGN §9): a
/// cell-level Dijkstra seeded from the goal's cells and from the border with
/// downhill neighbour tiles. Pure function of its inputs.
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
    let (cx0, cy0) = (tx * TILE, ty * TILE);
    let mut comp = [u8::MAX; 256];
    for i in 0..256u32 {
        if let Some(c) = map.comp_of(cx0 + (i & 15), cy0 + (i >> 4)) {
            comp[i as usize] = c;
        }
    }
    // Per-cell step factor from crowding and gas (sampled per 2 m field cell).
    let mut factor = [10u32; 256];
    let fx0 = tx as f32 * TILE_M - map.half;
    let fy0 = ty as f32 * TILE_M - map.half;
    for qy in 0..4 {
        for qx in 0..4 {
            let p = Vec2::new(fx0 + qx as f32 * 2.0 + 1.0, fy0 + qy as f32 * 2.0 + 1.0);
            let dens = fields.value_at(Layer::Dens, p);
            let gas = fields.value_at(Layer::Gas, p);
            let pen = (tun.flow_density_cost * dens + tun.flow_gas_cost * gas).clamp(0.0, 24.0);
            let f = 10 + (pen * 10.0) as u32;
            for ly in 0..4 {
                for lx in 0..4 {
                    factor[((qy * 4 + ly) * 16 + qx * 4 + lx) as usize] = f;
                }
            }
        }
    }
    let mut integ = [INF; 256];
    // border_dir[i]: heading + 1 toward the downhill tile that seeded cell i.
    let mut border_dir = [0u8; 256];
    let mut goal_cell = [false; 256];
    let r2 = goal.radius * goal.radius;
    for i in 0..256usize {
        if comp[i] == u8::MAX {
            continue;
        }
        let (wx, wy) = map.nav.centre(cx0 + (i as u32 & 15), cy0 + (i as u32 >> 4));
        let p = Vec2::new(wx, wy);
        if goal.targets.iter().any(|&q| (q - p).len2() <= r2) {
            integ[i] = 0;
            goal_cell[i] = true;
            heap.push(0, i as u16);
        }
    }
    // Seeds from downhill neighbours.
    for (dx, dy, heading) in [(1i32, 0i32, 0u8), (-1, 0, 128), (0, 1, 64), (0, -1, 192)] {
        let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
        if nx < 0 || ny < 0 || nx >= map.tdim as i32 || ny >= map.tdim as i32 {
            continue;
        }
        let nt = map.tile_index(nx as u32, ny as u32);
        for k in 0..TILE {
            let (lx, ly) = match (dx, dy) {
                (1, _) => (15, k),
                (-1, _) => (0, k),
                (_, 1) => (k, 15),
                _ => (k, 0),
            };
            let i = (ly * 16 + lx) as usize;
            if comp[i] == u8::MAX || goal_cell[i] {
                continue;
            }
            let (ox, oy) = ((cx0 + lx) as i32 + dx, (cy0 + ly) as i32 + dy);
            let Some(nc) = map.comp_of(ox as u32, oy as u32) else {
                continue;
            };
            let own = fs.tile_cost(map, g, t, comp[i]);
            let theirs = fs.tile_cost(map, g, nt, nc);
            if theirs == INF || theirs >= own {
                continue;
            }
            let v = theirs.saturating_add(TILE_STEP / 2);
            if v < integ[i] {
                integ[i] = v;
                border_dir[i] = heading.wrapping_add(1).max(1);
                heap.push(v, i as u16);
            }
        }
    }
    while let Some((c, i)) = heap.pop() {
        let i = i as usize;
        if c > integ[i] {
            continue;
        }
        let (x, y) = ((i & 15) as i32, (i >> 4) as i32);
        for &(dx, dy, base) in &NBR8 {
            let (nx, ny) = (x + dx, y + dy);
            if !(0..16).contains(&nx) || !(0..16).contains(&ny) {
                continue;
            }
            let n = (ny * 16 + nx) as usize;
            if comp[n] == u8::MAX {
                continue;
            }
            if dx != 0
                && dy != 0
                && (comp[(y * 16 + nx) as usize] == u8::MAX
                    || comp[(ny * 16 + x) as usize] == u8::MAX)
            {
                continue;
            }
            let nc = c.saturating_add(base * factor[n] / 10);
            if nc < integ[n] {
                integ[n] = nc;
                heap.push(nc, n as u16);
            }
        }
    }
    // Directions: toward the lowest neighbour, or across the seeding border.
    for i in 0..256usize {
        if comp[i] == u8::MAX || integ[i] == INF || goal_cell[i] {
            continue;
        }
        let (x, y) = ((i & 15) as i32, (i >> 4) as i32);
        let mut best = integ[i];
        let mut dir = border_dir[i];
        for (k, &(dx, dy, _)) in NBR8.iter().enumerate() {
            let (nx, ny) = (x + dx, y + dy);
            if !(0..16).contains(&nx) || !(0..16).contains(&ny) {
                continue;
            }
            let n = (ny * 16 + nx) as usize;
            if comp[n] == u8::MAX {
                continue;
            }
            if dx != 0
                && dy != 0
                && (comp[(y * 16 + nx) as usize] == u8::MAX
                    || comp[(ny * 16 + x) as usize] == u8::MAX)
            {
                continue;
            }
            if integ[n] < best {
                best = integ[n];
                dir = (k as u8 * 32).wrapping_add(1);
            }
        }
        out[i] = dir;
    }
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
    FlowSample::Missing
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
        let tun = Tunables {
            flow_latency: 0.0,
            ..Default::default()
        };
        let mut p = start;
        let mut rec = Vec::with_capacity(512);
        let mut arrived = false;
        for tick in 0..ticks {
            rec.clear();
            let s = sample_flow(fs, map, g, p, &mut rec);
            fs.records[0].extend_from_slice(&rec);
            fs.update(map, fields, &tun, tick);
            match s {
                FlowSample::Dir(d) => {
                    let np = p + d * 0.1;
                    if map.walkable_at(np) {
                        p = np;
                    }
                }
                FlowSample::Arrived => {
                    arrived = true;
                    break;
                }
                FlowSample::Missing => {}
            }
        }
        (p, arrived)
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
