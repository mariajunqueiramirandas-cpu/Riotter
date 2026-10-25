//! Phase 3: fields (DESIGN §8). Things a whole crowd shares — fear spreading
//! through a square, a drifting gas cloud, how threatening an area feels —
//! live in 2 m grids instead of agent-to-agent talk, turning O(N·k) work
//! into O(N + cells).
//!
//! Storage is sparse: layers live in 64 × 64-cell chunks (128 m × 128 m) from
//! a pool allocated at riot start. A chunk exists only while it holds people
//! or an emitter, or its values have not decayed yet. Cost and memory follow
//! the area where something is happening, not the size of the map.

#![deny(unsafe_code)]

use riot_core::Vec2;
use riot_core::grid::{GridGeom, demorton2};
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::{dither_u8, widen_u8};
use riot_core::rng::{Stream, roll, unit};
use riot_team::prelude::*;

use riot_defs::archetype::{ARCHETYPES, Archetype, ArchetypeParams};
use riot_defs::state::{Action, aux, emo, flag, health, stat};
use riot_defs::tunables::Tunables;
use riot_map::Map;
use riot_people::People;
use riot_spatial::{FIELD_CELL, Spatial};

/// Cells per chunk side.
pub const CC: usize = 64;
/// Cells per chunk.
pub const CL: usize = CC * CC;
pub const CHUNK_M: f32 = CC as f32 * FIELD_CELL;
pub const NO_SLOT: u16 = u16::MAX;
pub const NO_CHUNK: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Layer {
    /// People per cell (u16 count stored as f32).
    Dens = 0,
    /// Fear of the local crowd, smoothed in space and time (a sum: divide
    /// by `Weight` for the mean).
    Fear = 1,
    Anger = 2,
    /// Officers and violence.
    Threat = 3,
    /// Tear gas.
    Gas = 4,
    Smoke = 5,
    /// Organizers chanting.
    Chant = 6,
    /// Injured or fallen people per cell.
    Injured = 7,
    /// Cameras that can see this cell.
    Camera = 8,
    /// Fire intensity, 0..1.
    Fire = 9,
    /// Remaining fuel.
    Fuel = 10,
    /// Walkable fraction of the cell (static, set on activation).
    Open = 11,
    /// Civilians per cell, smoothed like `Fear` and `Anger`: those two hold
    /// smoothed sums, so `Fear / Weight` is the local mean (DESIGN §8).
    Weight = 12,
    /// Police barriers and roadblocks, 0..1: routes price it like a wall
    /// they could still push through. Rewritten by the police commander.
    Block = 13,
}

pub const LAYERS: usize = 14;
/// Layers one fused blur pass can carry.
pub const FUSED: usize = 3;

/// A layer's bit in the per-slot layer masks.
#[inline(always)]
const fn bit(l: Layer) -> u16 {
    1 << l as u16
}

/// A layer (or a run of whole rows) as rows of `CC` cells: fixed-length
/// rows let the row loops vectorize without bounds checks.
#[inline(always)]
fn rows(v: &[f32]) -> &[[f32; CC]] {
    v.as_chunks::<CC>().0
}

#[inline(always)]
fn rows_mut(v: &mut [f32]) -> &mut [[f32; CC]] {
    v.as_chunks_mut::<CC>().0
}

/// Some value is not +0.0.
#[inline]
fn any_bits(v: &[f32]) -> bool {
    v.iter().fold(0u32, |b, &x| b | x.to_bits()) != 0
}

pub struct Fields {
    pub geom: GridGeom,
    pub cdim: u32,
    pub pool: usize,
    /// Chunk index `cy * cdim + cx` → pool slot.
    pub table: Vec<u16>,
    pub slot_chunk: Vec<u32>,
    pub free: Vec<u16>,
    /// `pool × LAYERS × CL` floats: slot-major, then layer, then row-major cell.
    pub data: Vec<f32>,
    pub tmp: Vec<f32>,
    /// Message layer (DESIGN §13), `fidelity << 8 | id`, and its scratch.
    pub msg: Vec<u16>,
    pub msg_tmp: Vec<u16>,
    pub idle: Vec<u16>,
    pub people_in: Vec<u32>,
    pub pinned: Vec<bool>,
    /// Slots whose `Block` layer holds anything: routes look for barriers
    /// only there.
    pub blocked: Vec<bool>,
    /// Anything is blocked anywhere.
    pub block_live: bool,
    pub slot_runs: Vec<[u32; 2]>,
    /// Cells of each slot that hold people this tick, so the next scatter
    /// clears only those instead of whole layers.
    written: Vec<u16>,
    written_n: Vec<u16>,
    /// Per slot, one bit per layer ([`bit`]): clear only while every value
    /// of that layer in the slot is +0.0 (all bits zero). Everything that
    /// writes a layer sets its bit (`stamp`, `raise_at`, `layer_mut`, the
    /// scatter); the passes that rewrite whole layers (`filter`, `blur`)
    /// recompute it. Work on a clear layer that could only reproduce +0.0
    /// is skipped. Write `data` only through these.
    live: Vec<u16>,
    /// Chunks to activate at the start of the next update.
    pub want: Vec<u32>,
    pub active: u32,
    pub overflow: u32,
}

impl Fields {
    pub fn new(world_size: f32, pool: usize) -> Self {
        let geom = GridGeom::new(world_size, FIELD_CELL);
        let cdim = geom.dim / CC as u32;
        let pool = pool.min((cdim * cdim) as usize).max(1);
        Fields {
            geom,
            cdim,
            pool,
            table: vec![NO_SLOT; (cdim * cdim) as usize],
            slot_chunk: vec![NO_CHUNK; pool],
            free: (0..pool as u16).rev().collect(),
            data: vec![0.0; pool * LAYERS * CL],
            tmp: vec![0.0; pool * FUSED * CL],
            msg: vec![0; pool * CL],
            msg_tmp: vec![0; pool * CL],
            idle: vec![0; pool],
            people_in: vec![0; pool],
            pinned: vec![false; pool],
            blocked: vec![false; pool],
            block_live: false,
            slot_runs: vec![[0; 2]; pool],
            written: vec![0; pool * CL],
            written_n: vec![0; pool],
            live: vec![0; pool],
            want: Vec::with_capacity(pool * 4),
            active: 0,
            overflow: 0,
        }
    }

