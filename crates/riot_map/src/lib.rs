//! Lookup structures derived from the map polygons at load time (DESIGN §7.1,
//! §9). None of them decides where people can stand; the polygons do.
//!
//! - **Nav tiles**: the 0.5 m nav grid is stored per 8 m tile. A tile is open,
//!   blocked or mixed, and only mixed tiles keep a 256-bit walkability bitmap,
//!   their wall lists (one per 4 m sub-cell) and their connected components.
//! - **Regions**: 128 m blocks of 16 × 16 tiles. Each region's tile components
//!   are grouped into region components, which form the coarse routing graph
//!   (DESIGN §9, two-level routing).
//!
//! Nothing here is written during a riot.

#![deny(unsafe_code)]

use rayon::prelude::*;
use riot_core::Vec2;
use riot_core::grid::GridGeom;

use riot_defs::mapdef::{EntryKind, MapDef, PropDef};

pub const NAV_CELL: f32 = 0.5;
/// Nav cells per tile side.
pub const TILE: u32 = 16;
pub const TILE_M: f32 = 8.0;
/// Tiles per region side.
pub const REGION: u32 = 16;
pub const REGION_M: f32 = 128.0;
/// A nav cell is open only if a body of this radius fits at its centre. A
/// little under the smallest body, so a 0.9 m doorway always keeps an open
/// cell whatever its alignment to the grid.
pub const CLEARANCE: f32 = 0.19;
pub const OPEN: u32 = u32::MAX - 1;
pub const BLOCKED: u32 = u32::MAX;
pub const MAX_COMP: usize = 4;
pub const NO_LABELS: u32 = u32::MAX;
pub const NO_RC: u8 = u8::MAX;
/// Route cost units per metre: an orthogonal nav-cell step costs 10.
pub const COST_PER_M: f32 = 20.0;
pub const LARGEST_MAP: f32 = 8192.0;

/// A tile that contains walls (DESIGN §9 "mixed").
#[derive(Clone, Debug)]
pub struct MixedTile {
    /// Walkability per cell, bit `ly * 16 + lx`.
    pub bits: [u64; 4],
    /// `[start, end)` into `Map::wall_idx` for each 4 m sub-cell, row-major.
    pub walls: [[u32; 2]; 4],
    /// Index into `Map::labels` when the tile has 2+ components.
    pub labels: u32,
    pub ncomp: u8,
    /// Region component of each tile component.
    pub rc: [u8; MAX_COMP],
    /// Connectivity across each border (+x, -x, +y, -y): bit `a * 4 + b` is
    /// set when component `a` here touches component `b` of the neighbour.
    pub links: [u16; 4],
}

impl MixedTile {
    #[inline]
    pub fn walkable(&self, bit: u32) -> bool {
        (self.bits[(bit >> 6) as usize] >> (bit & 63)) & 1 != 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegionNode {
    pub region: u32,
    pub centroid: Vec2,
    pub tiles: u32,
}

/// An entry point, resolved to an open position.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub pos: Vec2,
    pub radius: f32,
}

#[derive(Clone, Debug)]
pub struct Place {
    pub name: String,
    pub pos: Vec2,
    pub radius: f32,
    pub tag: String,
}

/// Road graph in CSR form, for vehicles (DESIGN §2.8).
#[derive(Clone, Debug, Default)]
pub struct Roads {
    pub nodes: Vec<Vec2>,
    pub start: Vec<u32>,
    pub adj: Vec<(u32, f32)>,
}

impl Roads {
    pub fn nearest(&self, p: Vec2) -> Option<u32> {
        let mut best = None;
        let mut bd = f32::INFINITY;
        for (i, n) in self.nodes.iter().enumerate() {
            let d = (*n - p).len2();
            if d < bd {
                bd = d;
                best = Some(i as u32);
            }
        }
        best
    }
}

pub struct Map {
    pub name: String,
    pub width: f32,
    pub height: f32,
    /// Side of the square power-of-two world the map is padded to.
    pub size: f32,
    pub half: f32,
    pub nav: GridGeom,
    pub tdim: u32,
    pub tshift: u32,
    pub rdim: u32,
    pub tiles: Vec<u32>,
    pub tile_rc0: Vec<u8>,
    pub mixed: Vec<MixedTile>,
    pub labels: Vec<[u8; 256]>,
    pub seg_ax: Vec<f32>,
    pub seg_ay: Vec<f32>,
    pub seg_bx: Vec<f32>,
    pub seg_by: Vec<f32>,
    pub wall_idx: Vec<u32>,
    pub region_first: Vec<u32>,
    pub rnodes: Vec<RegionNode>,
    pub radj_start: Vec<u32>,
    pub radj: Vec<(u32, u32)>,
    pub roads: Roads,
    pub entries: Vec<Entry>,
    pub depots: Vec<Place>,
    pub staging: Vec<Place>,
    pub areas: Vec<Place>,
    pub props: Vec<PropDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapError {
    TooLarge(f32),
    Empty,
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapError::TooLarge(s) => write!(
                f,
                "map is {s} m across; the largest supported is {LARGEST_MAP} m"
            ),
            MapError::Empty => write!(f, "map has no area"),
        }
    }
}

impl std::error::Error for MapError {}

struct Seg {
    a: Vec2,
    b: Vec2,
}

impl Map {
    /// Derive every lookup structure from the authored map. `margin` is how
    /// far from a wall a body centre can be and still touch it (largest body
    /// radius plus slack).
    pub fn build(def: &MapDef, margin: f32) -> Result<Map, MapError> {
        let extent = def.width.max(def.height);
        if extent.is_nan() || extent <= 0.0 {
            return Err(MapError::Empty);
        }
        if extent > LARGEST_MAP {
            return Err(MapError::TooLarge(extent));
        }
        let size = (extent.max(REGION_M) as u32).next_power_of_two() as f32;
        let half = size * 0.5;
        let nav = GridGeom::new(size, NAV_CELL);
        let tdim = (size / TILE_M) as u32;
        let tshift = tdim.trailing_zeros();
        let rdim = tdim / REGION;
        let (hw, hh) = (def.width * 0.5, def.height * 0.5);

        // --- segments: solids (count for winding) then the map frame (collision only)
        let mut segs: Vec<Seg> = Vec::new();
        for poly in &def.solids {
            let n = poly.points.len();
            if n < 3 {
                continue;
            }
            let mut area = 0.0f64;
            for i in 0..n {
                let [x0, y0] = poly.points[i];
                let [x1, y1] = poly.points[(i + 1) % n];
                area += x0 as f64 * y1 as f64 - x1 as f64 * y0 as f64;
            }
            let pts: Vec<Vec2> = if area >= 0.0 {
                poly.points.iter().map(|p| Vec2::new(p[0], p[1])).collect()
            } else {
                poly.points
                    .iter()
                    .rev()
                    .map(|p| Vec2::new(p[0], p[1]))
                    .collect()
            };
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                if (b - a).len2() > 1e-8 {
                    segs.push(Seg { a, b });
                }
            }
        }
        let solid_segs = segs.len();
        let corners = [
            Vec2::new(-hw, -hh),
            Vec2::new(hw, -hh),
            Vec2::new(hw, hh),
            Vec2::new(-hw, hh),
        ];
        for i in 0..4 {
            segs.push(Seg {
                a: corners[i],
                b: corners[(i + 1) % 4],
            });
        }