    #[inline]
    pub fn layer(&self, slot: usize, l: Layer) -> &[f32] {
        let o = (slot * LAYERS + l as usize) * CL;
        &self.data[o..o + CL]
    }

    /// The layer to write: from now on it may hold anything.
    #[inline]
    pub fn layer_mut(&mut self, slot: usize, l: Layer) -> &mut [f32] {
        self.live[slot] |= bit(l);
        let o = (slot * LAYERS + l as usize) * CL;
        &mut self.data[o..o + CL]
    }

    /// Every value of layer `l` in `slot` is +0.0 (`false`: it may not be).
    #[inline]
    pub fn is_zero(&self, slot: usize, l: Layer) -> bool {
        self.live[slot] & bit(l) == 0
    }

    /// Look at a layer again, so [`Fields::is_zero`] knows if it is +0.0.
    pub fn refresh_zero(&mut self, slot: usize, l: Layer) {
        if self.layer(slot, l).iter().all(|v| v.to_bits() == 0) {
            self.live[slot] &= !bit(l);
        }
    }

    /// Set layer `l` to 0.0 in every active chunk. Chunks whose layer is
    /// already +0.0 throughout are left alone.
    pub fn clear_layer(&mut self, l: Layer) {
        for slot in 0..self.pool {
            if self.slot_chunk[slot] != NO_CHUNK && !self.is_zero(slot, l) {
                self.layer_mut(slot, l).fill(0.0);
                self.live[slot] &= !bit(l);
            }
        }
    }

    #[inline]
    pub fn chunk_index(&self, cx: u32, cy: u32) -> u32 {
        cy * self.cdim + cx
    }

    #[inline]
    pub fn slot_of_chunk(&self, cx: i32, cy: i32) -> Option<usize> {
        if cx < 0 || cy < 0 || cx >= self.cdim as i32 || cy >= self.cdim as i32 {
            return None;
        }
        let s = self.table[(cy as u32 * self.cdim + cx as u32) as usize];
        if s == NO_SLOT { None } else { Some(s as usize) }
    }

    /// Chunk containing a point.
    #[inline]
    pub fn chunk_at(&self, p: Vec2) -> (u32, u32) {
        let (cx, cy) = self.geom.cell_of(p.x, p.y);
        (cx / CC as u32, cy / CC as u32)
    }

    /// Ask for the chunk under `p` to be active from the next update on.
    pub fn request(&mut self, p: Vec2) {
        let (cx, cy) = self.chunk_at(p);
        let c = self.chunk_index(cx, cy);
        if self.table[c as usize] == NO_SLOT
            && !self.want.contains(&c)
            && self.want.len() < self.want.capacity()
        {
            self.want.push(c);
        }
    }

    /// Activate a chunk now (sequential code only).
    pub fn activate(&mut self, c: u32, map: &Map) -> Option<usize> {
        let s = self.table[c as usize];
        if s != NO_SLOT {
            return Some(s as usize);
        }
        let Some(slot) = self.free.pop() else {
            self.overflow += 1;
            return None;
        };
        let slot = slot as usize;
        self.table[c as usize] = slot as u16;
        self.slot_chunk[slot] = c;
        self.idle[slot] = 0;
        self.written_n[slot] = 0;
        self.blocked[slot] = false;
        self.active += 1;
        let o = slot * LAYERS * CL;
        self.data[o..o + LAYERS * CL].fill(0.0);
        self.live[slot] = 0;
        self.msg[slot * CL..(slot + 1) * CL].fill(0);
        // Static walkable fraction of each 2 m cell (4 × 4 nav cells).
        let (cx, cy) = (c % self.cdim, c / self.cdim);
        let open = self.layer_mut(slot, Layer::Open);
        for ly in 0..CC as u32 {
            for lx in 0..CC as u32 {
                let (nx, ny) = ((cx * CC as u32 + lx) * 4, (cy * CC as u32 + ly) * 4);
                let mut n = 0;
                for dy in 0..4 {
                    for dx in 0..4 {
                        n += map.walkable(nx + dx, ny + dy) as u32;
                    }
                }
                open[(ly * CC as u32 + lx) as usize] = n as f32 * (1.0 / 16.0);
            }
        }
        Some(slot)
    }

    fn release(&mut self, slot: usize) {
        let c = self.slot_chunk[slot];
        if c == NO_CHUNK {
            return;
        }
        self.table[c as usize] = NO_SLOT;
        self.slot_chunk[slot] = NO_CHUNK;
        self.free.push(slot as u16);
        self.active -= 1;
    }

    // -----------------------------------------------------------------------
    // Sampling

    /// Bilinear sample: values change smoothly as someone walks.
    #[inline]
    pub fn sample(&self, l: Layer, x: f32, y: f32) -> f32 {
        self.probe(x, y).get(self, l)
    }

    /// Locate the four cells around a point once, to sample several layers
    /// (and their gradients) with one chunk lookup.
    #[inline]
    pub fn probe(&self, x: f32, y: f32) -> Probe {
        let (fx, fy) = self.geom.to_cell_f(x, y);
        let (fx, fy) = (fx - 0.5, fy - 0.5);
        let (x0f, y0f) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0f, fy - y0f);
        let (x0, y0) = (x0f as i32, y0f as i32);
        let (lx, ly) = (x0 & (CC as i32 - 1), y0 & (CC as i32 - 1));
        let fast = x0 >= 0 && y0 >= 0 && lx < CC as i32 - 1 && ly < CC as i32 - 1;
        let slot = if fast {
            match self.slot_of_chunk(x0 >> 6, y0 >> 6) {
                Some(s) => s as u32,
                None => Probe::EMPTY,
            }
        } else {
            Probe::SLOW
        };
        Probe {
            slot,
            cell: (ly * CC as i32 + lx) as u32,
            x0,
            y0,
            tx,
            ty,
        }
    }
    /// Value of one field cell (0 where no chunk is active).
    #[inline]
    pub fn cell_value(&self, l: Layer, cx: i32, cy: i32) -> f32 {
        match self.slot_of_chunk(cx.div_euclid(CC as i32), cy.div_euclid(CC as i32)) {
            Some(s) => self.layer(s, l)
                [(cy.rem_euclid(CC as i32) as usize) * CC + cx.rem_euclid(CC as i32) as usize],
            None => 0.0,
        }
    }

    #[inline]
    pub fn value_at(&self, l: Layer, p: Vec2) -> f32 {
        let (fx, fy) = self.geom.to_cell_f(p.x, p.y);
        self.cell_value(l, fx.floor() as i32, fy.floor() as i32)
    }

    /// Gradient (per metre) of the bilinear interpolant at `p`.
    #[inline]
    pub fn gradient(&self, l: Layer, p: Vec2) -> Vec2 {
        self.probe(p.x, p.y).grad(self, l)
    }

    /// Add `v` (falling off linearly) to every cell within `r` of `p`,
    /// activating chunks as needed. Sequential code only.
    pub fn stamp(&mut self, l: Layer, p: Vec2, r: f32, v: f32, map: &Map) {
        let (fx, fy) = self.geom.to_cell_f(p.x, p.y);
        let rc = (r / FIELD_CELL).max(0.5);
        let (x0, x1) = ((fx - rc).floor() as i32, (fx + rc).ceil() as i32);
        let (y0, y1) = ((fy - rc).floor() as i32, (fy + rc).ceil() as i32);
        let dim = self.geom.dim as i32;
        for cy in y0.max(0)..y1.min(dim) {
            for cx in x0.max(0)..x1.min(dim) {
                let (dx, dy) = (cx as f32 + 0.5 - fx, cy as f32 + 0.5 - fy);
                let d = (dx * dx + dy * dy).sqrt();
                if d > rc {
                    continue;
                }
                let c = self.chunk_index(cx as u32 / CC as u32, cy as u32 / CC as u32);
                let Some(s) = self.activate(c, map) else {
                    continue;
                };
                let w = if rc > 0.5 { 1.0 - d / (rc + 0.5) } else { 1.0 };
                let cell = (cy as usize % CC) * CC + cx as usize % CC;
                self.layer_mut(s, l)[cell] += v * w;
            }
        }
    }

    /// Set a cell to `max(current, v)`.
    pub fn raise_at(&mut self, l: Layer, p: Vec2, v: f32, map: &Map) {
        let (cx, cy) = self.geom.cell_of(p.x, p.y);
        let c = self.chunk_index(cx / CC as u32, cy / CC as u32);
        if let Some(s) = self.activate(c, map) {
            let cell = (cy as usize % CC) * CC + cx as usize % CC;
            let x = &mut self.layer_mut(s, l)[cell];
            *x = x.max(v);
        }
    }

    // -----------------------------------------------------------------------
    // The per-tick update

    /// Activate chunks with people or pending requests, and remember each
    /// slot's run range from the field index.
    pub fn activate_for_tick(&mut self, sp: &Spatial, map: &Map) {
        self.people_in.fill(0);
        self.slot_runs.fill([0; 2]);
        let want = std::mem::take(&mut self.want);
        for &c in &want {
            self.activate(c, map);
        }
        self.want = want;
        self.want.clear();
        for cr in &sp.chunk_runs {
            let (cx, cy) = demorton2(cr.chunk);
            let c = self.chunk_index(cx, cy);
            if let Some(s) = self.activate(c, map) {
                self.slot_runs[s] = [cr.start, cr.end];
                let (a, b) = (
                    sp.runs[cr.start as usize].start,
                    sp.runs[cr.end as usize - 1].end,
                );
                self.people_in[s] = b - a;
            }
        }
    }

    /// Scatter people into their cells: a segmented reduction over the field
    /// index runs. Parallel over chunks; each cell is written by one task.
    /// On emotion ticks, `Fear`, `Anger` and `Weight` move toward this
    /// tick's per-cell sums.
    pub fn scatter(
        &mut self,
        sp: &Spatial,
        p: &People,
        tun: &Tunables,
        threat_tick: bool,
        emo_tick: bool,
    ) {
        let keep = 1.0 - tun.field_blend.clamp(0.0, 1.0);
        let gain = 1.0 - keep;
        let (t_decay, t_officer) = (tun.threat_decay, tun.threat_officer);
        // Layers this pass may leave holding something other than +0.0:
        // decay keeps +0.0 at +0.0 unless the factor is negative or NaN.
        let emo_layers = bit(Layer::Fear) | bit(Layer::Anger) | bit(Layer::Weight);
        let mut always = 0;
        if emo_tick && (0.0 * keep).to_bits() != 0 {
            always |= emo_layers;
        }
        if threat_tick && (0.0 * t_decay).to_bits() != 0 {
            always |= bit(Layer::Threat);
        }
        let slot_runs = &self.slot_runs;
        let slot_chunk = &self.slot_chunk;
        self.data
            .par_chunks_mut(LAYERS * CL)
            .zip(
                self.written
                    .par_chunks_mut(CL)
                    .zip(self.written_n.par_iter_mut()),
            )
            .zip(self.live.par_iter_mut())
            .enumerate()
            .for_each(|(slot, ((d, (written, written_n)), live))| {
                if slot_chunk[slot] == NO_CHUNK {
                    return;
                }
                let mut set = always;
                let (dens, rest) = d.split_at_mut(CL);
                let (fear, rest) = rest.split_at_mut(CL);
                let (anger, rest) = rest.split_at_mut(CL);
                let (threat, rest) = rest.split_at_mut(CL);
                let (_gas, rest) = rest.split_at_mut(CL);
                let (_smoke, rest) = rest.split_at_mut(CL);
                let (_chant, rest) = rest.split_at_mut(CL);
                let (injured, rest) = rest.split_at_mut(CL);
                let weight =
                    &mut rest[(Layer::Weight as usize - Layer::Injured as usize - 1) * CL..][..CL];
                for &c in &written[..*written_n as usize] {
                    dens[c as usize] = 0.0;
                    injured[c as usize] = 0.0;
                }
                *written_n = 0;
                if emo_tick {
                    for ((f, a), w) in fear.iter_mut().zip(anger.iter_mut()).zip(weight.iter_mut())
                    {
                        *f *= keep;
                        *a *= keep;
                        *w *= keep;
                    }
                }
                if threat_tick {
                    for t in threat.iter_mut() {
                        *t *= t_decay;
                    }
                }
                let [r0, r1] = slot_runs[slot];
                for run in &sp.runs[r0 as usize..r1 as usize] {
                    let (lx, ly) = demorton2(run.key & 0xfff);
                    let cell = ly as usize * CC + lx as usize;
                    let (mut n, mut sf, mut sa, mut off, mut inj) = (0u32, 0u32, 0u32, 0u32, 0u32);
                    let mut seen = 0u32;
                    for &i in &sp.fsort.order[run.start as usize..run.end as usize] {
                        let i = i as usize;
                        let st = p.stat[i];
                        let e = p.emo[i];
                        n += 1;
                        if Archetype::from_u8(st[stat::ARCH]).is_officer() {
                            off += 1;
                            // An ambush lying in wait shows no threat.
                            if st[stat::ACTION] != Action::Lurk as u8 {
                                seen += 1;
                            }
                        } else {
                            sf += e[emo::FEAR] as u32;
                            sa += e[emo::ANGER] as u32;
                        }
                        if st[stat::HEALTH] < health::INJURED || st[stat::FLAGS] & flag::DOWN != 0 {
                            inj += 1;
                        }
                    }
                    dens[cell] = n as f32;
                    injured[cell] = inj as f32;
                    written[*written_n as usize] = cell as u16;
                    *written_n += 1;
                    set |= bit(Layer::Dens) | bit(Layer::Injured);
                    let civ = n - off;
                    if emo_tick && civ > 0 {
                        fear[cell] += gain * sf as f32 * (1.0 / 255.0);
                        anger[cell] += gain * sa as f32 * (1.0 / 255.0);
                        weight[cell] += gain * civ as f32;
                        set |= emo_layers;
                    }
                    if threat_tick && seen > 0 {
                        threat[cell] += t_officer * seen as f32;
                        set |= bit(Layer::Threat);
                    }
                }
                *live |= set;
            });
    }

    /// Separable 5-tap filter of one layer, with a 2-cell halo from the
    /// neighbouring chunks: rows `data → tmp`, then columns `tmp → data`,
    /// times `decay`. `kx`/`ky` are kernels at offsets -2..=2. Returns the
    /// largest value left in the layer.
    pub fn filter(
        &mut self,
        l: Layer,
        kx: [f32; 5],
        ky: [f32; 5],
        decay: f32,
        mask_open: bool,
    ) -> f32 {
        let li = l as usize;
        let open_i = Layer::Open as usize;
        {
            let data = &self.data;
            let table = &self.table;
            let slot_chunk = &self.slot_chunk;
            let cdim = self.cdim;
            let pool = self.pool;
            self.tmp[..pool * CL]
                .par_chunks_mut(CL)
                .enumerate()
                .for_each(|(slot, out)| {
                    let c = slot_chunk[slot];
                    if c == NO_CHUNK {
                        return;
                    }
                    let (cx, cy) = (c % cdim, c / cdim);
                    let nb = |dx: i32| -> Option<usize> {
                        let x = cx as i32 + dx;
                        if x < 0 || x >= cdim as i32 {
                            return None;
                        }
                        let s = table[(cy * cdim + x as u32) as usize];
                        if s == NO_SLOT { None } else { Some(s as usize) }
                    };
                    let me = rows(&data[(slot * LAYERS + li) * CL..][..CL]);
                    let left = nb(-1).map(|s| rows(&data[(s * LAYERS + li) * CL..][..CL]));
                    let right = nb(1).map(|s| rows(&data[(s * LAYERS + li) * CL..][..CL]));
                    let out = rows_mut(out);
                    let mut row = [0.0f32; CC + 4];
                    for y in 0..CC {
                        row[2..CC + 2].copy_from_slice(&me[y]);
                        row[0] = left.map_or(0.0, |l| l[y][CC - 2]);
                        row[1] = left.map_or(0.0, |l| l[y][CC - 1]);
                        row[CC + 2] = right.map_or(0.0, |r| r[y][0]);
                        row[CC + 3] = right.map_or(0.0, |r| r[y][1]);
                        let o = &mut out[y];
                        for x in 0..CC {
                            o[x] = kx[0] * row[x]
                                + kx[1] * row[x + 1]
                                + kx[2] * row[x + 2]
                                + kx[3] * row[x + 3]
                                + kx[4] * row[x + 4];
                        }
                    }
                });
        }
        let tmp = &self.tmp;
        let table = &self.table;
        let slot_chunk = &self.slot_chunk;
        let cdim = self.cdim;
        self.data
            .par_chunks_mut(LAYERS * CL)
            .zip(self.live.par_iter_mut())
            .enumerate()
            .map(|(slot, (d, live))| {
                let c = slot_chunk[slot];
                if c == NO_CHUNK {
                    return 0.0f32;
                }
                let (cx, cy) = (c % cdim, c / cdim);
                let nb = |dy: i32| -> Option<usize> {
                    let y = cy as i32 + dy;
                    if y < 0 || y >= cdim as i32 {
                        return None;
                    }
                    let s = table[(y as u32 * cdim + cx) as usize];
                    if s == NO_SLOT { None } else { Some(s as usize) }
                };
                let me = rows(&tmp[slot * CL..][..CL]);
                let up = nb(-1).map(|s| rows(&tmp[s * CL..][..CL]));
                let down = nb(1).map(|s| rows(&tmp[s * CL..][..CL]));
                let zero = [0.0f32; CC];
                // Row `y` of the row pass, from the chunks above and below
                // past the edges (zeros where they are inactive).
                let row = |y: i32| -> &[f32; CC] {
                    if y < 0 {
                        up.map_or(&zero, |u| &u[(CC as i32 + y) as usize])
                    } else if y >= CC as i32 {
                        down.map_or(&zero, |dn| &dn[(y - CC as i32) as usize])
                    } else {
                        &me[y as usize]
                    }
                };
                let (out, open): (&mut [f32], &[f32]) = if li < open_i {
                    let (head, tail) = d.split_at_mut(open_i * CL);
                    (&mut head[li * CL..(li + 1) * CL], &tail[..CL])
                } else {
                    let (head, tail) = d.split_at_mut(li * CL);
                    (&mut tail[..CL], &head[open_i * CL..(open_i + 1) * CL])
                };
                let rows_out = rows_mut(out);
                for y in 0..CC as i32 {
                    let (a, b, c0, e, f) = (row(y - 2), row(y - 1), row(y), row(y + 1), row(y + 2));
                    let o = &mut rows_out[y as usize];
                    for x in 0..CC {
                        o[x] = decay
                            * (ky[0] * a[x]
                                + ky[1] * b[x]
                                + ky[2] * c0[x]
                                + ky[3] * e[x]
                                + ky[4] * f[x]);
                    }
                }
                if mask_open {
                    for (v, o) in out.iter_mut().zip(open) {
                        *v *= *o;
                    }
                }
                *live = (*live & !bit(l)) | if any_bits(out) { bit(l) } else { 0 };
                out.iter().fold(0.0f32, |m, &v| m.max(v.abs()))
            })
            .reduce(|| 0.0f32, f32::max)
    }

    /// Symmetric 3-tap blur `[a, 1-2a, a]` of up to `FUSED` layers in one
    /// pair of parallel passes (rows into scratch, then columns back), with a
    /// 1-cell halo from neighbouring chunks.
    pub fn blur(&mut self, layers: &[Layer], a: f32) {
        assert!(layers.len() <= FUSED);
        let c = 1.0 - 2.0 * a;
        let pool = self.pool;
        let cdim = self.cdim;
        {
            let (data, table, slot_chunk) = (&self.data, &self.table, &self.slot_chunk);
            self.tmp[..pool * FUSED * CL]
                .par_chunks_mut(FUSED * CL)
                .enumerate()
                .for_each(|(slot, out)| {
                    let ch = slot_chunk[slot];
                    if ch == NO_CHUNK {
                        return;
                    }
                    let (cx, cy) = (ch % cdim, ch / cdim);
                    let nb = |dx: i32| {
                        let x = cx as i32 + dx;
                        if x < 0 || x >= cdim as i32 {
                            return None;
                        }
                        let s = table[(cy * cdim + x as u32) as usize];
                        (s != NO_SLOT).then_some(s as usize)
                    };
                    let (left, right) = (nb(-1), nb(1));
                    for (j, &l) in layers.iter().enumerate() {
                        let li = l as usize;
                        let me = rows(&data[(slot * LAYERS + li) * CL..][..CL]);
                        let left = left.map(|s| rows(&data[(s * LAYERS + li) * CL..][..CL]));
                        let right = right.map(|s| rows(&data[(s * LAYERS + li) * CL..][..CL]));
                        let o = rows_mut(&mut out[j * CL..(j + 1) * CL]);
                        let mut row = [0.0f32; CC + 2];
                        for y in 0..CC {
                            row[1..CC + 1].copy_from_slice(&me[y]);
                            row[0] = left.map_or(0.0, |l| l[y][CC - 1]);
                            row[CC + 1] = right.map_or(0.0, |r| r[y][0]);
                            let o = &mut o[y];
                            for x in 0..CC {
                                o[x] = a * (row[x] + row[x + 2]) + c * row[x + 1];
                            }
                        }
                    }
                });
        }
        let (tmp, table, slot_chunk) = (&self.tmp, &self.table, &self.slot_chunk);
        self.data
            .par_chunks_mut(LAYERS * CL)
            .zip(self.live.par_iter_mut())
            .enumerate()
            .for_each(|(slot, (d, live))| {
                let ch = slot_chunk[slot];
                if ch == NO_CHUNK {
                    return;
                }
                let (cx, cy) = (ch % cdim, ch / cdim);
                let nb = |dy: i32| {
                    let y = cy as i32 + dy;
                    if y < 0 || y >= cdim as i32 {
                        return None;
                    }
                    let s = table[(y as u32 * cdim + cx) as usize];
                    (s != NO_SLOT).then_some(s as usize)
                };
                let (up, down) = (nb(-1), nb(1));
                let zero = [0.0f32; CC];
                for (j, &l) in layers.iter().enumerate() {
                    let me = rows(&tmp[(slot * FUSED + j) * CL..][..CL]);
                    let above =
                        up.map_or(&zero, |s| &rows(&tmp[(s * FUSED + j) * CL..][..CL])[CC - 1]);
                    let below =
                        down.map_or(&zero, |s| &rows(&tmp[(s * FUSED + j) * CL..][..CL])[0]);
                    let out = rows_mut(&mut d[l as usize * CL..(l as usize + 1) * CL]);
                    for y in 0..CC {
                        let r0 = if y > 0 { &me[y - 1] } else { above };
                        let r1 = &me[y];
                        let r2 = if y + 1 < CC { &me[y + 1] } else { below };
                        let o = &mut out[y];
                        for x in 0..CC {
                            o[x] = a * (r0[x] + r2[x]) + c * r1[x];
                        }
                    }
                    let out = out.as_flattened();
                    *live = (*live & !bit(l)) | if any_bits(out) { bit(l) } else { 0 };
                }
            });
    }

    /// Release chunks with no people, no emitters and nothing left; ask for
    /// neighbours of chunks whose gas or smoke reaches their border.
    pub fn housekeeping(&mut self, eps: f32) {
        for slot in 0..self.pool {
            let c = self.slot_chunk[slot];
            if c == NO_CHUNK {
                continue;
            }
            let (cx, cy) = (c % self.cdim, c / self.cdim);
            // Border spread for drifting clouds.
            for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < 0 || ny < 0 || nx >= self.cdim as i32 || ny >= self.cdim as i32 {
                    continue;
                }
                let nc = ny as u32 * self.cdim + nx as u32;
                if self.table[nc as usize] != NO_SLOT
                    || self.want.contains(&nc)
                    || self.want.len() >= self.want.capacity()
                {
                    continue;
                }
                let mut edge = 0.0f32;
                for l in [Layer::Gas, Layer::Smoke] {
                    let v = self.layer(slot, l);
                    for k in 0..CC {
                        let cell = match (dx, dy) {
                            (-1, _) => k * CC,
                            (1, _) => k * CC + CC - 1,
                            (_, -1) => k,
                            _ => (CC - 1) * CC + k,
                        };
                        edge = edge.max(v[cell]);
                    }
                }
                if edge > eps * 10.0 {
                    self.want.push(nc);
                }
            }
            if self.people_in[slot] > 0 || self.pinned[slot] {
                self.idle[slot] = 0;
                continue;
            }
            let mut mx = 0.0f32;
            for l in [
                Layer::Fear,
                Layer::Anger,
                Layer::Threat,
                Layer::Gas,
                Layer::Smoke,
                Layer::Fire,
                Layer::Chant,
            ] {
                for &v in self.layer(slot, l) {
                    mx = mx.max(v.abs());
                }
            }
            if self.msg[slot * CL..(slot + 1) * CL].iter().any(|&m| m != 0) {
                mx = mx.max(1.0);
            }
            if mx < eps {
                self.idle[slot] = self.idle[slot].saturating_add(1);
                if self.idle[slot] >= 5 {
                    self.release(slot);
                }
            } else {
                self.idle[slot] = 0;
            }
        }
    }

    /// Bytes allocated for the pool.
    pub fn memory_bytes(&self) -> usize {
        (self.data.len() + self.tmp.len()) * 4
            + (self.msg.len() + self.msg_tmp.len()) * 2
            + self.table.len() * 2
    }
}