        let boxes: Vec<[f32; 4]> = segs.iter().map(clear_box).collect();

        // --- (tile, segment) pairs for every tile within `margin` of a segment
        let tile_of =
            |v: f32| (((v + half) / TILE_M).floor() as i64).clamp(0, tdim as i64 - 1) as u32;
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (s, seg) in segs.iter().enumerate() {
            let (x0, x1) = (seg.a.x.min(seg.b.x) - margin, seg.a.x.max(seg.b.x) + margin);
            let (y0, y1) = (seg.a.y.min(seg.b.y) - margin, seg.a.y.max(seg.b.y) + margin);
            for ty in tile_of(y0)..=tile_of(y1) {
                for tx in tile_of(x0)..=tile_of(x1) {
                    let bmin = Vec2::new(tx as f32 * TILE_M - half, ty as f32 * TILE_M - half);
                    if seg_box_dist2(seg.a, seg.b, bmin, bmin + Vec2::new(TILE_M, TILE_M))
                        <= margin * margin
                    {
                        pairs.push(((ty << tshift) | tx, s as u32));
                    }
                }
            }
        }
        pairs.sort_unstable();

        // --- rasterize per tile row, in parallel
        struct RowOut {
            kinds: Vec<u32>,
            mixed: Vec<(MixedTile, [Vec<u32>; 4], Option<[u8; 256]>)>,
        }
        let rows: Vec<RowOut> = (0..tdim)
            .into_par_iter()
            .map(|ty| {
                let lo = pairs.partition_point(|p| p.0 < ty << tshift);
                let hi = pairs.partition_point(|p| p.0 < (ty + 1) << tshift);
                let row_pairs = &pairs[lo..hi];
                // Signed crossings of each cell row's centre line, sorted by x.
                let mut cross: [Vec<(f32, i32)>; TILE as usize] = Default::default();
                for &(t, s) in row_pairs {
                    if s as usize >= solid_segs {
                        continue;
                    }
                    let tx = t & (tdim - 1);
                    let seg = &segs[s as usize];
                    for (ly, c) in cross.iter_mut().enumerate() {
                        let y = (ty * TILE + ly as u32) as f32 * NAV_CELL + NAV_CELL * 0.5 - half;
                        let sign = if seg.a.y <= y && y < seg.b.y {
                            1
                        } else if seg.b.y <= y && y < seg.a.y {
                            -1
                        } else {
                            continue;
                        };
                        let xc =
                            seg.a.x + (y - seg.a.y) / (seg.b.y - seg.a.y) * (seg.b.x - seg.a.x);
                        if tile_of(xc) == tx {
                            c.push((xc, sign));
                        }
                    }
                }
                for c in cross.iter_mut() {
                    c.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                }
                let mut ptr = [0usize; TILE as usize];
                let mut wind = [0i32; TILE as usize];
                let mut seg_cols: Vec<u16> = Vec::new();
                let mut out = RowOut {
                    kinds: Vec::with_capacity(tdim as usize),
                    mixed: Vec::new(),
                };
                let mut pi = 0usize;
                for tx in 0..tdim {
                    let t = (ty << tshift) | tx;
                    let p0 = pi;
                    while pi < row_pairs.len() && row_pairs[pi].0 == t {
                        pi += 1;
                    }
                    let tile_segs = &row_pairs[p0..pi];
                    let x0 = tx as f32 * TILE_M - half;
                    let y0 = ty as f32 * TILE_M - half;
                    // Cell centres of the tile's columns, and the columns
                    // inside the map. The centres never decrease with `lx`.
                    let mut xs = [0.0f32; TILE as usize];
                    let mut cols_in = 0u16;
                    for (lx, x) in xs.iter_mut().enumerate() {
                        *x = x0 + (lx as f32 + 0.5) * NAV_CELL;
                        if *x > -hw && *x < hw {
                            cols_in |= 1 << lx;
                        }
                    }
                    // The columns whose centre is inside each segment's
                    // clearance box. A cell outside the box is farther than
                    // CLEARANCE from the segment, so its clearance test
                    // below would fail anyway (see `clear_box`).
                    seg_cols.clear();
                    seg_cols.extend(tile_segs.iter().map(|&(_, s)| {
                        let b = &boxes[s as usize];
                        let mut cols = 0u16;
                        for (lx, &x) in xs.iter().enumerate() {
                            if !(x < b[0] || x > b[1]) {
                                cols |= 1 << lx;
                            }
                        }
                        cols
                    }));
                    let mut bits = [0u64; 4];
                    for ly in 0..TILE as usize {
                        let y = y0 + (ly as f32 + 0.5) * NAV_CELL;
                        // Winding of each cell: the crossings left of its centre.
                        let c = &cross[ly];
                        let (mut p, mut w) = (ptr[ly], wind[ly]);
                        while p < c.len() && c[p].0 < xs[0] {
                            w += c[p].1;
                            p += 1;
                        }
                        let mut row = if p == c.len() || c[p].0 >= xs[TILE as usize - 1] {
                            // No crossing up to the last centre: one winding.
                            if w == 0 { u16::MAX } else { 0 }
                        } else {
                            let mut row = 0u16;
                            for (lx, &x) in xs.iter().enumerate() {
                                while p < c.len() && c[p].0 < x {
                                    w += c[p].1;
                                    p += 1;
                                }
                                if w == 0 {
                                    row |= 1 << lx;
                                }
                            }
                            row
                        };
                        (ptr[ly], wind[ly]) = (p, w);
                        if !(y > -hh && y < hh) {
                            row = 0;
                        }
                        row &= cols_in;
                        // Clearance: no wall closer than CLEARANCE. Only the
                        // cells inside a segment's box need the exact test.
                        for (&(_, s), &cols) in tile_segs.iter().zip(&seg_cols) {
                            if row == 0 {
                                break;
                            }
                            let b = &boxes[s as usize];
                            if y < b[2] || y > b[3] {
                                continue;
                            }
                            let mut cand = row & cols;
                            let seg = &segs[s as usize];
                            while cand != 0 {
                                let lx = cand.trailing_zeros() as usize;
                                cand &= cand - 1;
                                let p = Vec2::new(xs[lx], y);
                                if (riot_core::math::closest_on_segment(p, seg.a, seg.b) - p).len2()
                                    < CLEARANCE * CLEARANCE
                                {
                                    row &= !(1 << lx);
                                }
                            }
                        }
                        bits[ly >> 2] |= (row as u64) << ((ly & 3) * 16);
                    }
                    let any_open = bits != [0; 4];
                    if !any_open {
                        out.kinds.push(BLOCKED);
                        continue;
                    }
                    if tile_segs.is_empty() {
                        out.kinds.push(OPEN);
                        continue;
                    }
                    // Wall lists per 4 m sub-cell.
                    let mut lists: [Vec<u32>; 4] = Default::default();
                    for (q, list) in lists.iter_mut().enumerate() {
                        let bmin = Vec2::new(x0 + (q & 1) as f32 * 4.0, y0 + (q >> 1) as f32 * 4.0);
                        let bmax = bmin + Vec2::new(4.0, 4.0);
                        for &(_, s) in tile_segs {
                            let seg = &segs[s as usize];
                            if seg_box_near(seg, bmin, bmax, margin) {
                                list.push(s);
                            }
                        }
                    }
                    let (ncomp, labels) = components(&mut bits);
                    out.kinds.push(u32::MAX - 2); // patched below
                    out.mixed.push((
                        MixedTile {
                            bits,
                            walls: [[0; 2]; 4],
                            labels: NO_LABELS,
                            ncomp,
                            rc: [NO_RC; MAX_COMP],
                            links: [0; 4],
                        },
                        lists,
                        labels,
                    ));
                }
                out
            })
            .collect();