/// Position of a 24-bit field-cell target (`aux` targets that are places).
#[inline]
pub fn field_cell_pos(f: &Fields, cell: u32) -> Vec2 {
    let dim = f.geom.dim;
    let (cx, cy) = (cell % dim, cell / dim);
    let (x, y) = f.geom.centre(cx, cy);
    Vec2::new(x, y)
}

/// 24-bit field-cell target for a position.
#[inline]
pub fn field_cell_of(f: &Fields, p: Vec2) -> u32 {
    let (cx, cy) = f.geom.cell_of(p.x, p.y);
    (cy * f.geom.dim + cx) & aux::NO_TARGET.wrapping_sub(1)
}

/// The four field cells around a point and its bilinear weights.
#[derive(Clone, Copy, Debug)]
pub struct Probe {
    /// Pool slot when all four cells are in one active chunk, `EMPTY` when
    /// that chunk is inactive, `SLOW` when the cells straddle chunks.
    slot: u32,
    cell: u32,
    x0: i32,
    y0: i32,
    tx: f32,
    ty: f32,
}

impl Probe {
    const EMPTY: u32 = u32::MAX;
    const SLOW: u32 = u32::MAX - 1;

    /// Inlined everywhere: callers read several layers through one probe,
    /// and the shared offset arithmetic folds away.
    #[inline(always)]
    fn corners(&self, f: &Fields, l: Layer) -> [f32; 4] {
        if self.slot < Probe::SLOW {
            let b = (self.slot as usize * LAYERS + l as usize) * CL + self.cell as usize;
            let d = &f.data[b..b + CC + 2];
            [d[0], d[1], d[CC], d[CC + 1]]
        } else if self.slot == Probe::SLOW {
            self.slow_corners(f, l)
        } else {
            [0.0; 4]
        }
    }

    /// The four cells straddle chunks: rare, kept out of line.
    #[cold]
    #[inline(never)]
    fn slow_corners(&self, f: &Fields, l: Layer) -> [f32; 4] {
        [
            f.cell_value(l, self.x0, self.y0),
            f.cell_value(l, self.x0 + 1, self.y0),
            f.cell_value(l, self.x0, self.y0 + 1),
            f.cell_value(l, self.x0 + 1, self.y0 + 1),
        ]
    }

    #[inline(always)]
    pub fn get(&self, f: &Fields, l: Layer) -> f32 {
        if self.slot == Probe::EMPTY {
            return 0.0;
        }
        bilerp(self.corners(f, l), self.tx, self.ty)
    }

    /// Where the four cells are in every layer, for reading several layers
    /// with one bounds check (absorb), and the layers that are not +0.0 in
    /// all four (the `live` bits of their slots).
    #[inline(always)]
    fn cells<'a>(&self, f: &'a Fields) -> (Cells<'a>, u16) {
        if self.slot < Probe::SLOW {
            let s = self.slot as usize;
            let b = s * LAYERS * CL + self.cell as usize;
            // `cell + CC + 2 <= CL` on the fast path, so this always fits.
            match f.data[b..].first_chunk::<WINDOW>() {
                Some(w) => (Cells::Window(w), f.live[s]),
                None => self.split_cells(f),
            }
        } else if self.slot == Probe::SLOW {
            self.split_cells(f)
        } else {
            (Cells::Empty, 0)
        }
    }

    /// Each corner's index into `data` at layer 0 (`None` where its chunk
    /// is inactive): the cells `slow_corners` reads.
    #[cold]
    #[inline(never)]
    fn split_cells<'a>(&self, f: &'a Fields) -> (Cells<'a>, u16) {
        let mut live = 0;
        let mut at = |cx: i32, cy: i32| {
            let s = f.slot_of_chunk(cx.div_euclid(CC as i32), cy.div_euclid(CC as i32))?;
            let cell = (cy.rem_euclid(CC as i32) as usize) * CC + cx.rem_euclid(CC as i32) as usize;
            live |= f.live[s];
            Some(s * LAYERS * CL + cell)
        };
        let c = [
            at(self.x0, self.y0),
            at(self.x0 + 1, self.y0),
            at(self.x0, self.y0 + 1),
            at(self.x0 + 1, self.y0 + 1),
        ];
        (Cells::Split(c), live)
    }

    /// Gradient per metre.
    #[inline]
    pub fn grad(&self, f: &Fields, l: Layer) -> Vec2 {
        if self.slot == Probe::EMPTY {
            return Vec2::ZERO;
        }
        let [a, b, c, d] = self.corners(f, l);
        let gx = (b - a) * (1.0 - self.ty) + (d - c) * self.ty;
        let gy = (c - a) * (1.0 - self.tx) + (d - b) * self.tx;
        Vec2::new(gx, gy) * (1.0 / FIELD_CELL)
    }
}