        let mut tiles = Vec::with_capacity((tdim * tdim) as usize);
        let mut mixed = Vec::new();
        let mut labels = Vec::new();
        let mut wall_idx = Vec::new();
        for row in rows {
            let mut m = row.mixed.into_iter();
            for k in row.kinds {
                if k == u32::MAX - 2 {
                    let (mut mt, lists, lab) = m.next().unwrap();
                    for (q, list) in lists.iter().enumerate() {
                        let s = wall_idx.len() as u32;
                        wall_idx.extend_from_slice(list);
                        mt.walls[q] = [s, wall_idx.len() as u32];
                    }
                    if let Some(l) = lab {
                        mt.labels = labels.len() as u32;
                        labels.push(l);
                    }
                    tiles.push(mixed.len() as u32);
                    mixed.push(mt);
                } else {
                    tiles.push(k);
                }
            }
        }

        let mut map = Map {
            name: def.name.clone(),
            width: def.width,
            height: def.height,
            size,
            half,
            nav,
            tdim,
            tshift,
            rdim,
            tiles,
            tile_rc0: Vec::new(),
            mixed,
            labels,
            seg_ax: segs.iter().map(|s| s.a.x).collect(),
            seg_ay: segs.iter().map(|s| s.a.y).collect(),
            seg_bx: segs.iter().map(|s| s.b.x).collect(),
            seg_by: segs.iter().map(|s| s.b.y).collect(),
            wall_idx,
            region_first: Vec::new(),
            rnodes: Vec::new(),
            radj_start: Vec::new(),
            radj: Vec::new(),
            roads: Roads::default(),
            entries: Vec::new(),
            depots: Vec::new(),
            staging: Vec::new(),
            areas: Vec::new(),
            props: def.props.clone(),
        };
        map.build_links();
        map.build_regions();
        map.resolve_places(def);
        Ok(map)
    }

    // -----------------------------------------------------------------------
    // Cell and tile queries

    #[inline]
    pub fn tile_index(&self, tx: u32, ty: u32) -> u32 {
        (ty << self.tshift) | tx
    }

    /// Nav cell containing a point, if it is on the grid.
    #[inline]
    pub fn cell_of(&self, p: Vec2) -> Option<(u32, u32)> {
        let (fx, fy) = self.nav.to_cell_f(p.x, p.y);
        if fx < 0.0 || fy < 0.0 || fx >= self.nav.dim as f32 || fy >= self.nav.dim as f32 {
            None
        } else {
            Some((fx as u32, fy as u32))
        }
    }

    #[inline]
    pub fn walkable(&self, cx: u32, cy: u32) -> bool {
        if cx >= self.nav.dim || cy >= self.nav.dim {
            return false;
        }
        match self.tiles[self.tile_index(cx >> 4, cy >> 4) as usize] {
            OPEN => true,
            BLOCKED => false,
            m => self.mixed[m as usize].walkable((cy & 15) * 16 + (cx & 15)),
        }
    }

    #[inline]
    pub fn walkable_at(&self, p: Vec2) -> bool {
        self.cell_of(p).is_some_and(|(x, y)| self.walkable(x, y))
    }

    /// Component of a walkable nav cell within its tile.
    #[inline]
    pub fn comp_of(&self, cx: u32, cy: u32) -> Option<u8> {
        if cx >= self.nav.dim || cy >= self.nav.dim {
            return None;
        }
        match self.tiles[self.tile_index(cx >> 4, cy >> 4) as usize] {
            OPEN => Some(0),
            BLOCKED => None,
            m => {
                let mt = &self.mixed[m as usize];
                let bit = (cy & 15) * 16 + (cx & 15);
                if !mt.walkable(bit) {
                    None
                } else if mt.labels == NO_LABELS {
                    Some(0)
                } else {
                    Some(self.labels[mt.labels as usize][bit as usize])
                }
            }
        }
    }

    /// Number of components of a tile.
    #[inline]
    pub fn tile_ncomp(&self, t: u32) -> u8 {
        match self.tiles[t as usize] {
            OPEN => 1,
            BLOCKED => 0,
            m => self.mixed[m as usize].ncomp,
        }
    }

    /// Region node of a tile component.
    #[inline]
    pub fn rnode_of(&self, t: u32, comp: u8) -> Option<u32> {
        let rc = match self.tiles[t as usize] {
            OPEN => self.tile_rc0[t as usize],
            BLOCKED => NO_RC,
            m => self.mixed[m as usize].rc[comp as usize],
        };
        if rc == NO_RC {
            return None;
        }
        let (tx, ty) = (t & (self.tdim - 1), t >> self.tshift);
        let r = (ty / REGION) * self.rdim + tx / REGION;
        Some(self.region_first[r as usize] + rc as u32)
    }

    #[inline]
    pub fn region_of_tile(&self, t: u32) -> u32 {
        let (tx, ty) = (t & (self.tdim - 1), t >> self.tshift);
        (ty / REGION) * self.rdim + tx / REGION
    }

    /// The tile across border `dir` (+x, -x, +y, -y) of `t`.
    #[inline]
    pub fn neighbour_tile(&self, t: u32, dir: usize) -> Option<u32> {
        let (tx, ty) = (t & (self.tdim - 1), t >> self.tshift);
        let (nx, ny) = match dir {
            0 => (tx.checked_add(1)?, ty),
            1 => (tx.checked_sub(1)?, ty),
            2 => (tx, ty.checked_add(1)?),
            _ => (tx, ty.checked_sub(1)?),
        };
        (nx < self.tdim && ny < self.tdim).then(|| self.tile_index(nx, ny))
    }

    /// Which components of `t` touch which components of its neighbour
    /// across border `dir`: bit `a * 4 + b`.
    #[inline]
    pub fn tile_links(&self, t: u32, dir: usize) -> u16 {
        let Some(nt) = self.neighbour_tile(t, dir) else {
            return 0;
        };
        match (self.tiles[t as usize], self.tiles[nt as usize]) {
            (BLOCKED, _) | (_, BLOCKED) => 0,
            (OPEN, OPEN) => 1,
            (OPEN, m) => transpose_links(self.mixed[m as usize].links[dir ^ 1]),
            (m, _) => self.mixed[m as usize].links[dir],
        }
    }

    /// Component of each cell along one side of a tile (0 +x, 1 -x, 2 +y,
    /// 3 -y), in order along the side, `NO_RC` where not walkable: the
    /// `comp_of` of those cells.
    fn border_comps(&self, t: u32, side: usize) -> [u8; TILE as usize] {
        let m = match self.tiles[t as usize] {
            OPEN => return [0; TILE as usize],
            BLOCKED => return [NO_RC; TILE as usize],
            m => &self.mixed[m as usize],
        };
        let labels = (m.labels != NO_LABELS).then(|| &self.labels[m.labels as usize]);
        std::array::from_fn(|k| {
            let k = k as u32;
            let bit = match side {
                0 => k * 16 + 15,
                1 => k * 16,
                2 => 15 * 16 + k,
                _ => k,
            };
            if !m.walkable(bit) {
                NO_RC
            } else {
                labels.map_or(0, |l| l[bit as usize])
            }
        })
    }

    /// Links of every mixed tile across its four borders, in parallel per
    /// tile row: each depends only on the bits and labels of the tile and its
    /// neighbours.
    fn build_links(&mut self) {
        let this = &*self;
        let rows: Vec<Vec<[u16; 4]>> = (0..this.tdim)
            .into_par_iter()
            .map(|ty| {
                let mut out = Vec::new();
                for tx in 0..this.tdim {
                    let t = this.tile_index(tx, ty);
                    let m = this.tiles[t as usize];
                    if m == OPEN || m == BLOCKED {
                        continue;
                    }
                    let mut links = [0u16; 4];
                    for (dir, l) in links.iter_mut().enumerate() {
                        let Some(nt) = this.neighbour_tile(t, dir) else {
                            continue;
                        };
                        // The cells facing each other across the border.
                        let (a, b) = (this.border_comps(t, dir), this.border_comps(nt, dir ^ 1));
                        for (&ca, &cb) in a.iter().zip(&b) {
                            if ca != NO_RC && cb != NO_RC {
                                *l |= 1 << (ca * 4 + cb);
                            }
                        }
                    }
                    out.push(links);
                }
                out
            })
            .collect();
        // Mixed tiles are numbered in tile order, as the rows are.
        let mut mixed = self.mixed.iter_mut();
        for links in rows.into_iter().flatten() {
            mixed.next().expect("one link set per mixed tile").links = links;
        }
        debug_assert!(mixed.next().is_none());
    }

    /// Wall segments near a point: the list of the 4 m sub-cell containing it.
    #[inline]
    pub fn walls_near(&self, p: Vec2) -> &[u32] {
        let fx = (p.x + self.half) * (1.0 / 4.0);
        let fy = (p.y + self.half) * (1.0 / 4.0);
        if fx < 0.0 || fy < 0.0 {
            return &[];
        }
        let (sx, sy) = (fx as u32, fy as u32);
        let (tx, ty) = (sx >> 1, sy >> 1);
        if tx >= self.tdim || ty >= self.tdim {
            return &[];
        }
        match self.tiles[self.tile_index(tx, ty) as usize] {
            OPEN | BLOCKED => &[],
            m => {
                let [s, e] = self.mixed[m as usize].walls[((sy & 1) * 2 + (sx & 1)) as usize];
                &self.wall_idx[s as usize..e as usize]
            }
        }
    }

    #[inline]
    pub fn segment(&self, s: u32) -> (Vec2, Vec2) {
        let s = s as usize;
        (
            Vec2::new(self.seg_ax[s], self.seg_ay[s]),
            Vec2::new(self.seg_bx[s], self.seg_by[s]),
        )
    }

    /// Does the straight segment `a → b` cross a wall? Walks the 4 m
    /// sub-cells along it.
    pub fn ray_blocked(&self, a: Vec2, b: Vec2) -> Option<f32> {
        let d = b - a;
        let len = d.len();
        let steps = (len / 2.0) as u32 + 1;
        let mut best: Option<f32> = None;
        for k in 0..=steps {
            let p = a + d * (k as f32 / steps as f32);
            for &s in self.walls_near(p) {
                let (sa, sb) = self.segment(s);
                if let Some(t) = riot_core::math::segment_hit(a, b, sa, sb) {
                    best = Some(best.map_or(t, |x: f32| x.min(t)));
                }
            }
            if best.is_some() {
                return best;
            }
        }
        best
    }

    /// Nearest walkable cell centre within `max_r` metres.
    pub fn nearest_open(&self, p: Vec2, max_r: f32) -> Option<Vec2> {
        let (fx, fy) = self.nav.to_cell_f(p.x, p.y);
        let (cx, cy) = (fx.floor() as i64, fy.floor() as i64);
        let rmax = (max_r / NAV_CELL) as i64 + 1;
        for r in 0..=rmax {
            let mut best: Option<(f32, Vec2)> = None;
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let (x, y) = (cx + dx, cy + dy);
                    if x < 0 || y < 0 || !self.walkable(x as u32, y as u32) {
                        continue;
                    }
                    let (wx, wy) = self.nav.centre(x as u32, y as u32);
                    let c = Vec2::new(wx, wy);
                    let d = (c - p).len2();
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, c));
                    }
                }
            }
            if let Some((_, c)) = best {
                return Some(c);
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Regions (DESIGN §9 two-level routing)

    fn build_regions(&mut self) {
        let rdim = self.rdim;
        let tdim = self.tdim;
        let rcount = (rdim * rdim) as usize;
        // Union-find over the tile components of each region, in parallel.
        struct RegionOut {
            /// Region component of every (tile, comp) in the region, row-major tiles.
            rc: Vec<[u8; MAX_COMP]>,
            nodes: Vec<(Vec2, u32)>,
        }
        let this = &*self;
        let outs: Vec<RegionOut> = (0..rcount as u32)
            .into_par_iter()
            .map(|r| {
                let (rx, ry) = (r % rdim, r / rdim);
                let n = (REGION * REGION) as usize;
                let mut parent: Vec<u16> = (0..(n * MAX_COMP) as u16).collect();
                fn find(p: &mut [u16], mut x: u16) -> u16 {
                    while p[x as usize] != x {
                        p[x as usize] = p[p[x as usize] as usize];
                        x = p[x as usize];
                    }
                    x
                }
                let key = |lx: u32, ly: u32, c: u8| {
                    ((ly * REGION + lx) as usize * MAX_COMP + c as usize) as u16
                };
                // Join the components that touch across the +x and +y borders
                // inside the region: the links `build_links` found along the
                // border cells. Union by minimum keeps every root the
                // smallest key of its set whatever the order of the unions,
                // so the ids below depend only on the partition.
                for ly in 0..REGION {
                    for lx in 0..REGION {
                        let t = this.tile_index(rx * REGION + lx, ry * REGION + ly);
                        for (dir, nx, ny) in [(0, lx + 1, ly), (2, lx, ly + 1)] {
                            if nx >= REGION || ny >= REGION {
                                continue;
                            }
                            let mut l = this.tile_links(t, dir);
                            while l != 0 {
                                let bit = l.trailing_zeros();
                                l &= l - 1;
                                let (a, b) = ((bit / 4) as u8, (bit % 4) as u8);
                                let (ra, rb) = (
                                    find(&mut parent, key(lx, ly, a)),
                                    find(&mut parent, key(nx, ny, b)),
                                );
                                if ra != rb {
                                    let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                                    parent[hi as usize] = lo;
                                }
                            }
                        }
                    }
                }
                let mut rc = vec![[NO_RC; MAX_COMP]; n];
                let mut root_id: Vec<u8> = vec![NO_RC; n * MAX_COMP];
                let mut nodes: Vec<(Vec2, u32)> = Vec::new();
                let mut sums: Vec<(f64, f64, u32)> = Vec::new();
                for ly in 0..REGION {
                    for lx in 0..REGION {
                        let t = this.tile_index(rx * REGION + lx, ry * REGION + ly);
                        for c in 0..this.tile_ncomp(t) {
                            let root = find(&mut parent, key(lx, ly, c));
                            let id = &mut root_id[root as usize];
                            if *id == NO_RC {
                                if nodes.len() >= (NO_RC - 1) as usize {
                                    continue;
                                }
                                *id = nodes.len() as u8;
                                nodes.push((Vec2::ZERO, 0));
                                sums.push((0.0, 0.0, 0));
                            }
                            rc[(ly * REGION + lx) as usize][c as usize] = *id;
                            let s = &mut sums[*id as usize];
                            s.0 += ((rx * REGION + lx) as f64 + 0.5) * TILE_M as f64;
                            s.1 += ((ry * REGION + ly) as f64 + 0.5) * TILE_M as f64;
                            s.2 += 1;
                        }
                    }
                }
                for (node, s) in nodes.iter_mut().zip(&sums) {
                    *node = (
                        Vec2::new(
                            (s.0 / s.2 as f64) as f32 - this.half,
                            (s.1 / s.2 as f64) as f32 - this.half,
                        ),
                        s.2,
                    );
                }
                RegionOut { rc, nodes }
            })
            .collect();

        self.tile_rc0 = vec![NO_RC; (tdim * tdim) as usize];
        self.region_first = Vec::with_capacity(rcount + 1);
        for (r, out) in outs.iter().enumerate() {
            self.region_first.push(self.rnodes.len() as u32);
            for &(centroid, tiles) in &out.nodes {
                self.rnodes.push(RegionNode {
                    region: r as u32,
                    centroid,
                    tiles,
                });
            }
            let (rx, ry) = (r as u32 % rdim, r as u32 / rdim);
            for ly in 0..REGION {
                for lx in 0..REGION {
                    let t = self.tile_index(rx * REGION + lx, ry * REGION + ly);
                    let rc = out.rc[(ly * REGION + lx) as usize];
                    match self.tiles[t as usize] {
                        OPEN => self.tile_rc0[t as usize] = rc[0],
                        BLOCKED => {}
                        m => {
                            self.tile_rc0[t as usize] = rc[0];
                            self.mixed[m as usize].rc = rc;
                        }
                    }
                }
            }
        }
        self.region_first.push(self.rnodes.len() as u32);

        // Region adjacency across region borders, from the tile links (the
        // sort and dedup make the list independent of the push order).
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for ty in 0..tdim {
            for tx in 0..tdim {
                for (dir, nx, ny) in [(0, tx + 1, ty), (2, tx, ty + 1)] {
                    if nx >= tdim
                        || ny >= tdim
                        || (nx / REGION == tx / REGION && ny / REGION == ty / REGION)
                    {
                        continue;
                    }
                    let (ta, tb) = (self.tile_index(tx, ty), self.tile_index(nx, ny));
                    let mut l = self.tile_links(ta, dir);
                    while l != 0 {
                        let bit = l.trailing_zeros();
                        l &= l - 1;
                        let (a, b) = ((bit / 4) as u8, (bit % 4) as u8);
                        if let (Some(na), Some(nb)) = (self.rnode_of(ta, a), self.rnode_of(tb, b)) {
                            edges.push((na, nb));
                            edges.push((nb, na));
                        }
                    }
                }
            }
        }
        edges.sort_unstable();
        edges.dedup();
        self.radj_start = vec![0; self.rnodes.len() + 1];
        for &(a, _) in &edges {
            self.radj_start[a as usize + 1] += 1;
        }
        for i in 1..self.radj_start.len() {
            self.radj_start[i] += self.radj_start[i - 1];
        }
        self.radj = edges
            .iter()
            .map(|&(a, b)| {
                let d = (self.rnodes[a as usize].centroid - self.rnodes[b as usize].centroid).len();
                (b, (d * COST_PER_M) as u32 + 1)
            })
            .collect();
    }

    #[inline]
    pub fn region_neighbours(&self, n: u32) -> &[(u32, u32)] {
        &self.radj[self.radj_start[n as usize] as usize..self.radj_start[n as usize + 1] as usize]
    }

    fn resolve_places(&mut self, def: &MapDef) {
        let snap = |m: &Map, p: [f32; 2]| {
            let v = Vec2::new(p[0], p[1]);
            m.nearest_open(v, 64.0).unwrap_or(v)
        };
        self.entries = def
            .entries
            .iter()
            .map(|e| Entry {
                name: e.name.clone(),
                kind: e.kind,
                pos: snap(self, e.pos),
                radius: e.radius.max(1.0),
            })
            .collect();
        self.depots = def
            .depots
            .iter()
            .map(|d| Place {
                name: d.name.clone(),
                pos: snap(self, d.pos),
                radius: 6.0,
                tag: String::new(),
            })
            .collect();
        self.staging = def
            .staging
            .iter()
            .map(|d| Place {
                name: d.name.clone(),
                pos: snap(self, d.pos),
                radius: 6.0,
                tag: String::new(),
            })
            .collect();
        self.areas = def
            .areas
            .iter()
            .map(|a| Place {
                name: a.name.clone(),
                pos: snap(self, a.pos),
                radius: a.radius.max(2.0),
                tag: a.tag.clone(),
            })
            .collect();
        let nodes: Vec<Vec2> = def
            .roads
            .nodes
            .iter()
            .map(|n| Vec2::new(n[0], n[1]))
            .collect();
        let mut start = vec![0u32; nodes.len() + 1];
        let mut list: Vec<(u32, u32, f32)> = Vec::new();
        for &[a, b] in &def.roads.edges {
            if a as usize >= nodes.len() || b as usize >= nodes.len() || a == b {
                continue;
            }
            let d = (nodes[a as usize] - nodes[b as usize]).len();
            list.push((a, b, d));
            list.push((b, a, d));
        }
        list.sort_unstable_by_key(|x| (x.0, x.1));
        list.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
        for &(a, _, _) in &list {
            start[a as usize + 1] += 1;
        }
        for i in 1..start.len() {
            start[i] += start[i - 1];
        }
        self.roads = Roads {
            nodes,
            start,
            adj: list.iter().map(|&(_, b, d)| (b, d)).collect(),
        };
    }

    pub fn entry(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }
    pub fn area(&self, name: &str) -> Option<&Place> {
        self.areas.iter().find(|e| e.name == name)
    }

    /// Bytes held by the derived structures.
    pub fn memory_bytes(&self) -> usize {
        self.tiles.len() * 4
            + self.tile_rc0.len()
            + self.mixed.len() * std::mem::size_of::<MixedTile>()
            + self.labels.len() * 256
            + self.seg_ax.len() * 16
            + self.wall_idx.len() * 4
            + self.rnodes.len() * std::mem::size_of::<RegionNode>()
            + self.radj.len() * 8
    }
}