/// Bilinear interpolation of corners `[x0y0, x1y0, x0y1, x1y1]`.
#[inline(always)]
fn bilerp([a, b, c, d]: [f32; 4], tx: f32, ty: f32) -> f32 {
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    top + (bot - top) * ty
}

/// Floats from a cell of layer 0 to the cell below and right of it in the
/// last layer: the four corners of a fast probe in every layer.
const WINDOW: usize = (LAYERS - 1) * CL + CC + 2;

/// A probe's four cells in every layer (see [`Probe::cells`]).
#[derive(Clone, Copy)]
enum Cells<'a> {
    /// All four in one active chunk: a window of its data.
    Window(&'a [f32; WINDOW]),
    /// Straddling chunks: each corner's index at layer 0, if active.
    Split([Option<usize>; 4]),
    /// In an inactive chunk: every layer reads 0.
    Empty,
}

impl Cells<'_> {
    /// [`Probe::get`] for layer `l`, without its per-layer lookups.
    #[inline(always)]
    fn get(&self, f: &Fields, l: Layer, tx: f32, ty: f32) -> f32 {
        let o = l as usize * CL;
        match *self {
            Cells::Window(w) => bilerp([w[o], w[o + 1], w[o + CC], w[o + CC + 1]], tx, ty),
            Cells::Split(c) => bilerp(c.map(|i| i.map_or(0.0, |i| f.data[i + o])), tx, ty),
            Cells::Empty => 0.0,
        }
    }
}

/// 3-tap blur `[a, 1-2a, a]` as a 5-tap kernel.
pub fn blur_kernel(a: f32) -> [f32; 5] {
    [0.0, a, 1.0 - 2.0 * a, a, 0.0]
}

/// Blur followed by a semi-Lagrangian shift of `s` cells (|s| ≤ 1) in the
/// direction of the wind: the new value at `c` is sampled at `c - s`.
pub fn blur_shift_kernel(a: f32, s: f32) -> [f32; 5] {
    let s = s.clamp(-1.0, 1.0);
    // Shift kernel at offsets -1, 0, +1.
    let sh = if s >= 0.0 {
        [s, 1.0 - s, 0.0]
    } else {
        [0.0, 1.0 + s, -s]
    };
    let b = [a, 1.0 - 2.0 * a, a];
    let mut k = [0.0f32; 5];
    for (i, bv) in b.iter().enumerate() {
        for (j, sv) in sh.iter().enumerate() {
            k[i + j] += bv * sv;
        }
    }
    k
}

/// Inputs to the absorb pass that do not depend on the agent.
pub struct AbsorbParams<'a> {
    pub arch: &'a [ArchetypeParams; ARCHETYPES],
    pub tun: &'a Tunables,
    pub heat: f32,
    pub seed: u64,
    pub tick: u32,
}

/// The least health gas alone leaves anyone with.
const GAS_FLOOR: u8 = 220;

/// Health lost to `harm` (a fraction of full health): whole units, plus one
/// more with the probability of the remainder, so doses smaller than a unit
/// per tick still add up.
#[inline]
fn harm_units(harm: f32, bits: u64) -> u8 {
    let h = harm * 255.0;
    let whole = h as u32;
    (whole + (unit(bits) < h - whole as f32) as u32).min(255) as u8
}

/// What gas and fire did in one tick of [`absorb`].
#[derive(Clone, Copy, Default)]
pub struct AbsorbHarm {
    pub injured: u32,
    pub officers_injured: u32,
    /// Health lost, in 1/255ths of a person.
    pub gas: u32,
    pub fire: u32,
}

impl AbsorbHarm {
    fn add(self, o: AbsorbHarm) -> AbsorbHarm {
        AbsorbHarm {
            injured: self.injured + o.injured,
            officers_injured: self.officers_injured + o.officers_injured,
            gas: self.gas + o.gas,
            fire: self.fire + o.fire,
        }
    }
}

/// Contagion (DESIGN §8 absorb): each agent moves its emotions toward the
/// fields at its position and reacts to gas, chants and fire. Element-wise:
/// each agent writes only to itself.
pub fn absorb(f: &Fields, p: &mut People, sp: &Spatial, ap: &AbsorbParams) -> AbsorbHarm {
    let len = p.len;
    let chunks = p.chunks();
    let (px, py) = (lanes::flat(&p.pos_x), lanes::flat(&p.pos_y));
    let _ = sp;
    let vx = lanes::flat(&p.vel_x);
    let vy = lanes::flat(&p.vel_y);
    let emo_all = &mut p.emo[..chunks * CHUNK_AGENTS];
    let stat_all = &mut p.stat[..chunks * CHUNK_AGENTS];
    let ids = &p.stable_id;
    emo_all
        .par_chunks_mut(CHUNK_AGENTS)
        .zip(stat_all.par_chunks_mut(CHUNK_AGENTS))
        .enumerate()
        .map(|(c, (es, ss))| {
            let base = c * CHUNK_AGENTS;
            let mut out = AbsorbHarm::default();
            for k in 0..es.len() {
                let i = base + k;
                if i >= len || ss[k][stat::FLAGS] & flag::DEAD != 0 {
                    continue;
                }
                let pr = f.probe(px[i], py[i]);
                let (tx, ty) = (pr.tx, pr.ty);
                let speed2 = vx[i] * vx[i] + vy[i] * vy[i];
                let (e, st, id) = (&mut es[k], &mut ss[k], ids[i]);
                // One bounds check for all seven layers on the fast path.
                let (cells, mut live) = pr.cells(f);
                // Four +0.0 corners interpolate to exactly +0.0 for weights
                // in 0..=1, so the probes of layers known +0.0 are skipped;
                // only NaN weights (a NaN position) would make NaN of them.
                if tx.is_nan() || ty.is_nan() {
                    live = u16::MAX;
                }
                match cells {
                    Cells::Window(w) => react(
                        |l| {
                            let o = l as usize * CL;
                            bilerp([w[o], w[o + 1], w[o + CC], w[o + CC + 1]], tx, ty)
                        },
                        live,
                        e,
                        st,
                        speed2,
                        id,
                        ap,
                        &mut out,
                    ),
                    cells => react(
                        |l| cells.get(f, l, tx, ty),
                        live,
                        e,
                        st,
                        speed2,
                        id,
                        ap,
                        &mut out,
                    ),
                }
            }
            out
        })
        .reduce(AbsorbHarm::default, AbsorbHarm::add)
}