/// Swap the roles of the two tiles in a link mask.
#[inline]
pub fn transpose_links(m: u16) -> u16 {
    let mut out = 0;
    for a in 0..4 {
        for b in 0..4 {
            if m & (1 << (a * 4 + b)) != 0 {
                out |= 1 << (b * 4 + a);
            }
        }
    }
    out
}

/// The box `[x0, x1, y0, y1]` around a segment, CLEARANCE wider on each side
/// plus slack: a point outside it is more than CLEARANCE from the segment,
/// so the exact clearance test (`closest_on_segment`, distance squared
/// `< CLEARANCE²`) fails for it. The slack (0.05 m) covers the rounding of
/// that test, whose closest point can leave the segment's own box only by a
/// few ulps of the coordinates (≤ 2 mm on an 8 km map), and of this box.
fn clear_box(s: &Seg) -> [f32; 4] {
    const R: f32 = CLEARANCE + 0.05;
    [
        s.a.x.min(s.b.x) - R,
        s.a.x.max(s.b.x) + R,
        s.a.y.min(s.b.y) - R,
        s.a.y.max(s.b.y) + R,
    ]
}

/// `seg_box_dist2(a, b, bmin, bmax) <= margin²`, skipping the exact test
/// when the segment's bounding box is more than `margin` plus 0.05 m from the
/// box along an axis. Then every distance that function takes is longer than
/// `margin` and none of its edge intersections can register: rounding moves
/// the points and intersections it computes by at most a few ulps of the
/// coordinates (≤ 5 mm on an 8 km map), well inside the slack.
#[inline]
fn seg_box_near(s: &Seg, bmin: Vec2, bmax: Vec2, margin: f32) -> bool {
    let g = margin + 0.05;
    if s.a.x.max(s.b.x) < bmin.x - g
        || s.a.x.min(s.b.x) > bmax.x + g
        || s.a.y.max(s.b.y) < bmin.y - g
        || s.a.y.min(s.b.y) > bmax.y + g
    {
        return false;
    }
    seg_box_dist2(s.a, s.b, bmin, bmax) <= margin * margin
}

/// Squared distance between a segment and an axis-aligned box.
fn seg_box_dist2(a: Vec2, b: Vec2, bmin: Vec2, bmax: Vec2) -> f32 {
    let inside = |p: Vec2| p.x >= bmin.x && p.x <= bmax.x && p.y >= bmin.y && p.y <= bmax.y;
    if inside(a) || inside(b) {
        return 0.0;
    }
    let corners = [
        bmin,
        Vec2::new(bmax.x, bmin.y),
        bmax,
        Vec2::new(bmin.x, bmax.y),
    ];
    let mut best = f32::INFINITY;
    for i in 0..4 {
        let (c0, c1) = (corners[i], corners[(i + 1) % 4]);
        if riot_core::math::segment_hit(a, b, c0, c1).is_some() {
            return 0.0;
        }
        // Closest distance between two segments: check the four endpoints.
        for (p, s0, s1) in [(a, c0, c1), (b, c0, c1), (c0, a, b), (c1, a, b)] {
            best = best.min((riot_core::math::closest_on_segment(p, s0, s1) - p).len2());
        }
    }
    best
}

/// 4-connected components of a tile bitmap, capped at `MAX_COMP`; cells of
/// further components are cleared. Components are numbered in the order of
/// their first cell (row-major), as a flood fill started from each unlabelled
/// cell in turn numbers them. Returns the count and, for 2+ components, the
/// label of every cell (0 where not walkable).
///
/// Works on the runs of walkable cells along each row: runs in adjacent rows
/// that share a column are connected. Union by minimum makes every root the
/// component's first run, so roots in run order are components in the order
/// of their first cell.
fn components(bits: &mut [u64; 4]) -> (u8, Option<[u8; 256]>) {
    // A 16-cell row has at most 8 runs.
    const MAX_RUNS: usize = (TILE * TILE / 2) as usize;
    fn find(p: &mut [u8; MAX_RUNS], mut x: u8) -> u8 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    let mut runs = [0u16; MAX_RUNS];
    let mut run_row = [0u8; MAX_RUNS];
    let mut parent = [0u8; MAX_RUNS];
    let mut n = 0usize;
    let mut prev = 0..0;
    for ly in 0..TILE as usize {
        let mut w = (bits[ly >> 2] >> ((ly & 3) * 16)) as u16;
        let first = n;
        while w != 0 {
            // The run starting at the lowest set bit (the carry clears it).
            let run = (w ^ w.wrapping_add(w.isolate_lowest_one())) & w;
            w &= !run;
            (runs[n], run_row[n], parent[n]) = (run, ly as u8, n as u8);
            for j in prev.clone() {
                if runs[j] & run != 0 {
                    let (a, b) = (find(&mut parent, j as u8), find(&mut parent, n as u8));
                    parent[a.max(b) as usize] = a.min(b);
                }
            }
            n += 1;
        }
        prev = first..n;
    }
    let mut comp = [0u8; MAX_RUNS];
    let mut count = 0usize;
    for r in 0..n {
        let root = find(&mut parent, r as u8) as usize;
        if root == r {
            comp[r] = count.min(MAX_COMP) as u8;
            count += 1;
        } else {
            comp[r] = comp[root];
        }
    }
    let ncomp = count.min(MAX_COMP);
    let mut labels = [0u8; 256];
    for r in 0..n {
        let ly = run_row[r] as usize;
        let c = comp[r];
        if c as usize == MAX_COMP {
            // Too many pockets: drop this one.
            bits[ly >> 2] &= !((runs[r] as u64) << ((ly & 3) * 16));
        } else if c > 0 {
            let lx = runs[r].trailing_zeros() as usize;
            let len = runs[r].count_ones() as usize;
            labels[ly * 16 + lx..][..len].fill(c);
        }
    }
    (ncomp as u8, (ncomp > 1).then_some(labels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use riot_defs::mapdef::{Polygon, SolidKind};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon {
        Polygon {
            kind: SolidKind::Building,
            points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        }
    }

    fn test_map() -> MapDef {
        MapDef {
            name: "test".into(),
            width: 200.0,
            height: 100.0,
            // A building and a wall with a 0.9 m doorway between x = 0 and 0.9.
            solids: vec![
                rect(10.0, 10.0, 30.0, 30.0),
                rect(-40.0, -20.0, 0.0, -19.7),
                rect(0.9, -20.0, 40.0, -19.7),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn raster_matches_polygons() {
        let m = Map::build(&test_map(), 0.45).unwrap();
        assert_eq!(m.size, 256.0);
        assert!(m.walkable_at(Vec2::new(0.0, 0.0)));
        assert!(!m.walkable_at(Vec2::new(20.0, 20.0)), "inside building");
        assert!(!m.walkable_at(Vec2::new(10.1, 20.0)), "too close to wall");
        assert!(m.walkable_at(Vec2::new(9.5, 20.0)));
        assert!(!m.walkable_at(Vec2::new(-101.0, 0.0)), "outside map");
        assert!(!m.walkable_at(Vec2::new(0.0, 51.0)), "outside map");
        // The doorway keeps at least one open cell.
        let door = (0..4).any(|k| m.walkable_at(Vec2::new(0.25 + k as f32 * 0.1, -19.85)));
        assert!(door, "doorway closed");
        assert!(!m.walkable_at(Vec2::new(-10.0, -19.85)));
        assert!(!m.walls_near(Vec2::new(9.8, 20.0)).is_empty());
        assert!(m.walls_near(Vec2::new(-60.0, 30.0)).is_empty());
        assert!(
            m.ray_blocked(Vec2::new(0.0, 20.0), Vec2::new(40.0, 20.0))
                .is_some()
        );
        assert!(
            m.ray_blocked(Vec2::new(0.0, 0.0), Vec2::new(40.0, 0.0))
                .is_none()
        );
    }

    #[test]
    fn overlapping_polygons_stay_solid() {
        let mut def = test_map();
        def.solids.push(rect(20.0, 20.0, 50.0, 40.0));
        let m = Map::build(&def, 0.45).unwrap();
        assert!(!m.walkable_at(Vec2::new(25.0, 25.0)));
        assert!(!m.walkable_at(Vec2::new(45.0, 35.0)));
    }

    #[test]
    fn regions_split_by_walls() {
        // A wall cutting the map in two, except for a gap.
        let mut def = MapDef {
            name: "split".into(),
            width: 256.0,
            height: 256.0,
            ..Default::default()
        };
        def.solids.push(rect(-1.0, -128.0, 1.0, 100.0));
        let m = Map::build(&def, 0.45).unwrap();
        let node = |p: Vec2| {
            let (cx, cy) = m.cell_of(p).unwrap();
            let t = m.tile_index(cx >> 4, cy >> 4);
            m.rnode_of(t, m.comp_of(cx, cy).unwrap()).unwrap()
        };
        let a = node(Vec2::new(-20.0, 0.0));
        let b = node(Vec2::new(20.0, 0.0));
        assert_ne!(a, b);
        // Walk the region graph from a: b must be reachable through the gap.
        let mut seen = vec![false; m.rnodes.len()];
        let mut stack = vec![a];
        while let Some(n) = stack.pop() {
            if std::mem::replace(&mut seen[n as usize], true) {
                continue;
            }
            stack.extend(m.region_neighbours(n).iter().map(|e| e.0));
        }
        assert!(seen[b as usize]);
    }

    #[test]
    fn components_cap() {
        let mut bits = [0u64; 4];
        // Five isolated cells.
        for i in [0usize, 2, 4, 6, 8] {
            bits[0] |= 1 << i;
        }
        let (n, labels) = components(&mut bits);
        assert_eq!(n, 4);
        assert_eq!(labels.unwrap()[6], 3);
        assert_eq!(bits[0] & (1 << 8), 0);
    }

    /// The original flood fill, which `components` must agree with.
    fn components_flood(bits: &mut [u64; 4]) -> (u8, [u8; 256]) {
        let mut labels = [u8::MAX; 256];
        let get = |bits: &[u64; 4], i: usize| (bits[i >> 6] >> (i & 63)) & 1 != 0;
        let mut n = 0u8;
        let mut stack = [0u16; 256];
        for start in 0..256usize {
            if !get(bits, start) || labels[start] != u8::MAX {
                continue;
            }
            let label = n;
            let keep = (n as usize) < MAX_COMP;
            let mut sp = 0;
            stack[sp] = start as u16;
            sp += 1;
            labels[start] = label;
            while sp > 0 {
                sp -= 1;
                let c = stack[sp] as usize;
                let (x, y) = (c & 15, c >> 4);
                let mut push = |nc: usize, stack: &mut [u16; 256], sp: &mut usize| {
                    if get(bits, nc) && labels[nc] == u8::MAX {
                        labels[nc] = label;
                        stack[*sp] = nc as u16;
                        *sp += 1;
                    }
                };
                if x > 0 {
                    push(c - 1, &mut stack, &mut sp);
                }
                if x < 15 {
                    push(c + 1, &mut stack, &mut sp);
                }
                if y > 0 {
                    push(c - 16, &mut stack, &mut sp);
                }
                if y < 15 {
                    push(c + 16, &mut stack, &mut sp);
                }
            }
            if keep {
                n += 1;
            } else {
                // Too many pockets: drop this one.
                for i in 0..256 {
                    if labels[i] == label {
                        bits[i >> 6] &= !(1 << (i & 63));
                        labels[i] = u8::MAX - 1;
                    }
                }
            }
        }
        for l in labels.iter_mut() {
            if *l >= MAX_COMP as u8 {
                *l = 0;
            }
        }
        (n, labels)
    }

    #[test]
    fn components_match_flood_fill() {
        let mut seed = 7u64;
        let mut next = || {
            seed = riot_core::mix(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
            seed
        };
        for case in 0..20_000 {
            // Random cells at various densities, sometimes cut by a wall.
            let density = [16u64, 96, 128, 160, 200, 240][case % 6];
            let mut bits = [0u64; 4];
            for i in 0..256 {
                if next() % 256 < density {
                    bits[i >> 6] |= 1 << (i & 63);
                }
            }
            if case % 3 == 0 {
                let col = (next() % 16) as usize;
                for ly in 0..16 {
                    if next() % 8 != 0 {
                        let i = ly * 16 + col;
                        bits[i >> 6] &= !(1 << (i & 63));
                    }
                }
            }
            let (mut a, mut b) = (bits, bits);
            let (n, labels) = components(&mut a);
            let (n0, labels0) = components_flood(&mut b);
            assert_eq!((n, a), (n0, b), "case {case}");
            assert_eq!(labels.unwrap_or([0; 256]), labels0, "case {case}");
        }
    }

    /// Where the cheap box tests say "too far", the exact tests agree: just
    /// past the slack, far from the origin, for long and nearly
    /// axis-parallel segments.
    #[test]
    fn box_rejects_are_exact() {
        let mut seed = 11u64;
        let mut next = || {
            seed = riot_core::mix(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let (mut boxes_skipped, mut cells_skipped) = (0, 0);
        for case in 0..200_000 {
            let c = Vec2::new(next() * 8000.0 - 4000.0, next() * 8000.0 - 4000.0);
            let len = [0.01, 0.5, 4.0, 30.0, 500.0, 8000.0][case % 6] * (0.5 + next());
            let angle = if case % 3 == 0 {
                (case % 4) as f32 * std::f32::consts::FRAC_PI_2 + (next() - 0.5) * 1e-4
            } else {
                next() * std::f32::consts::TAU
            };
            let seg = Seg {
                a: c,
                b: c + Vec2::new(angle.cos(), angle.sin()) * len,
            };
            let (lo, hi) = (
                Vec2::new(seg.a.x.min(seg.b.x), seg.a.y.min(seg.b.y)),
                Vec2::new(seg.a.x.max(seg.b.x), seg.a.y.max(seg.b.y)),
            );
            // A 4 m box just past the segment's box plus margin and slack,
            // on a random side, slid along that side.
            let margin = [0.45, 0.47, 0.6][case % 3];
            let gap = margin + 0.05 + next() * 0.02;
            let slide = |l: f32, h: f32, t: f32| l - 4.0 + (h - l + 8.0) * t;
            let bmin = match case % 4 {
                0 => Vec2::new(hi.x + gap, slide(lo.y, hi.y, next())),
                1 => Vec2::new(lo.x - gap - 4.0, slide(lo.y, hi.y, next())),
                2 => Vec2::new(slide(lo.x, hi.x, next()), hi.y + gap),
                _ => Vec2::new(slide(lo.x, hi.x, next()), lo.y - gap - 4.0),
            };
            let bmax = bmin + Vec2::new(4.0, 4.0);
            let near = seg_box_near(&seg, bmin, bmax, margin);
            assert_eq!(
                near,
                seg_box_dist2(seg.a, seg.b, bmin, bmax) <= margin * margin,
                "case {case}"
            );
            boxes_skipped += !near as u32;
            // A cell centre just outside the clearance box.
            let b = clear_box(&seg);
            let e = next() * 0.01;
            let (u, v) = (next(), next());
            let p = match case % 4 {
                0 => Vec2::new(b[1] + e, b[2] + (b[3] - b[2]) * u),
                1 => Vec2::new(b[0] - e, b[2] + (b[3] - b[2]) * u),
                2 => Vec2::new(b[0] + (b[1] - b[0]) * v, b[3] + e),
                _ => Vec2::new(b[0] + (b[1] - b[0]) * v, b[2] - e),
            };
            if p.x < b[0] || p.x > b[1] || p.y < b[2] || p.y > b[3] {
                cells_skipped += 1;
                let d2 = (riot_core::math::closest_on_segment(p, seg.a, seg.b) - p).len2();
                assert!(d2 >= CLEARANCE * CLEARANCE, "case {case}: {d2}");
            }
        }
        assert!(boxes_skipped > 150_000 && cells_skipped > 150_000);
    }
}