/// One agent of [`absorb`]: `get(l)` is the bilinear sample of layer `l` at
/// its position, which is +0.0 for the sparse layers (gas, fire, chants)
/// whose bit in `live` is clear; `speed2` is its squared speed.
#[inline(always)]
fn react(
    get: impl Fn(Layer) -> f32,
    live: u16,
    e: &mut [u8; 4],
    st: &mut [u8; 4],
    speed2: f32,
    id: u32,
    ap: &AbsorbParams,
    out: &mut AbsorbHarm,
) {
    let tun = ap.tun;
    let a = &ap.arch[st[stat::ARCH] as usize];
    let officer = Archetype::from_u8(st[stat::ARCH]).is_officer();
    let mut fear = widen_u8(e[emo::FEAR]);
    let mut anger = widen_u8(e[emo::ANGER]);
    let mut morale = widen_u8(e[emo::MORALE]);
    let mut fatigue = widen_u8(e[emo::FATIGUE]);
    let mut harm = 0.0;
    let mut gas_harm = 0.0;
    let sparse = |l: Layer| if live & bit(l) == 0 { 0.0 } else { get(l) };
    let w = get(Layer::Weight);
    let threat = (get(Layer::Threat) * 0.1).min(1.0);
    if !officer {
        if w > 0.05 {
            let inv = 1.0 / w;
            let ff = get(Layer::Fear) * inv;
            let fa = get(Layer::Anger) * inv;
            fear += tun.contagion_fear * (ff - fear);
            // Anger catches more easily than it calms.
            let ka = if fa > anger {
                tun.contagion_anger
            } else {
                tun.contagion_anger * 0.3
            };
            anger += ka * (fa - anger);
        }
        let chant = sparse(Layer::Chant);
        morale += tun.chant_morale * chant.min(2.0);
        fear -= 0.5 * tun.chant_morale * chant.min(2.0);
        // Police and violence nearby: the timid fear it, the willing are angered.
        fear += tun.threat_fear * threat * (1.0 - a.violence);
        anger += tun.threat_anger * threat * (a.violence + 0.1);
    } else if w > 0.05 {
        // A hostile crowd frightens officers.
        let hostile = get(Layer::Anger) / w * (w * 0.25).min(1.0);
        fear += tun.threat_fear * 0.5 * hostile * (1.0 - a.violence);
    }
    let gas = sparse(Layer::Gas);
    if gas > 0.01 {
        // Officers wear masks: gas barely frightens or tires them.
        let g = gas.min(4.0);
        fear += tun.gas_fear * g * if officer { 0.03 } else { 1.0 };
        fatigue += tun.gas_fatigue * g * if officer { 0.25 } else { 1.0 };
        if !officer {
            gas_harm = tun.gas_damage * g;
            morale -= tun.gas_morale * g;
        }
        if gas > 0.3 {
            st[stat::FLAGS] |= flag::URGENT;
        }
    }
    let fire = sparse(Layer::Fire);
    if fire > 0.05 {
        // The sampled field bleeds a cell past the flames: it
        // frightens there, but only burns inside them.
        harm += tun.fire_damage * ((fire - 0.3) * (1.0 / 0.7)).max(0.0);
        fear += 0.05 * fire;
        st[stat::FLAGS] |= flag::URGENT;
    }
    // Relax toward the archetype's baseline.
    let r = tun.emotion_relax;
    fear += r * (a.base_fear - fear);
    anger += r * (a.base_anger - anger);
    morale -= tun.morale_decay * (0.5 + fear);
    // Fatigue from moving and heat; rest recovers it.
    let act = Action::from_u8(st[stat::ACTION]);
    let mut dfat = tun.fatigue_heat * ap.heat;
    if speed2 > a.walk * a.walk * 1.44 {
        dfat += tun.fatigue_run;
    } else if speed2 > 0.04 {
        dfat += tun.fatigue_walk;
    } else if act == Action::Rest {
        dfat -= tun.rest_recover;
    } else {
        dfat -= tun.rest_recover * 0.25;
    }
    fatigue += dfat / a.stamina;
    // Rates are far below one byte step per tick: dither.
    let bits = roll(ap.seed, ap.tick, id, Stream::Emotion);
    e[emo::FEAR] = dither_u8(fear, bits);
    e[emo::ANGER] = dither_u8(anger, bits >> 16);
    e[emo::MORALE] = dither_u8(morale, bits >> 32);
    e[emo::FATIGUE] = dither_u8(fatigue, bits >> 48);
    if harm + gas_harm > 0.0 {
        let hp = st[stat::HEALTH];
        let bits = roll(ap.seed, ap.tick, id, Stream::Hazard);
        // Gas alone wears people down but injures nobody, and
        // leaves a margin so a graze is not an injury either.
        let gassed = hp
            .saturating_sub(harm_units(gas_harm, bits))
            .max(hp.min(GAS_FLOOR));
        let after = gassed.saturating_sub(harm_units(harm, bits.rotate_left(24)));
        out.gas += (hp - gassed) as u32;
        out.fire += (gassed - after) as u32;
        if hp >= health::INJURED && after < health::INJURED {
            if officer {
                out.officers_injured += 1;
            } else {
                out.injured += 1;
            }
        }
        st[stat::HEALTH] = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels_conserve_mass() {
        for s in [-1.0f32, -0.3, 0.0, 0.25, 1.0] {
            let k = blur_shift_kernel(0.15, s);
            let sum: f32 = k.iter().sum();
            assert!((sum - 1.0).abs() < 1e-6, "{s}: {k:?}");
        }
        assert_eq!(blur_shift_kernel(0.2, 0.0), blur_kernel(0.2));
    }
}
