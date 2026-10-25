//! Procedural city generator (DESIGN §2.1). It writes a Tiled JSON map that
//! follows the convention in `data/maps/README.md`, so a generated city loads
//! through [`riot_data_tiled`] like a hand-made map and opens in Tiled for
//! editing.
//!
//! The layout, coarse to fine:
//! 1. **Street lines** along each axis: an avenue through the centre and
//!    every few blocks, streets and narrow lanes between them.
//! 2. **Groups** of the cells between lines: the government quarter with its
//!    square at the centre, a river band, squares, parks, police depots,
//!    superblocks (some with a cul-de-sac stub) and plain blocks. A group's
//!    inner street segments are dropped and the streets around every group
//!    stay, so the street network is connected by construction; the river
//!    band keeps only its bridges.
//! 3. **Blocks** are lined with buildings around a courtyard: rectangles,
//!    L-shaped corners, 2–4 m alleys between some of them, arcades facing
//!    squares and avenues.
//! 4. **Markers** (entries, depots, staging points, areas), props, trees, the
//!    road graph along every street centreline and the 4 m ground layer.
//!
//! All randomness comes from a small counter-based hash RNG with one stream
//! per purpose, and trigonometry from [`riot_data_core::math`], so a seed
//! gives the same city on every machine. Coordinates here are metres from the
//! map's top-left corner, y down; they become Tiled pixels on output.
//!
//! The generator itself is plain code: it fills in `riot_data_tiled`'s file
//! model, and that crate writes the JSON and converts the model to a
//! `MapDef`.

use riot_defs::mapdef::{Ground, MapDef, PropKind, SolidKind};

use riot_data_core::math::{point_in_polygon, seg_dist2, sin, sin_cos_deg};
use riot_data_tiled::{
    self as tiled, Layer, Object, Property, Pt, TileDef, TiledMap, Tileset, serde_json,
};

/// Ground tile size in metres.
pub const TILE_M: f32 = 4.0;
/// Metres per Tiled pixel in generated maps.
pub const METRES_PER_PIXEL: f64 = 0.25;
const TILE_PX: u32 = (TILE_M as f64 / METRES_PER_PIXEL) as u32;
/// Ground kinds; also the tile ids of the ground tileset.
const PAVEMENT: u8 = Ground::Pavement as u8;
const ROAD: u8 = Ground::Road as u8;
const GRASS: u8 = Ground::Grass as u8;
const PLAZA: u8 = Ground::Plaza as u8;
const WATER: u8 = Ground::Water as u8;
/// Tile property values, in tile id order.
const GROUND_NAMES: [&str; 5] = ["pavement", "road", "grass", "plaza", "water"];

/// What to generate.
#[derive(Clone, Debug, PartialEq)]
pub struct CityParams {
    /// Side of the square map in metres, 512–8192 (rounded to the 4 m tile).
    pub size_m: f32,
    pub seed: u64,
    /// Mean distance between street centrelines (60–300 m).
    pub block_m: f32,
    /// Mean number of blocks between avenues.
    pub avenue_every: u32,
    /// A river across the city, with bridges.
    pub river: bool,
    /// Most props (parked cars, bins, benches, barriers); the 8 km city
    /// reaches 6000, smaller maps get proportionally fewer.
    pub max_props: u32,
    /// zlib-compress the ground layer (Tiled reads both; an 8 km city's
    /// ground is 4 M tiles, 8 MB as a plain array).
    pub compress_ground: bool,
}

impl CityParams {
    pub fn new(size_m: f32, seed: u64) -> Self {
        CityParams {
            size_m,
            seed,
            ..Default::default()
        }
    }

    /// The map's side in metres after clamping and rounding.
    pub fn side(&self) -> f32 {
        (self.size_m.clamp(512.0, 8192.0) / TILE_M).round() * TILE_M
    }

    /// The name a generated map gets: its scenario pseudo-path.
    pub fn map_name(&self) -> String {
        format!("generated:city?size={}&seed={}", self.side(), self.seed)
    }
}

impl Default for CityParams {
    fn default() -> Self {
        CityParams {
            size_m: 8192.0,
            seed: 1,
            block_m: 100.0,
            avenue_every: 4,
            river: true,
            max_props: 6000,
            compress_ground: true,
        }
    }
}

/// The city as a Tiled JSON map.
pub fn generate(params: &CityParams) -> serde_json::Value {
    generate_tiled(params)
        .to_json_value()
        .expect("a generated map serializes")
}

/// The city as compact Tiled JSON text, ready to write to a `.json` file.
pub fn generate_json(params: &CityParams) -> String {
    generate_tiled(params)
        .to_json()
        .expect("a generated map serializes")
}

/// The city as a `MapDef`, exactly as loading [`generate`]'s JSON would give.
pub fn generate_mapdef(params: &CityParams) -> MapDef {
    tiled::convert(&generate_tiled(params), &params.map_name(), None)
        .expect("generated maps follow the Tiled convention")
}

fn generate_tiled(params: &CityParams) -> TiledMap {
    City::build(params).into_tiled(params)
}

// ---------------------------------------------------------------------------
// Randomness

/// SplitMix64 finalizer.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A SplitMix64 stream. One per purpose, so changing how one part of the
/// city is drawn does not reshuffle the others.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, stream: u64) -> Rng {
        Rng(mix(seed ^ mix(stream.wrapping_mul(0x9e37_79b9_7f4a_7c15))))
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 * (1.0 / 16_777_216.0)
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
    /// Uniform in `a..=b`.
    fn int(&mut self, a: i32, b: i32) -> i32 {
        a + (self.next() % (b - a + 1) as u64) as i32
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

fn snap(x: f32, q: f32) -> f32 {
    (x / q).round() * q
}

// ---------------------------------------------------------------------------
// Geometry

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Avenue,
    Street,
    Lane,
}

impl Kind {
    /// Right of way: carriageway plus both pavements.
    fn width(self) -> f32 {
        match self {
            Kind::Avenue => 32.0,
            Kind::Street => 16.0,
            Kind::Lane => 12.0,
        }
    }
    fn carriageway(self) -> f32 {
        match self {
            Kind::Avenue => 16.0,
            Kind::Street => 8.0,
            Kind::Lane => 4.0,
        }
    }
}

/// A street centreline across the whole map.
#[derive(Clone, Copy, Debug)]
struct Line {
    pos: f32,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }
    fn w(&self) -> f32 {
        self.x1 - self.x0
    }
    fn h(&self) -> f32 {
        self.y1 - self.y0
    }
    fn centre(&self) -> [f32; 2] {
        [(self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5]
    }
    fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x0 + d, self.y0 + d, self.x1 - d, self.y1 - d)
    }
    /// The parts of `self` outside `o`.
    fn minus(&self, o: &Rect) -> Vec<Rect> {
        if !self.overlaps(o) {
            return vec![*self];
        }
        let mut out = Vec::new();
        let (y0, y1) = (self.y0.max(o.y0), self.y1.min(o.y1));
        if o.y0 > self.y0 {
            out.push(Rect::new(self.x0, self.y0, self.x1, o.y0));
        }
        if o.y1 < self.y1 {
            out.push(Rect::new(self.x0, o.y1, self.x1, self.y1));
        }
        if o.x0 > self.x0 {
            out.push(Rect::new(self.x0, y0, o.x0, y1));
        }
        if o.x1 < self.x1 {
            out.push(Rect::new(o.x1, y0, self.x1, y1));
        }
        out
    }
    fn points(&self) -> Vec<[f32; 2]> {
        vec![
            [self.x0, self.y0],
            [self.x1, self.y0],
            [self.x1, self.y1],
            [self.x0, self.y1],
        ]
    }
}

#[derive(Clone, Debug)]
enum Shape {
    Rect(Rect),
    Poly(Vec<[f32; 2]>),
    /// Centre and radius.
    Circle([f32; 2], f32),
}

impl Shape {
    fn bbox(&self) -> Rect {
        match self {
            Shape::Rect(r) => *r,
            Shape::Poly(p) => {
                p.iter()
                    .fold(Rect::new(f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, q| {
                        Rect::new(
                            b.x0.min(q[0]),
                            b.y0.min(q[1]),
                            b.x1.max(q[0]),
                            b.y1.max(q[1]),
                        )
                    })
            }
            Shape::Circle(c, r) => Rect::new(c[0] - r, c[1] - r, c[0] + r, c[1] + r),
        }
    }
    /// Does the shape come within `r` of `p`?
    fn near(&self, p: [f32; 2], r: f32) -> bool {
        match self {
            Shape::Circle(c, cr) => {
                let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
                dx * dx + dy * dy < (r + cr) * (r + cr)
            }
            Shape::Rect(rc) => poly_near(&rc.points(), p, r),
            Shape::Poly(pts) => poly_near(pts, p, r),
        }
    }
}

fn poly_near(pts: &[[f32; 2]], p: [f32; 2], r: f32) -> bool {
    point_in_polygon(p, pts)
        || (0..pts.len()).any(|i| seg_dist2(p, pts[i], pts[(i + 1) % pts.len()]) < r * r)
}

struct Solid {
    kind: SolidKind,
    shape: Shape,
}

/// Render-only polygon: `canopy` or `arcade`.
struct Overhead {
    kind: &'static str,
    shape: Shape,
}

struct Prop {
    kind: PropKind,
    centre: [f32; 2],
    half: [f32; 2],
    /// Clockwise degrees: 0 along x, 90 along y.
    deg: f32,
}

/// An entry, depot, staging point or area. `kind` is the entry type or the
/// area tag; `radius` is 0 for depots and staging points.
struct Marker {
    name: String,
    kind: &'static str,
    pos: [f32; 2],
    radius: f32,
}

#[derive(Clone, Debug, PartialEq)]
enum Use {
    Buildings,
    Square(String),
    GovSquare,
    Government,
    Park(String),
    Depot(String),
    River,
}

/// A rectangle of cells `i0..=i1` × `j0..=j1` with one use.
struct Group {
    i0: usize,
    i1: usize,
    j0: usize,
    j1: usize,
    use_: Use,
    stub: Option<Stub>,
}

/// A dead-end street into a superblock.
#[derive(Clone, Copy, Debug)]
struct Stub {
    /// Centreline from the street it leaves to its end.
    a: [f32; 2],
    b: [f32; 2],
    /// Its right of way, plus turning room past the end.
    corridor: Rect,
    kind: Kind,
    /// The street at `a`.
    from: Kind,
}

struct River {
    /// Cell row of the band.
    row: usize,
    mid: f32,
    width: f32,
    amp: f32,
    waves: [(f32, f32, f32); 2],
    /// x-lines with a bridge.
    bridges: Vec<usize>,
}

impl River {
    fn centre(&self, x: f32) -> f32 {
        let mut c = self.mid;
        for &(a, l, p) in &self.waves {
            c += self.amp * a * sin((x / l + p) as f64 * std::f64::consts::TAU) as f32;
        }
        c
    }
}

const NONE: usize = usize::MAX;

/// A building: its rectangles, its outline when it is not one rectangle
/// (an L), and the roof of its arcade if it has one.
type Bld = (Vec<Rect>, Option<Vec<[f32; 2]>>, Option<Rect>);

struct City {
    s: f32,
    seed: u64,
    xs: Vec<Line>,
    ys: Vec<Line>,
    /// `vkeep[k][j]`: the segment of x-line `k` across cell row `j` is a street.
    vkeep: Vec<Vec<bool>>,
    /// `hkeep[m][i]`: the segment of y-line `m` across cell column `i`.
    hkeep: Vec<Vec<bool>>,
    /// `cell[j][i]` → group.
    cell: Vec<Vec<usize>>,
    groups: Vec<Group>,
    river: Option<River>,
    solids: Vec<Solid>,
    overhead: Vec<Overhead>,
    props: Vec<Prop>,
    /// Props kept regardless of the budget (barriers, benches).
    fixed_props: usize,
    roads: Vec<Vec<[f32; 2]>>,
    entries: Vec<Marker>,
    depots: Vec<Marker>,
    staging: Vec<Marker>,
    areas: Vec<Marker>,
    tiles: usize,
    ground: Vec<u8>,
}

/// Street lines along one axis, snapped to the 4 m tile grid.
fn street_lines(s: f32, block: f32, every: u32, rng: &mut Rng) -> Vec<Line> {
    let mut v = Vec::new();
    let mut pos = snap(block * rng.range(0.45, 0.7), TILE_M);
    while pos < s - block * 0.35 {
        v.push(Line {
            pos,
            kind: Kind::Street,
        });
        pos += snap(block * rng.range(0.8, 1.2), TILE_M);
    }
    let c = nearest_line(&v, s * 0.5);
    v[c].kind = Kind::Avenue;
    let every = every.max(2) as i32;
    let mut k = c as i32;
    loop {
        k += (every + rng.int(-1, 1)).max(2);
        if k as usize >= v.len() {
            break;
        }
        v[k as usize].kind = Kind::Avenue;
    }
    let mut k = c as i32;
    loop {
        k -= (every + rng.int(-1, 1)).max(2);
        if k < 0 {
            break;
        }
        v[k as usize].kind = Kind::Avenue;
    }
    for l in &mut v {
        if l.kind == Kind::Street && rng.chance(0.2) {
            l.kind = Kind::Lane;
        }
    }
    v
}

fn nearest_line(v: &[Line], at: f32) -> usize {
    (0..v.len())
        .min_by(|&a, &b| (v[a].pos - at).abs().total_cmp(&(v[b].pos - at).abs()))
        .unwrap_or(0)
}

impl City {
    fn build(p: &CityParams) -> City {
        let s = p.side();
        let block = p.block_m.clamp(60.0, 300.0).min(s / 5.0);
        let xs = street_lines(s, block, p.avenue_every, &mut Rng::new(p.seed, 1));
        let mut ys = street_lines(s, block, p.avenue_every, &mut Rng::new(p.seed, 2));
        let mc = nearest_line(&ys, s * 0.5);
        let sq_rows = if s >= 2000.0 { 2 } else { 1 };
        let river = if p.river {
            place_river(s, block, &mut ys, mc, sq_rows, &mut Rng::new(p.seed, 3))
        } else {
            None
        };
        let (nx, ny) = (xs.len(), ys.len());
        let tiles = (s / TILE_M) as usize;
        let mut c = City {
            s,
            seed: p.seed,
            vkeep: vec![vec![true; ny + 1]; nx],
            hkeep: vec![vec![true; nx + 1]; ny],
            cell: vec![vec![NONE; nx + 1]; ny + 1],
            xs,
            ys,
            groups: Vec::new(),
            river,
            solids: Vec::new(),
            overhead: Vec::new(),
            props: Vec::new(),
            fixed_props: 0,
            roads: Vec::new(),
            entries: Vec::new(),
            depots: Vec::new(),
            staging: Vec::new(),
            areas: Vec::new(),
            tiles,
            ground: vec![PAVEMENT; tiles * tiles],
        };
        if let Some(r) = &mut c.river {
            r.bridges = choose_bridges(&c.xs, &mut Rng::new(p.seed, 8));
        }
        c.place_groups(mc, sq_rows);
        c.fill();
        c.markers();
        c.clear_markers();
        c.street_props(p);
        c.road_graph();
        c.paint_streets();
        c
    }

    // --- cells -----------------------------------------------------------

    fn nx(&self) -> usize {
        self.xs.len()
    }
    fn ny(&self) -> usize {
        self.ys.len()
    }
    /// Centreline west of cell column `i` (the map edge for the first).
    fn xl(&self, i: usize) -> f32 {
        if i == 0 { 0.0 } else { self.xs[i - 1].pos }
    }
    fn xr(&self, i: usize) -> f32 {
        if i == self.nx() {
            self.s
        } else {
            self.xs[i].pos
        }
    }
    fn yl(&self, j: usize) -> f32 {
        if j == 0 { 0.0 } else { self.ys[j - 1].pos }
    }
    fn yr(&self, j: usize) -> f32 {
        if j == self.ny() {
            self.s
        } else {
            self.ys[j].pos
        }
    }
    /// The block of a group: its cells inside the streets around them.
    fn block(&self, g: &Group) -> Rect {
        let half = |l: &Line| l.kind.width() * 0.5;
        Rect::new(
            if g.i0 == 0 {
                0.0
            } else {
                self.xs[g.i0 - 1].pos + half(&self.xs[g.i0 - 1])
            },
            if g.j0 == 0 {
                0.0
            } else {
                self.ys[g.j0 - 1].pos + half(&self.ys[g.j0 - 1])
            },
            if g.i1 == self.nx() {
                self.s
            } else {
                self.xs[g.i1].pos - half(&self.xs[g.i1])
            },
            if g.j1 == self.ny() {
                self.s
            } else {
                self.ys[g.j1].pos - half(&self.ys[g.j1])
            },
        )
    }
    fn interior(&self, i: usize, j: usize) -> bool {
        i >= 1 && j >= 1 && i < self.nx() && j < self.ny()
    }
    fn free(&self, i: usize, j: usize) -> bool {
        self.interior(i, j) && self.cell[j][i] == NONE
    }

    /// Claims cells `i0..i0+w` × `j0..j0+h` for a group and drops the
    /// streets inside it. False if any cell is taken or on the map edge.
    fn claim(&mut self, i0: usize, j0: usize, w: usize, h: usize, use_: Use) -> bool {
        let (i1, j1) = (i0 + w - 1, j0 + h - 1);
        if (j0..=j1).any(|j| (i0..=i1).any(|i| !self.free(i, j))) {
            return false;
        }
        self.add_group(i0, i1, j0, j1, use_);
        true
    }

    fn add_group(&mut self, i0: usize, i1: usize, j0: usize, j1: usize, use_: Use) {
        let g = self.groups.len();
        for j in j0..=j1 {
            for i in i0..=i1 {
                self.cell[j][i] = g;
            }
        }
        for k in i0..i1 {
            for j in j0..=j1 {
                self.vkeep[k][j] = false;
            }
        }
        for m in j0..j1 {
            for i in i0..=i1 {
                self.hkeep[m][i] = false;
            }
        }
        self.groups.push(Group {
            i0,
            i1,
            j0,
            j1,
            use_,
            stub: None,
        });
    }

    fn group_centre(&self, g: &Group) -> [f32; 2] {
        [
            (self.xl(g.i0) + self.xr(g.i1)) * 0.5,
            (self.yl(g.j0) + self.yr(g.j1)) * 0.5,
        ]
    }

    // --- groups ----------------------------------------------------------

    fn place_groups(&mut self, mc: usize, sq_rows: usize) {
        let mut rng = Rng::new(self.seed, 4);
        let s = self.s;
        // The river band: one group across the whole map; only bridges cross.
        if let Some(r) = &self.river {
            let (row, bridges) = (r.row, r.bridges.clone());
            self.add_group(0, self.nx(), row, row, Use::River);
            for k in bridges {
                self.vkeep[k][row] = true;
            }
        }
        // The government quarter north of the central avenue crossing, its
        // square south of it.
        let kc = nearest_line(&self.xs, s * 0.5);
        let ok = self.claim(kc, mc, 2, 1, Use::Government);
        assert!(ok, "no room for the government quarter");
        let mut rows = sq_rows;
        while !self.claim(kc, mc + 1, 2, rows, Use::GovSquare) {
            rows -= 1;
            assert!(rows > 0, "no room for the central square");
        }
        // Squares and parks, spread out.
        let km2 = (s / 1000.0) * (s / 1000.0);
        let n_sq = ((km2 * 0.5).round() as usize).clamp(2, 40);
        let n_park = ((km2 * 0.35).round() as usize).clamp(1, 30);
        let mut spacing = 0.6 * (s * s / (n_sq + n_park + 2) as f32).sqrt();
        let mut cands: Vec<(usize, usize)> = (1..self.ny())
            .flat_map(|j| (1..self.nx()).map(move |i| (i, j)))
            .collect();
        rng.shuffle(&mut cands);
        let (mut squares, mut parks) = (0, 0);
        for _attempt in 0..3 {
            for &(i, j) in &cands {
                if squares >= n_sq && parks >= n_park {
                    break;
                }
                let park = parks < n_park && (squares >= n_sq || rng.chance(0.45));
                let (w, h) = if park {
                    match rng.int(0, 9) {
                        0..=3 => (1, 1),
                        4..=5 => (2, 1),
                        6..=7 => (1, 2),
                        _ => (2, 2),
                    }
                } else {
                    match rng.int(0, 9) {
                        0..=6 => (1, 1),
                        7 => (2, 1),
                        _ => (1, 2),
                    }
                };
                let fits = |c: &City, w: usize, h: usize| {
                    (j..j + h).all(|jj| (i..i + w).all(|ii| c.free(ii, jj)))
                };
                let (w, h) = if fits(self, w, h) { (w, h) } else { (1, 1) };
                if !fits(self, w, h) {
                    continue;
                }
                let centre = [
                    (self.xl(i) + self.xr(i + w - 1)) * 0.5,
                    (self.yl(j) + self.yr(j + h - 1)) * 0.5,
                ];
                let far = self.groups.iter().all(|g| {
                    matches!(g.use_, Use::Buildings | Use::River) || {
                        let c = self.group_centre(g);
                        (c[0] - centre[0]).hypot(c[1] - centre[1]) >= spacing
                    }
                });
                if !far {
                    continue;
                }
                let use_ = if park {
                    parks += 1;
                    Use::Park(String::new())
                } else {
                    squares += 1;
                    Use::Square(String::new())
                };
                self.claim(i, j, w, h, use_);
            }
            spacing *= 0.7;
        }
        // Police depots on a ring at the outskirts.
        let n_depots = ((s / 1600.0).round() as usize + 1).clamp(2, 6);
        let phase = rng.range(0.0, 360.0);
        let mut used_names: Vec<String> = Vec::new();
        for n in 0..n_depots {
            let deg = phase + 360.0 * n as f32 / n_depots as f32;
            let (sn, cs) = sin_cos_deg(deg as f64);
            let target = [s * 0.5 + 0.4 * s * cs as f32, s * 0.5 + 0.4 * s * sn as f32];
            // The free block nearest the target; on a crowded small map,
            // a block on the map edge.
            let nearest = |edge_ok: bool| {
                (0..=self.ny())
                    .flat_map(|j| (0..=self.nx()).map(move |i| (i, j)))
                    .filter(|&(i, j)| self.cell[j][i] == NONE && (edge_ok || self.interior(i, j)))
                    .min_by(|a, b| {
                        let d = |&(i, j): &(usize, usize)| {
                            let c = [
                                (self.xl(i) + self.xr(i)) * 0.5,
                                (self.yl(j) + self.yr(j)) * 0.5,
                            ];
                            (c[0] - target[0]).hypot(c[1] - target[1])
                        };
                        d(a).total_cmp(&d(b))
                    })
            };
            let Some((i, j)) = nearest(false).or_else(|| nearest(true)) else {
                break;
            };
            let c = [
                (self.xl(i) + self.xr(i)) * 0.5,
                (self.yl(j) + self.yr(j)) * 0.5,
            ];
            let mut name = format!("depot_{}", compass(c, s));
            if used_names.contains(&name) {
                name = format!("{name}_{}", n + 1);
            }
            used_names.push(name.clone());
            self.add_group(i, i, j, j, Use::Depot(name));
        }
        // Superblocks, some with a cul-de-sac.
        for j in 1..self.ny() {
            for i in 1..self.nx() {
                if !self.free(i, j) {
                    continue;
                }
                let r = rng.f();
                let horizontal = r < 0.1 && self.xs[i].kind != Kind::Avenue && self.free(i + 1, j);
                let vertical = !horizontal
                    && r < 0.16
                    && self.ys[j].kind != Kind::Avenue
                    && self.free(i, j + 1);
                if !(horizontal || vertical) {
                    continue;
                }
                let (w, h) = if horizontal { (2, 1) } else { (1, 2) };
                self.claim(i, j, w, h, Use::Buildings);
                if rng.chance(0.5) {
                    let g = self.groups.len() - 1;
                    let from_start = rng.chance(0.5);
                    let stub = if horizontal {
                        let (l, a, b) = (self.xs[i], self.yl(j), self.yr(j));
                        let len = snap((b - a) * 0.45, TILE_M);
                        let (y0, y1) = if from_start {
                            (a, a + len)
                        } else {
                            (b, b - len)
                        };
                        let from = if from_start {
                            self.ys[j - 1].kind
                        } else {
                            self.ys[j].kind
                        };
                        let hw = l.kind.width() * 0.5;
                        let corridor =
                            Rect::new(l.pos - hw, y0.min(y1) - hw, l.pos + hw, y0.max(y1) + hw);
                        Stub {
                            a: [l.pos, y0],
                            b: [l.pos, y1],
                            corridor,
                            kind: l.kind,
                            from,
                        }
                    } else {
                        let (l, a, b) = (self.ys[j], self.xl(i), self.xr(i));
                        let len = snap((b - a) * 0.45, TILE_M);
                        let (x0, x1) = if from_start {
                            (a, a + len)
                        } else {
                            (b, b - len)
                        };
                        let from = if from_start {
                            self.xs[i - 1].kind
                        } else {
                            self.xs[i].kind
                        };
                        let hw = l.kind.width() * 0.5;
                        let corridor =
                            Rect::new(x0.min(x1) - hw, l.pos - hw, x0.max(x1) + hw, l.pos + hw);
                        Stub {
                            a: [x0, l.pos],
                            b: [x1, l.pos],
                            corridor,
                            kind: l.kind,
                            from,
                        }
                    };
                    self.groups[g].stub = Some(stub);
                }
            }
        }
        // Everything else, edge cells included, is a plain block.
        for j in 0..=self.ny() {
            for i in 0..=self.nx() {
                if self.cell[j][i] == NONE {
                    self.add_group(i, i, j, j, Use::Buildings);
                }
            }
        }
        // Name squares and parks by distance from the centre.
        for (is_park, prefix) in [(false, "square"), (true, "park")] {
            let mut ids: Vec<usize> = (0..self.groups.len())
                .filter(|&g| match self.groups[g].use_ {
                    Use::Park(_) => is_park,
                    Use::Square(_) => !is_park,
                    _ => false,
                })
                .collect();
            ids.sort_by(|&a, &b| {
                let d = |g: usize| {
                    let c = self.group_centre(&self.groups[g]);
                    (c[0] - s * 0.5).hypot(c[1] - s * 0.5)
                };
                d(a).total_cmp(&d(b)).then(a.cmp(&b))
            });
            for (n, g) in ids.into_iter().enumerate() {
                let name = format!("{prefix}_{}", n + 1);
                self.groups[g].use_ = if is_park {
                    Use::Park(name)
                } else {
                    Use::Square(name)
                };
            }
        }
    }

    // --- block contents --------------------------------------------------

    fn fill(&mut self) {
        let mut rng = Rng::new(self.seed, 5);
        for g in 0..self.groups.len() {
            let use_ = self.groups[g].use_.clone();
            let b = self.block(&self.groups[g]);
            match use_ {
                Use::Buildings => self.fill_buildings(g, b, &mut rng),
                Use::Square(name) => self.fill_square(b, &name, &mut rng),
                Use::GovSquare => self.fill_square(b, "central_square", &mut rng),
                Use::Government => self.fill_government(g, b),
                Use::Park(name) => self.fill_park(b, &name, &mut rng),
                Use::Depot(name) => self.fill_depot(b, &name),
                Use::River => self.fill_river(b, &mut rng),
            }
        }
        self.avenue_trees(&mut rng);
    }

    /// What lies across the street on each side of a group (N, S, W, E):
    /// the street's kind (`None` at the map edge) and whether a square is
    /// on the far side.
    fn sides(&self, g: &Group) -> [(Option<Kind>, bool); 4] {
        let square_at = |i: usize, j: usize| {
            self.cell
                .get(j)
                .and_then(|row| row.get(i))
                .is_some_and(|&c| {
                    c != NONE && matches!(self.groups[c].use_, Use::Square(_) | Use::GovSquare)
                })
        };
        let (im, jm) = ((g.i0 + g.i1) / 2, (g.j0 + g.j1) / 2);
        [
            (
                (g.j0 > 0).then(|| self.ys[g.j0 - 1].kind),
                g.j0 > 0 && square_at(im, g.j0 - 1),
            ),
            (
                (g.j1 < self.ny()).then(|| self.ys[g.j1].kind),
                square_at(im, g.j1 + 1),
            ),
            (
                (g.i0 > 0).then(|| self.xs[g.i0 - 1].kind),
                g.i0 > 0 && square_at(g.i0 - 1, jm),
            ),
            (
                (g.i1 < self.nx()).then(|| self.xs[g.i1].kind),
                square_at(g.i1 + 1, jm),
            ),
        ]
    }

    /// Perimeter buildings around a courtyard.
    fn fill_buildings(&mut self, g: usize, b: Rect, rng: &mut Rng) {
        if b.w() < 8.0 || b.h() < 8.0 {
            return;
        }
        let sides = self.sides(&self.groups[g]);
        // Work in (u, v): u along the block's long side.
        let along_x = b.w() >= b.h();
        let (u0, u1, v0, v1) = if along_x {
            (b.x0, b.x1, b.y0, b.y1)
        } else {
            (b.y0, b.y1, b.x0, b.x1)
        };
        let (low, high) = if along_x {
            (sides[0], sides[1])
        } else {
            (sides[2], sides[3])
        };
        let to_rect = |a: f32, c: f32, d: f32, e: f32| {
            if along_x {
                Rect::new(a, c, d, e)
            } else {
                Rect::new(c, a, e, d)
            }
        };
        let to_pt = |u: f32, v: f32| if along_x { [u, v] } else { [v, u] };
        let arcade_p = |side: (Option<Kind>, bool)| match side {
            (_, true) => 0.6,
            (Some(Kind::Avenue), false) => 0.15,
            _ => 0.0,
        };
        let short = v1 - v0;
        let mut out: Vec<Bld> = Vec::new();
        if short < 34.0 {
            // One row of buildings through the block.
            for (a, c) in lots(rng, u0, u1, 0.15) {
                out.push((vec![to_rect(a, v0, c, v1)], None, None));
            }
        } else {
            let d1 = snap(rng.range(12.0, 20.0).min(short * 0.4), 0.5);
            let d2 = snap(rng.range(12.0, 20.0).min(short * 0.4), 0.5);
            let (vb, va) = (v0 + d1, v1 - d2);
            let long = u1 - u0;
            let caps = long >= 60.0;
            let e0 = snap(rng.range(10.0, 15.0), 0.5);
            let e1 = snap(rng.range(10.0, 15.0), 0.5);
            let cap0 = caps && rng.chance(0.8);
            let cap1 = caps && rng.chance(0.8);
            // Each end cap splits in two, sometimes with an alley between.
            let vm = snap((vb + va) * 0.5, 0.5);
            let gap = |rng: &mut Rng| {
                if rng.chance(0.35) {
                    snap(rng.range(2.5, 4.0), 0.5)
                } else {
                    0.0
                }
            };
            let (g0, g1) = (gap(rng), gap(rng));
            let strip = |rng: &mut Rng, high_side: bool, out: &mut Vec<Bld>| {
                let side = if high_side { high } else { low };
                let row_lots = lots(rng, u0, u1, 0.18);
                let n = row_lots.len();
                for (k, &(a, c)) in row_lots.iter().enumerate() {
                    let (front, back) = if high_side { (v1, va) } else { (v0, vb) };
                    let first = k == 0 && cap0;
                    let last = k + 1 == n && cap1;
                    // Corner lots join their half of the end cap as an L.
                    if first || last {
                        let (e, g) = if first { (e0, g0) } else { (e1, g1) };
                        let inner = if high_side {
                            vm + g * 0.5
                        } else {
                            vm - g * 0.5
                        };
                        let cap = if first {
                            to_rect(u0, back.min(inner), u0 + e, back.max(inner))
                        } else {
                            to_rect(u1 - e, back.min(inner), u1, back.max(inner))
                        };
                        let lot = to_rect(a, front.min(back), c, front.max(back));
                        let wide = c - a > e + 2.0;
                        if wide && rng.chance(0.7) {
                            // Outline from the outer corner, mirrored as needed.
                            let (uo, du) = if first { (u0, 1.0) } else { (u1, -1.0) };
                            let (vo, dv) = if high_side { (v1, -1.0) } else { (v0, 1.0) };
                            let (f, dd, cc) = (c - a, (back - front).abs(), (inner - back).abs());
                            let pts = [
                                (0.0, 0.0),
                                (f, 0.0),
                                (f, dd),
                                (e, dd),
                                (e, dd + cc),
                                (0.0, dd + cc),
                            ]
                            .iter()
                            .map(|&(pu, pv)| to_pt(uo + du * pu, vo + dv * pv))
                            .collect();
                            out.push((vec![lot, cap], Some(pts), None));
                        } else {
                            out.push((vec![lot], None, None));
                            out.push((vec![cap], None, None));
                        }
                        continue;
                    }
                    let jitter = snap(rng.range(0.0, 3.0), 0.5);
                    let back = if high_side {
                        back + jitter
                    } else {
                        back - jitter
                    };
                    let setback = if rng.chance(0.1) { 2.0 } else { 0.0 };
                    let front = if high_side {
                        front - setback
                    } else {
                        front + setback
                    };
                    let lot = to_rect(a, front.min(back), c, front.max(back));
                    if (back - front).abs() >= 12.0 && rng.chance(arcade_p(side)) {
                        let solid_front = if high_side { front - 4.0 } else { front + 4.0 };
                        let solid = to_rect(a, solid_front.min(back), c, solid_front.max(back));
                        out.push((vec![solid], None, Some(lot)));
                    } else {
                        out.push((vec![lot], None, None));
                    }
                }
            };
            strip(rng, false, &mut out);
            strip(rng, true, &mut out);
            // A rear building in a big courtyard, sometimes a garden.
            let yard = to_rect(
                u0 + if cap0 { e0 } else { 0.0 },
                vb,
                u1 - if cap1 { e1 } else { 0.0 },
                va,
            );
            if yard.w() > 44.0 && yard.h() > 30.0 && rng.chance(0.35) {
                out.push((vec![yard.inset(9.0)], None, None));
            } else if rng.chance(0.3) {
                self.paint_rect(yard.inset(3.0), GRASS);
            }
        }
        let stub = self.groups[g].stub;
        for (rects, outline, arcade) in out {
            let clipped = stub.is_some_and(|st| rects.iter().any(|r| r.overlaps(&st.corridor)));
            if let (Some(Stub { corridor, .. }), true) = (stub, clipped) {
                for r in rects {
                    for piece in r.minus(&corridor) {
                        if piece.w() >= 4.0 && piece.h() >= 4.0 {
                            self.solid(SolidKind::Building, Shape::Rect(piece));
                        }
                    }
                }
                continue;
            }
            match outline {
                Some(pts) => self.solid(SolidKind::Building, Shape::Poly(pts)),
                None => {
                    for r in rects {
                        self.solid(SolidKind::Building, Shape::Rect(r));
                    }
                }
            }
            if let Some(a) = arcade {
                self.overhead.push(Overhead {
                    kind: "arcade",
                    shape: Shape::Rect(a),
                });
            }
        }
    }

    fn fill_square(&mut self, b: Rect, name: &str, rng: &mut Rng) {
        self.paint_rect(b, PLAZA);
        let r = snap(b.w().min(b.h()) * 0.42, 0.5);
        self.areas.push(Marker {
            name: name.to_string(),
            kind: "square",
            pos: b.centre(),
            radius: r,
        });
        // Trees and benches around the edge, a fountain off-centre on big ones.
        let inner = b.inset(5.0);
        ring_points(inner, 12.0, |p, along_x| {
            if rng.chance(0.75) {
                self.overhead.push(Overhead {
                    kind: "canopy",
                    shape: Shape::Circle(p, snap(rng.range(2.5, 4.0), 0.25)),
                });
            }
            if rng.chance(0.3) {
                self.fixed_prop(
                    PropKind::Bench,
                    p,
                    [0.9, 0.3],
                    if along_x { 0.0 } else { 90.0 },
                );
            }
        });
        if b.w().min(b.h()) >= 110.0 {
            let c = b.centre();
            let f = [
                snap(c[0] - b.w() * 0.22, 0.5),
                snap(c[1] - b.h() * 0.22, 0.5),
            ];
            self.solid(SolidKind::Water, Shape::Circle(f, 6.0));
            self.paint_circle(f, 6.0, WATER);
        }
    }

    fn fill_government(&mut self, g: usize, b: Rect) {
        // The parliament faces the central square to the south, behind a
        // fenced forecourt; ministries flank it as L-shapes.
        // Small blocks shrink the forecourt and the margins.
        let kc = self.groups[g].i0;
        let cx = self.xs[kc].pos;
        let forecourt = snap((b.h() * 0.28).clamp(4.0, 30.0), 0.5);
        let half_w = snap(b.w() * 0.21, 0.5);
        let back = (b.y0 + 8.0).min(b.y1 - forecourt - 8.0).max(b.y0);
        let parl = Rect::new(cx - half_w, back, cx + half_w, b.y1 - forecourt);
        self.solid(SolidKind::Building, Shape::Rect(parl));
        self.paint_rect(
            Rect::new(parl.x0 - 6.0, parl.y1, parl.x1 + 6.0, b.y1),
            PLAZA,
        );
        let wing = snap(b.w() * 0.2, 0.5);
        let depth = snap((b.h() * 0.4).max(14.0), 0.5);
        for west in [true, false] {
            let (x0, x1) = if west {
                (b.x0, b.x0 + wing)
            } else {
                (b.x1 - wing, b.x1)
            };
            if x1 - x0 < 10.0 {
                continue;
            }
            // The L's long arm runs along the side street, its short arm
            // along the back street; a shallow block gets a plain wing.
            let arm = snap(((x1 - x0) * 0.5).max(8.0), 0.5);
            let pts = if b.y0 + depth + 2.0 > b.y1 - 6.0 {
                Rect::new(x0, b.y0, x1, b.y1 - 6.0).points()
            } else if west {
                vec![
                    [x0, b.y0],
                    [x1, b.y0],
                    [x1, b.y0 + depth],
                    [x0 + arm, b.y0 + depth],
                    [x0 + arm, b.y1 - 6.0],
                    [x0, b.y1 - 6.0],
                ]
            } else {
                vec![
                    [x0, b.y0],
                    [x1, b.y0],
                    [x1, b.y1 - 6.0],
                    [x1 - arm, b.y1 - 6.0],
                    [x1 - arm, b.y0 + depth],
                    [x0, b.y0 + depth],
                ]
            };
            self.solid(SolidKind::Building, Shape::Poly(pts));
        }
        // Fence along the forecourt with a gate, barriers on the pavement.
        let (fy, gate) = (b.y1 - 1.5, 8.0);
        if forecourt >= 8.0 {
            for (a, c) in [(parl.x0 - 6.0, cx - gate), (cx + gate, parl.x1 + 6.0)] {
                self.solid(SolidKind::Fence, Shape::Rect(Rect::new(a, fy, c, fy + 0.3)));
            }
        }
        let mut x = parl.x0;
        let mut n = 0;
        while x + 2.4 <= parl.x1 {
            if n % 6 != 5 && (x + 1.2 - cx).abs() > gate {
                self.fixed_prop(PropKind::Barrier, [x + 1.2, b.y1 + 1.6], [1.2, 0.12], 0.0);
            }
            x += 2.6;
            n += 1;
        }
        let r = if self.s >= 2000.0 { 70.0 } else { 50.0 };
        let y = self.ys[self.groups[g].j1].pos;
        self.areas.push(Marker {
            name: "government_quarter".into(),
            kind: "government",
            pos: [cx, y],
            radius: r,
        });
    }

    fn fill_park(&mut self, b: Rect, name: &str, rng: &mut Rng) {
        self.paint_rect(b, GRASS);
        let c = b.centre();
        // Two paths crossing at the centre.
        self.paint_rect(Rect::new(c[0] - 2.0, b.y0, c[0] + 2.0, b.y1), PAVEMENT);
        self.paint_rect(Rect::new(b.x0, c[1] - 2.0, b.x1, c[1] + 2.0), PAVEMENT);
        let pond = (b.w().max(b.h()) >= 150.0).then(|| {
            let p = [
                snap(c[0] + b.w() * 0.24, 0.5),
                snap(c[1] + b.h() * 0.22, 0.5),
            ];
            (p, snap(b.w().min(b.h()) * 0.14, 0.5).max(8.0))
        });
        if let Some((p, r)) = pond {
            self.solid(SolidKind::Water, Shape::Circle(p, r));
            self.paint_circle(p, r, WATER);
        }
        let step = 11.0;
        let mut y = b.y0 + 6.0;
        while y < b.y1 - 5.0 {
            let mut x = b.x0 + 6.0;
            while x < b.x1 - 5.0 {
                let p = [
                    snap(x + rng.range(-3.0, 3.0), 0.25),
                    snap(y + rng.range(-3.0, 3.0), 0.25),
                ];
                let r = snap(rng.range(2.5, 5.0), 0.25);
                let on_path =
                    (p[0] - c[0]).abs() < 3.0 + r * 0.3 || (p[1] - c[1]).abs() < 3.0 + r * 0.3;
                let clearing = (p[0] - c[0]).hypot(p[1] - c[1]) < 16.0;
                let in_pond =
                    pond.is_some_and(|(q, pr)| (p[0] - q[0]).hypot(p[1] - q[1]) < pr + 2.0);
                if !on_path && !clearing && !in_pond && rng.chance(0.7) {
                    self.overhead.push(Overhead {
                        kind: "canopy",
                        shape: Shape::Circle(p, r),
                    });
                }
                x += step;
            }
            y += step;
        }
        // Benches along the paths.
        let mut t = b.y0 + 10.0;
        while t < b.y1 - 10.0 {
            if (t - c[1]).abs() > 20.0 && rng.chance(0.5) {
                let side = if rng.chance(0.5) { 3.2 } else { -3.2 };
                self.fixed_prop(PropKind::Bench, [c[0] + side, t], [0.9, 0.3], 90.0);
            }
            t += 14.0;
        }
        self.areas.push(Marker {
            name: name.to_string(),
            kind: "park",
            pos: c,
            radius: snap(b.w().min(b.h()) * 0.45, 0.5),
        });
    }

    fn fill_depot(&mut self, b: Rect, name: &str) {
        // The yard faces the city centre; the depot building is behind it.
        let c = b.centre();
        let (dx, dy) = (self.s * 0.5 - c[0], self.s * 0.5 - c[1]);
        let f = 0.45;
        let (building, yard) = if dx.abs() > dy.abs() {
            let split = if dx > 0.0 {
                snap(b.x0 + b.w() * f, 0.5)
            } else {
                snap(b.x1 - b.w() * f, 0.5)
            };
            if dx > 0.0 {
                (
                    Rect::new(b.x0 + 3.0, b.y0 + 3.0, split, b.y1 - 3.0),
                    Rect::new(split, b.y0, b.x1, b.y1),
                )
            } else {
                (
                    Rect::new(split, b.y0 + 3.0, b.x1 - 3.0, b.y1 - 3.0),
                    Rect::new(b.x0, b.y0, split, b.y1),
                )
            }
        } else {
            let split = if dy > 0.0 {
                snap(b.y0 + b.h() * f, 0.5)
            } else {
                snap(b.y1 - b.h() * f, 0.5)
            };
            if dy > 0.0 {
                (
                    Rect::new(b.x0 + 3.0, b.y0 + 3.0, b.x1 - 3.0, split),
                    Rect::new(b.x0, split, b.x1, b.y1),
                )
            } else {
                (
                    Rect::new(b.x0 + 3.0, split, b.x1 - 3.0, b.y1 - 3.0),
                    Rect::new(b.x0, b.y0, b.x1, split),
                )
            }
        };
        self.solid(SolidKind::Building, Shape::Rect(building));
        // A fence around the yard with a wide gate on each street side.
        let y = yard.inset(1.0);
        let t = 0.3;
        let gate = 7.0;
        let yc = y.centre();
        let mut fence = |r: Rect| self.solid(SolidKind::Fence, Shape::Rect(r));
        for (horizontal, at) in [
            (true, y.y0),
            (true, y.y1 - t),
            (false, y.x0),
            (false, y.x1 - t),
        ] {
            // Skip the side against the depot building.
            let against = if horizontal {
                (at - building.y1).abs() < 2.0 || (at - building.y0).abs() < 2.0
            } else {
                (at - building.x1).abs() < 2.0 || (at - building.x0).abs() < 2.0
            };
            if against {
                continue;
            }
            if horizontal {
                fence(Rect::new(y.x0, at, yc[0] - gate, at + t));
                fence(Rect::new(yc[0] + gate, at, y.x1, at + t));
            } else {
                fence(Rect::new(at, y.y0, at + t, yc[1] - gate));
                fence(Rect::new(at, yc[1] + gate, at + t, y.y1));
            }
        }
        self.depots.push(Marker {
            name: name.to_string(),
            kind: "",
            pos: [snap(yc[0], 0.5), snap(yc[1], 0.5)],
            radius: 0.0,
        });
    }

    fn fill_river(&mut self, band: Rect, rng: &mut Rng) {
        let Some(river) = self.river.take() else {
            return;
        };
        self.paint_rect(band, GRASS);
        let hw = river.width * 0.5;
        // Water between bridges, sampled every 16 m.
        let mut decks: Vec<(f32, f32, usize)> = river
            .bridges
            .iter()
            .map(|&k| {
                let l = self.xs[k];
                (
                    l.pos - l.kind.width() * 0.5,
                    l.pos + l.kind.width() * 0.5,
                    k,
                )
            })
            .collect();
        decks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut a = 0.0;
        let bank = |x: f32, d: f32| snap(river.centre(x) + d, 0.01);
        for end in decks.iter().map(|d| d.0).chain([self.s]) {
            if end - a > 1.0 {
                let mut xs: Vec<f32> = Vec::new();
                let mut x = a;
                while x < end {
                    xs.push(x);
                    x = (snap(x / 16.0, 1.0) + 1.0) * 16.0;
                }
                xs.push(end);
                let mut pts: Vec<[f32; 2]> = xs.iter().map(|&x| [x, bank(x, -hw)]).collect();
                pts.extend(xs.iter().rev().map(|&x| [x, bank(x, hw)]));
                self.solids.push(Solid {
                    kind: SolidKind::Water,
                    shape: Shape::Poly(pts),
                });
            }
            if let Some(d) = decks.iter().find(|d| d.0 == end) {
                a = d.1;
            }
        }
        // Water tiles; bridge decks are painted over with the streets.
        for tx in 0..self.tiles {
            let x = (tx as f32 + 0.5) * TILE_M;
            let c = river.centre(x);
            let (t0, t1) = (
                ((c - hw) / TILE_M - 0.5).ceil().max(0.0) as usize,
                ((c + hw) / TILE_M - 0.5).floor() as usize,
            );
            for ty in t0..=t1.min(self.tiles - 1) {
                self.ground[ty * self.tiles + tx] = WATER;
            }
        }
        // Railings along both edges of each deck, and the bridge areas.
        for (n, &(x0, x1, k)) in decks.iter().enumerate() {
            let top = (0..=4)
                .map(|q| bank(x0 + (x1 - x0) * q as f32 / 4.0, -hw))
                .fold(f32::MAX, f32::min);
            let bot = (0..=4)
                .map(|q| bank(x0 + (x1 - x0) * q as f32 / 4.0, hw))
                .fold(f32::MIN, f32::max);
            let (y0, y1) = (snap(top - 1.5, 0.5), snap(bot + 1.5, 0.5));
            self.solid(
                SolidKind::Wall,
                Shape::Rect(Rect::new(x0, y0, x0 + 0.4, y1)),
            );
            self.solid(
                SolidKind::Wall,
                Shape::Rect(Rect::new(x1 - 0.4, y0, x1, y1)),
            );
            let x = self.xs[k].pos;
            self.areas.push(Marker {
                name: format!("bridge_{}", n + 1),
                kind: "bridge",
                pos: [x, snap(river.centre(x), 0.5)],
                radius: snap(hw + 6.0, 0.5),
            });
        }
        // Trees and benches along the promenades.
        let mut x = 10.0;
        while x < self.s - 10.0 {
            let on_deck = decks.iter().any(|d| x > d.0 - 8.0 && x < d.1 + 8.0);
            let c = river.centre(x);
            for (edge, water) in [(band.y0, c - hw), (band.y1, c + hw)] {
                let room = (water - edge).abs();
                if on_deck || room < 9.0 {
                    continue;
                }
                let y = snap((edge + water) * 0.5, 0.25);
                if rng.chance(0.7) {
                    self.overhead.push(Overhead {
                        kind: "canopy",
                        shape: Shape::Circle([x, y], snap(rng.range(3.0, 4.5), 0.25)),
                    });
                } else if rng.chance(0.3) {
                    self.fixed_prop(PropKind::Bench, [x, y], [0.9, 0.3], 0.0);
                }
            }
            x += 18.0;
        }
        self.river = Some(river);
    }

    /// Street trees on the wide pavements of avenues.
    fn avenue_trees(&mut self, rng: &mut Rng) {
        let mut trees = Vec::new();
        for (vertical, lines, cross) in [(true, &self.xs, &self.ys), (false, &self.ys, &self.xs)] {
            for (k, l) in lines.iter().enumerate() {
                if l.kind != Kind::Avenue {
                    continue;
                }
                let off = l.kind.width() * 0.5 - 3.0;
                let mut t = 12.0;
                while t < self.s - 12.0 {
                    let near_cross = cross
                        .iter()
                        .any(|c| (c.pos - t).abs() < c.kind.width() * 0.5 + 6.0);
                    let row = cross.partition_point(|c| c.pos < t);
                    let kept = if vertical {
                        self.vkeep[k][row]
                    } else {
                        self.hkeep[k][row]
                    };
                    let river_row = vertical && self.river.as_ref().is_some_and(|r| r.row == row);
                    if kept && !near_cross && !river_row {
                        for side in [-1.0, 1.0] {
                            if rng.chance(0.75) {
                                let p = if vertical {
                                    [l.pos + side * off, t]
                                } else {
                                    [t, l.pos + side * off]
                                };
                                trees.push((p, snap(rng.range(2.5, 3.5), 0.25)));
                            }
                        }
                    }
                    t += 24.0;
                }
            }
        }
        for (p, r) in trees {
            self.overhead.push(Overhead {
                kind: "canopy",
                shape: Shape::Circle(p, r),
            });
        }
    }

    /// Adds a solid. Shapes turned inside out or squashed to crumbs by a
    /// block too small for its layout are dropped; thin walls and fences
    /// stay.
    fn solid(&mut self, kind: SolidKind, shape: Shape) {
        let fits = |r: Rect| r.w() >= 0.2 && r.h() >= 0.2 && r.w().max(r.h()) >= 1.0;
        let ok = match &shape {
            Shape::Rect(r) => fits(*r),
            Shape::Circle(_, r) => *r >= 0.5,
            Shape::Poly(p) => p.len() >= 3 && fits(shape.bbox()),
        };
        if ok {
            self.solids.push(Solid { kind, shape });
        }
    }

    fn fixed_prop(&mut self, kind: PropKind, centre: [f32; 2], half: [f32; 2], deg: f32) {
        self.props.insert(
            self.fixed_props,
            Prop {
                kind,
                centre,
                half,
                deg,
            },
        );
        self.fixed_props += 1;
    }

    // --- markers ---------------------------------------------------------

    /// Is there a street corner at node (`k`, `m`) in quadrant (`sx`, `sy`):
    /// streets on both sides of it?
    fn corner(&self, k: usize, m: usize, sx: i32, sy: i32) -> bool {
        let i = if sx > 0 { k + 1 } else { k };
        let j = if sy > 0 { m + 1 } else { m };
        self.hkeep[m][i] && self.vkeep[k][j]
    }

    /// The pavement corner of node (`k`, `m`) in quadrant (`sx`, `sy`),
    /// `inset` metres in from the building line.
    fn corner_pos(&self, k: usize, m: usize, sx: i32, sy: i32, inset: f32) -> [f32; 2] {
        let (lx, ly) = (self.xs[k], self.ys[m]);
        [
            lx.pos + sx as f32 * (lx.kind.width() * 0.5 - inset),
            ly.pos + sy as f32 * (ly.kind.width() * 0.5 - inset),
        ]
    }

    fn markers(&mut self) {
        let s = self.s;
        let mut rng = Rng::new(self.seed, 6);
        // Map-edge entries at both ends of every avenue; on maps with few
        // avenues, streets too, so each edge has at least three.
        for (vertical, lines) in [(true, self.xs.clone()), (false, self.ys.clone())] {
            let mut chosen: Vec<usize> = (0..lines.len())
                .filter(|&k| lines[k].kind == Kind::Avenue)
                .collect();
            for q in [0.2, 0.8, 0.5] {
                if chosen.len() >= 3 {
                    break;
                }
                let pick = (0..lines.len())
                    .filter(|k| !chosen.contains(k) && lines[*k].kind != Kind::Lane)
                    .min_by(|&a, &b| {
                        (lines[a].pos - q * s)
                            .abs()
                            .total_cmp(&(lines[b].pos - q * s).abs())
                    });
                chosen.extend(pick);
            }
            chosen.sort_unstable();
            let (a, b) = if vertical { ("n", "s") } else { ("w", "e") };
            for (n, &k) in chosen.iter().enumerate() {
                let l = lines[k];
                for (side, at) in [(a, 6.0), (b, s - 6.0)] {
                    self.entries.push(Marker {
                        name: format!("edge_{side}{}", n + 1),
                        kind: "map_edge",
                        pos: if vertical { [l.pos, at] } else { [at, l.pos] },
                        radius: if l.kind == Kind::Avenue { 8.0 } else { 5.0 },
                    });
                }
            }
        }
        // Metro stations about every 800 m, on street corners.
        let n = (s / 800.0).ceil() as usize;
        let pitch = s / n as f32;
        let mut stations: Vec<([f32; 2], (usize, usize))> = Vec::new();
        for b in 0..n {
            for a in 0..n {
                let t = [
                    (a as f32 + 0.5 + rng.range(-0.15, 0.15)) * pitch,
                    (b as f32 + 0.5 + rng.range(-0.15, 0.15)) * pitch,
                ];
                if let Some(p) = self.corner_near(
                    t,
                    &stations.iter().map(|s| s.1).collect::<Vec<_>>(),
                    &mut rng,
                ) {
                    stations.push(p);
                }
            }
        }
        stations.sort_by(|a, b| {
            let d = |p: [f32; 2]| (p[0] - s * 0.5).hypot(p[1] - s * 0.5);
            d(a.0).total_cmp(&d(b.0)).then(a.1.cmp(&b.1))
        });
        for (n, (pos, _)) in stations.into_iter().enumerate() {
            self.entries.push(Marker {
                name: format!("metro_{}", n + 1),
                kind: "metro",
                pos,
                radius: 4.0,
            });
        }
        // Bus stops about every 1200 m, on avenue pavements mid-block.
        let n = (s / 1200.0).ceil() as usize;
        let pitch = s / n as f32;
        let mut stops = Vec::new();
        let mut taken = Vec::new();
        for b in 0..n {
            for a in 0..n {
                let t = [(a as f32 + 0.5) * pitch, (b as f32 + 0.5) * pitch];
                if let Some(p) = self.bus_stop_near(t, &mut taken, &mut rng) {
                    stops.push(p);
                }
            }
        }
        for (n, pos) in stops.into_iter().enumerate() {
            self.entries.push(Marker {
                name: format!("bus_{}", n + 1),
                kind: "bus",
                pos,
                radius: 3.0,
            });
        }
        // Staging points at the corner of each square nearest a depot.
        let depots: Vec<[f32; 2]> = self.depots.iter().map(|d| d.pos).collect();
        let squares: Vec<(String, usize, usize, usize, usize)> = self
            .groups
            .iter()
            .filter_map(|g| match &g.use_ {
                Use::GovSquare => Some(("central_square".to_string(), g.i0, g.i1, g.j0, g.j1)),
                Use::Square(n) => Some((n.clone(), g.i0, g.i1, g.j0, g.j1)),
                _ => None,
            })
            .collect();
        for (name, i0, i1, j0, j1) in squares {
            let corners = [(i0 - 1, j0 - 1), (i1, j0 - 1), (i0 - 1, j1), (i1, j1)];
            let pos = corners
                .iter()
                .map(|&(k, m)| [self.xs[k].pos, self.ys[m].pos])
                .min_by(|a, b| {
                    let d = |p: &[f32; 2]| {
                        depots
                            .iter()
                            .map(|q| (q[0] - p[0]).hypot(q[1] - p[1]))
                            .fold(f32::MAX, f32::min)
                    };
                    d(a).total_cmp(&d(b))
                })
                .unwrap_or([s * 0.5, s * 0.5]);
            self.staging.push(Marker {
                name: format!("staging_{name}"),
                kind: "",
                pos,
                radius: 0.0,
            });
        }
    }

    /// The street corner nearest `t` whose node is not in `taken`, away
    /// from the river.
    fn corner_near(
        &self,
        t: [f32; 2],
        taken: &[(usize, usize)],
        rng: &mut Rng,
    ) -> Option<([f32; 2], (usize, usize))> {
        let river_lines = self.river.as_ref().map(|r| (r.row - 1, r.row));
        let mut nodes: Vec<(f32, usize, usize)> = Vec::new();
        for (k, lx) in self.xs.iter().enumerate() {
            for (m, ly) in self.ys.iter().enumerate() {
                if (lx.pos - t[0]).abs() > 600.0
                    || (ly.pos - t[1]).abs() > 600.0
                    || taken.contains(&(k, m))
                    || river_lines.is_some_and(|(a, b)| m == a || m == b)
                {
                    continue;
                }
                nodes.push(((lx.pos - t[0]).hypot(ly.pos - t[1]), k, m));
            }
        }
        nodes.sort_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2).cmp(&(b.1, b.2))));
        for &(_, k, m) in nodes.iter().take(16) {
            let mut quads = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
            rng.shuffle(&mut quads);
            if let Some(&(sx, sy)) = quads.iter().find(|&&(sx, sy)| self.corner(k, m, sx, sy)) {
                return Some((self.corner_pos(k, m, sx, sy, 2.0), (k, m)));
            }
        }
        None
    }

    /// A pavement spot mid-block on the avenue segment nearest `t` that has
    /// no stop yet.
    fn bus_stop_near(
        &self,
        t: [f32; 2],
        taken: &mut Vec<(bool, usize, usize)>,
        rng: &mut Rng,
    ) -> Option<[f32; 2]> {
        let mut best: Option<(f32, (bool, usize, usize), [f32; 2], f32)> = None;
        for (vertical, lines, cross) in [(true, &self.xs, &self.ys), (false, &self.ys, &self.xs)] {
            for (k, l) in lines.iter().enumerate() {
                if l.kind != Kind::Avenue {
                    continue;
                }
                for row in 1..cross.len() {
                    let kept = if vertical {
                        self.vkeep[k][row]
                    } else {
                        self.hkeep[k][row]
                    };
                    let river = vertical && self.river.as_ref().is_some_and(|r| r.row == row);
                    if !kept || river || taken.contains(&(vertical, k, row)) {
                        continue;
                    }
                    let mid = snap((cross[row - 1].pos + cross[row].pos) * 0.5, 0.5);
                    let p = if vertical { [l.pos, mid] } else { [mid, l.pos] };
                    let d = (p[0] - t[0]).hypot(p[1] - t[1]);
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, (vertical, k, row), p, l.kind.width() * 0.5 - 2.0));
                    }
                }
            }
        }
        let (_, seg, p, off) = best?;
        taken.push(seg);
        let off = if rng.chance(0.5) { off } else { -off };
        Some(if seg.0 {
            [p[0] + off, p[1]]
        } else {
            [p[0], p[1] + off]
        })
    }

    /// Keeps entries, depots, staging points and area centres clear: drops
    /// any building, wall or fence that comes too close.
    fn clear_markers(&mut self) {
        let keep: Vec<([f32; 2], f32)> = self
            .entries
            .iter()
            .map(|e| (e.pos, 1.5))
            .chain(
                self.depots
                    .iter()
                    .chain(&self.staging)
                    .map(|d| (d.pos, 3.0)),
            )
            .chain(self.areas.iter().map(|a| (a.pos, 3.0)))
            .collect();
        const CELL: f32 = 64.0;
        let dim = (self.s / CELL).ceil() as usize + 1;
        let mut grid: Vec<Vec<usize>> = vec![Vec::new(); dim * dim];
        let cell_of = |v: f32| ((v / CELL).floor().max(0.0) as usize).min(dim - 1);
        for (n, (p, r)) in keep.iter().enumerate() {
            for gy in cell_of(p[1] - r)..=cell_of(p[1] + r) {
                for gx in cell_of(p[0] - r)..=cell_of(p[0] + r) {
                    grid[gy * dim + gx].push(n);
                }
            }
        }
        self.solids.retain(|sol| {
            let b = sol.shape.bbox();
            for gy in cell_of(b.y0)..=cell_of(b.y1) {
                for gx in cell_of(b.x0)..=cell_of(b.x1) {
                    for &n in &grid[gy * dim + gx] {
                        let (p, r) = keep[n];
                        if sol.shape.near(p, r) {
                            debug_assert!(
                                sol.kind != SolidKind::Water,
                                "water over a marker at {p:?}"
                            );
                            return sol.kind == SolidKind::Water;
                        }
                    }
                }
            }
            true
        });
        self.props.retain(|pr| {
            let reach = pr.half[0].max(pr.half[1]) + 3.0;
            keep.iter()
                .all(|(p, r)| (p[0] - pr.centre[0]).hypot(p[1] - pr.centre[1]) > r + reach)
        });
        self.fixed_props = self.props.len();
    }

    // --- props, roads, ground --------------------------------------------

    /// Parked cars in rows along streets, bins on street corners; within
    /// the prop budget.
    fn street_props(&mut self, p: &CityParams) {
        let mut rng = Rng::new(self.seed, 7);
        let km2 = (self.s / 1000.0) * (self.s / 1000.0);
        let budget = (p.max_props as f32).min(600.0 * km2) as usize;
        let free = budget.saturating_sub(self.fixed_props.min(budget));
        let car_budget = free * 85 / 100;
        // Kerb stretches between crossings, both sides.
        struct Kerb {
            vertical: bool,
            at: f32,
            a: f32,
            b: f32,
        }
        let mut kerbs = Vec::new();
        for (vertical, lines, cross) in [(true, &self.xs, &self.ys), (false, &self.ys, &self.xs)] {
            for (k, l) in lines.iter().enumerate() {
                if l.kind == Kind::Lane {
                    continue;
                }
                for row in 0..=cross.len() {
                    let kept = if vertical {
                        self.vkeep[k][row]
                    } else {
                        self.hkeep[k][row]
                    };
                    if !kept || (vertical && self.river.as_ref().is_some_and(|r| r.row == row)) {
                        continue;
                    }
                    let a = if row == 0 {
                        0.0
                    } else {
                        cross[row - 1].pos + cross[row - 1].kind.width() * 0.5
                    } + 6.0;
                    let b = if row == cross.len() {
                        self.s
                    } else {
                        cross[row].pos - cross[row].kind.width() * 0.5
                    } - 6.0;
                    let off = l.kind.carriageway() * 0.5 - 1.1;
                    for side in [-1.0, 1.0] {
                        kerbs.push(Kerb {
                            vertical,
                            at: l.pos + side * off,
                            a,
                            b,
                        });
                    }
                }
            }
        }
        let slots: f32 = kerbs.iter().map(|k| ((k.b - k.a) / 6.5).max(0.0)).sum();
        let q = (car_budget as f32 / (0.8 * slots.max(1.0))).min(1.0);
        let mut cars = Vec::new();
        for k in &kerbs {
            if !rng.chance(q) {
                continue;
            }
            let mut t = k.a + rng.range(0.0, 4.0);
            while t + 4.4 <= k.b {
                if rng.chance(0.8) {
                    let c = t + 2.2;
                    let (centre, deg) = if k.vertical {
                        ([k.at, c], 90.0)
                    } else {
                        ([c, k.at], 0.0)
                    };
                    cars.push(Prop {
                        kind: PropKind::Car,
                        centre: [snap(centre[0], 0.05), snap(centre[1], 0.05)],
                        half: [2.2, 0.9],
                        deg,
                    });
                }
                t += 6.5;
            }
        }
        // Bins on some street corners.
        let mut bins = Vec::new();
        let bin_q = ((free - car_budget.min(cars.len())) as f32
            / (self.nx() * self.ny()).max(1) as f32)
            .min(0.5);
        for k in 0..self.nx() {
            for m in 0..self.ny() {
                if !rng.chance(bin_q) {
                    continue;
                }
                let (sx, sy) = (
                    if rng.chance(0.5) { 1 } else { -1 },
                    if rng.chance(0.5) { 1 } else { -1 },
                );
                if self.corner(k, m, sx, sy) {
                    bins.push(Prop {
                        kind: PropKind::Bin,
                        centre: self.corner_pos(k, m, sx, sy, 1.0),
                        half: [0.35, 0.35],
                        deg: 0.0,
                    });
                }
            }
        }
        let markers: Vec<([f32; 2], f32)> = self
            .entries
            .iter()
            .map(|e| (e.pos, e.radius + 1.0))
            .chain(
                self.depots
                    .iter()
                    .chain(&self.staging)
                    .map(|d| (d.pos, 8.0)),
            )
            .chain(self.areas.iter().map(|a| (a.pos, 4.0)))
            .collect();
        let clear = |pr: &Prop| {
            markers
                .iter()
                .all(|(p, r)| (p[0] - pr.centre[0]).hypot(p[1] - pr.centre[1]) > r + 3.4)
        };
        self.props.truncate(self.fixed_props.min(budget));
        self.props
            .extend(bins.into_iter().chain(cars).filter(clear));
        self.props.truncate(budget);
    }

    /// Polylines along every street centreline (one per run of street
    /// segments, with a vertex at every crossing) and the cul-de-sac stubs.
    fn road_graph(&mut self) {
        let mut roads = Vec::new();
        for (vertical, lines, cross) in [(true, &self.xs, &self.ys), (false, &self.ys, &self.xs)] {
            let n = cross.len();
            for (k, l) in lines.iter().enumerate() {
                let kept = |row: usize| {
                    if vertical {
                        self.vkeep[k][row]
                    } else {
                        self.hkeep[k][row]
                    }
                };
                let mut row = 0;
                while row <= n {
                    if !kept(row) {
                        row += 1;
                        continue;
                    }
                    let start = row;
                    while row <= n && kept(row) {
                        row += 1;
                    }
                    let mut ts = vec![if start == 0 {
                        0.0
                    } else {
                        cross[start - 1].pos
                    }];
                    ts.extend((start..row).map(|r| if r == n { self.s } else { cross[r].pos }));
                    roads.push(
                        ts.iter()
                            .map(|&t| if vertical { [l.pos, t] } else { [t, l.pos] })
                            .collect::<Vec<_>>(),
                    );
                }
            }
        }
        for g in &self.groups {
            if let Some(st) = g.stub {
                roads.push(vec![st.a, st.b]);
            }
        }
        self.roads = roads;
    }

    /// Carriageways over everything else; pavement on bridge decks.
    fn paint_streets(&mut self) {
        let mut rects = Vec::new();
        let mut decks = Vec::new();
        for (vertical, lines, cross) in [(true, &self.xs, &self.ys), (false, &self.ys, &self.xs)] {
            for (k, l) in lines.iter().enumerate() {
                for row in 0..=cross.len() {
                    let kept = if vertical {
                        self.vkeep[k][row]
                    } else {
                        self.hkeep[k][row]
                    };
                    if !kept {
                        continue;
                    }
                    // Each end reaches the far edge of the crossing carriageway.
                    let (a, b) = (
                        if row == 0 {
                            0.0
                        } else {
                            cross[row - 1].pos - cross[row - 1].kind.carriageway() * 0.5
                        },
                        if row == cross.len() {
                            self.s
                        } else {
                            cross[row].pos + cross[row].kind.carriageway() * 0.5
                        },
                    );
                    let (hc, hw) = (l.kind.carriageway() * 0.5, l.kind.width() * 0.5);
                    let r = |h: f32| {
                        if vertical {
                            Rect::new(l.pos - h, a, l.pos + h, b)
                        } else {
                            Rect::new(a, l.pos - h, b, l.pos + h)
                        }
                    };
                    rects.push(r(hc));
                    if vertical && self.river.as_ref().is_some_and(|rv| rv.row == row) {
                        decks.push(r(hw));
                    }
                }
            }
        }
        for g in &self.groups {
            if let Some(Stub {
                a, b, kind, from, ..
            }) = g.stub
            {
                // From the far edge of the street it leaves to a turning
                // area at its end.
                let (hc, hf) = (kind.carriageway() * 0.5, from.carriageway() * 0.5);
                let dir = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
                let a = [a[0] - dir[0] * hf, a[1] - dir[1] * hf];
                let b = [b[0] + dir[0] * hc, b[1] + dir[1] * hc];
                rects.push(Rect::new(
                    a[0].min(b[0]) - hc * dir[1].abs(),
                    a[1].min(b[1]) - hc * dir[0].abs(),
                    a[0].max(b[0]) + hc * dir[1].abs(),
                    a[1].max(b[1]) + hc * dir[0].abs(),
                ));
            }
        }
        for d in decks {
            self.paint_rect(d, PAVEMENT);
        }
        for r in rects {
            self.paint_rect(r, ROAD);
        }
    }

    /// Paints the tiles whose centres lie in `r`.
    fn paint_rect(&mut self, r: Rect, kind: u8) {
        let t = self.tiles as i64;
        // First tile whose centre is at or past `v`.
        let lo = |v: f32| (((v / TILE_M) - 0.5).ceil() as i64).clamp(0, t);
        for ty in lo(r.y0)..lo(r.y1) {
            let row = ty as usize * self.tiles;
            for tx in lo(r.x0)..lo(r.x1) {
                self.ground[row + tx as usize] = kind;
            }
        }
    }

    fn paint_circle(&mut self, c: [f32; 2], radius: f32, kind: u8) {
        let b = Rect::new(c[0] - radius, c[1] - radius, c[0] + radius, c[1] + radius);
        let t = self.tiles as i64;
        let lo = |v: f32| (((v / TILE_M) - 0.5).ceil() as i64).clamp(0, t);
        for ty in lo(b.y0)..lo(b.y1) {
            for tx in lo(b.x0)..lo(b.x1) {
                let p = [(tx as f32 + 0.5) * TILE_M, (ty as f32 + 0.5) * TILE_M];
                if (p[0] - c[0]).hypot(p[1] - c[1]) < radius {
                    self.ground[ty as usize * self.tiles + tx as usize] = kind;
                }
            }
        }
    }

    // --- output ----------------------------------------------------------

    fn into_tiled(self, p: &CityParams) -> TiledMap {
        let mut next_id = 1u32;
        let mut obj = |kind: &str, name: &str| {
            let o = Object {
                id: next_id,
                kind: kind.to_string(),
                name: name.to_string(),
                ..Default::default()
            };
            next_id += 1;
            o
        };
        let solid_name = |k: SolidKind| match k {
            SolidKind::Building => "building",
            SolidKind::Wall => "wall",
            SolidKind::Water => "water",
            SolidKind::Fence => "fence",
        };
        let solids: Vec<Object> = self
            .solids
            .iter()
            .map(|s| shape_object(obj(solid_name(s.kind), ""), &s.shape))
            .collect();
        let props: Vec<Object> = self
            .props
            .iter()
            .map(|pr| {
                let name = match pr.kind {
                    PropKind::Car => "car",
                    PropKind::Bin => "bin",
                    PropKind::Barrier => "barrier",
                    PropKind::Bench => "bench",
                    PropKind::Barricade => "barricade",
                    PropKind::PoliceBarrier => "police_barrier",
                };
                // Tiled rotates about the top-left corner.
                let (sn, cs) = sin_cos_deg(pr.deg as f64);
                let (hx, hy) = (pr.half[0] as f64, pr.half[1] as f64);
                let ox = pr.centre[0] as f64 - (hx * cs - hy * sn);
                let oy = pr.centre[1] as f64 - (hx * sn + hy * cs);
                Object {
                    x: px(ox),
                    y: px(oy),
                    width: px(hx * 2.0),
                    height: px(hy * 2.0),
                    rotation: pr.deg as f64,
                    ..obj(name, "")
                }
            })
            .collect();
        let roads: Vec<Object> = self
            .roads
            .iter()
            .map(|line| {
                let (x, y) = (px(line[0][0] as f64), px(line[0][1] as f64));
                Object {
                    x,
                    y,
                    polyline: Some(rel_points(line, x, y)),
                    ..obj("", "")
                }
            })
            .collect();
        let overhead: Vec<Object> = self
            .overhead
            .iter()
            .map(|o| shape_object(obj(o.kind, ""), &o.shape))
            .collect();
        let circle = |o: Object, m: &Marker| {
            let r = m.radius as f64;
            Object {
                x: px(m.pos[0] as f64 - r),
                y: px(m.pos[1] as f64 - r),
                width: px(2.0 * r),
                height: px(2.0 * r),
                ellipse: true,
                ..o
            }
        };
        let entries: Vec<Object> = self
            .entries
            .iter()
            .map(|e| circle(obj(e.kind, &e.name), e))
            .collect();
        let point = |o: Object, m: &Marker| Object {
            x: px(m.pos[0] as f64),
            y: px(m.pos[1] as f64),
            point: true,
            ..o
        };
        let depots: Vec<Object> = self
            .depots
            .iter()
            .map(|d| point(obj("depot", &d.name), d))
            .collect();
        let staging: Vec<Object> = self
            .staging
            .iter()
            .map(|d| point(obj("staging", &d.name), d))
            .collect();
        let areas: Vec<Object> = self
            .areas
            .iter()
            .map(|a| Object {
                properties: vec![Property::string("tag", a.kind)],
                ..circle(obj("area", &a.name), a)
            })
            .collect();
        let gids: Vec<u32> = self.ground.iter().map(|&g| g as u32 + 1).collect();
        let (data, encoding, compression) = tiled::encode_gids(&gids, p.compress_ground);
        let objects = |id: u32, name: &str, objects: Vec<Object>| Layer {
            kind: "objectgroup".into(),
            id,
            name: name.into(),
            draworder: "topdown".into(),
            objects,
            ..Default::default()
        };
        let layers = vec![
            Layer {
                kind: "tilelayer".into(),
                id: 1,
                name: "ground".into(),
                width: self.tiles as u32,
                height: self.tiles as u32,
                data: Some(data),
                encoding,
                compression,
                ..Default::default()
            },
            objects(2, "solids", solids),
            objects(3, "props", props),
            objects(4, "roads", roads),
            objects(5, "overhead", overhead),
            Layer {
                kind: "group".into(),
                id: 6,
                name: "markers".into(),
                layers: vec![
                    objects(7, "entries", entries),
                    objects(8, "depots", depots),
                    objects(9, "staging", staging),
                    objects(10, "areas", areas),
                ],
                ..Default::default()
            },
        ];
        TiledMap {
            width: self.tiles as u32,
            height: self.tiles as u32,
            tilewidth: TILE_PX,
            tileheight: TILE_PX,
            nextlayerid: 11,
            nextobjectid: next_id,
            properties: vec![
                Property::float("metres_per_pixel", METRES_PER_PIXEL),
                Property::string("generator", "riot_data::citygen"),
                Property::string("seed", &p.seed.to_string()),
                Property::int("size_m", self.s as i64),
            ],
            tilesets: vec![ground_tileset()],
            layers,
            ..Default::default()
        }
    }
}

/// Chooses the river's row and makes it a band wide enough for the water,
/// its meanders and a promenade on each bank. Moves the lines south of it.
fn place_river(
    s: f32,
    block: f32,
    ys: &mut Vec<Line>,
    mc: usize,
    sq_rows: usize,
    rng: &mut Rng,
) -> Option<River> {
    let width = snap((20.0 + s * 0.008).clamp(24.0, 90.0), TILE_M);
    let amp = snap((s * 0.004).clamp(4.0, 30.0), 1.0);
    let band = width + 2.0 * amp + 2.0 * 10.0;
    // Rows strictly south of the central square, or north of the
    // government quarter; prefer about 72% of the way down.
    let mut rows: Vec<usize> = (1..ys.len())
        .filter(|&r| r > mc + sq_rows || r + 1 < mc)
        .collect();
    rows.sort_by(|&a, &b| {
        let d = |r: usize| (ys[r].pos - s * 0.72).abs() + if r < mc { s } else { 0.0 };
        d(a).total_cmp(&d(b))
    });
    for r in rows {
        let pitch = snap(
            band + (ys[r - 1].kind.width() + ys[r].kind.width()) * 0.5,
            TILE_M,
        );
        let delta = pitch - (ys[r].pos - ys[r - 1].pos);
        let limit = s - block * 0.35;
        if ys[r].pos + delta > limit {
            continue;
        }
        let mut moved = ys.clone();
        for l in &mut moved[r..] {
            l.pos += delta;
        }
        moved.retain(|l| l.pos <= limit);
        // Moving lines north of the centre must leave the central rows.
        if r < mc && nearest_line(&moved, s * 0.5) != mc {
            continue;
        }
        *ys = moved;
        let mid = (ys[r - 1].pos + ys[r].pos) * 0.5;
        let l1 = rng.range(1400.0, 2600.0);
        let waves = [
            (0.65, l1, rng.range(0.0, 1.0)),
            (0.35, l1 * 0.37, rng.range(0.0, 1.0)),
        ];
        return Some(River {
            row: r,
            mid,
            width,
            amp,
            waves,
            bridges: Vec::new(),
        });
    }
    None
}

/// x-lines that cross the river on a bridge: every avenue, some streets,
/// at least two and never more than 700 m apart.
fn choose_bridges(xs: &[Line], rng: &mut Rng) -> Vec<usize> {
    let mut b: Vec<usize> = (0..xs.len())
        .filter(|&k| xs[k].kind == Kind::Avenue || rng.chance(0.1))
        .collect();
    loop {
        let mut want: Option<f32> = None;
        if b.len() < 2 {
            let span = xs.last().map_or(0.0, |l| l.pos);
            want = Some(span * if b.is_empty() { 0.3 } else { 0.7 });
        }
        for w in b.windows(2) {
            let (a, c) = (xs[w[0]].pos, xs[w[1]].pos);
            if want.is_none() && c - a > 700.0 {
                want = Some((a + c) * 0.5);
            }
        }
        let Some(at) = want else { break };
        let Some(k) = (0..xs.len())
            .filter(|k| !b.contains(k))
            .min_by(|&p, &q| (xs[p].pos - at).abs().total_cmp(&(xs[q].pos - at).abs()))
        else {
            break;
        };
        b.push(k);
        b.sort_unstable();
    }
    b
}

/// Lots along a strip from `a` to `b`: frontages of 10–26 m, with an alley
/// gap of 2–4 m after some of them.
fn lots(rng: &mut Rng, a: f32, b: f32, gap_p: f32) -> Vec<(f32, f32)> {
    let mut v = Vec::new();
    let mut u = a;
    while b - u > 0.25 {
        let mut f = snap(rng.range(10.0, 26.0), 0.5);
        if b - (u + f) < 8.0 {
            f = b - u;
        }
        v.push((u, u + f));
        u += f;
        if b - u > 0.25 && rng.chance(gap_p) {
            let g = snap(rng.range(2.0, 4.0), 0.5);
            if b - (u + g) >= 8.0 {
                u += g;
            }
        }
    }
    v
}

/// Points every `step` metres around the edge of `r`; `along_x` for points
/// on the top and bottom edges.
fn ring_points(r: Rect, step: f32, mut f: impl FnMut([f32; 2], bool)) {
    let mut x = r.x0;
    while x <= r.x1 {
        f([x, r.y0], true);
        f([x, r.y1], true);
        x += step;
    }
    let mut y = r.y0 + step;
    while y < r.y1 {
        f([r.x0, y], false);
        f([r.x1, y], false);
        y += step;
    }
}

/// Compass direction of `p` from the map centre.
fn compass(p: [f32; 2], s: f32) -> String {
    let (dx, dy) = (p[0] - s * 0.5, s * 0.5 - p[1]);
    // Within 22.5° of an axis, only that axis counts.
    const TAN_22_5: f32 = 0.414_213_57;
    let ns = (dy.abs() > dx.abs() * TAN_22_5).then_some(if dy > 0.0 { "north" } else { "south" });
    let ew = (dx.abs() > dy.abs() * TAN_22_5).then_some(if dx > 0.0 { "east" } else { "west" });
    match (ns, ew) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) | (None, Some(a)) => a.to_string(),
        (None, None) => "centre".to_string(),
    }
}

/// Metres → Tiled pixels, rounded to 0.01 px.
fn px(m: f64) -> f64 {
    (m / METRES_PER_PIXEL * 100.0).round() / 100.0
}

fn rel_points(pts: &[[f32; 2]], x: f64, y: f64) -> Vec<Pt> {
    pts.iter()
        .map(|p| Pt {
            x: ((px(p[0] as f64) - x) * 100.0).round() / 100.0,
            y: ((px(p[1] as f64) - y) * 100.0).round() / 100.0,
        })
        .collect()
}

fn shape_object(o: Object, shape: &Shape) -> Object {
    match shape {
        Shape::Rect(r) => {
            let (x, y) = (px(r.x0 as f64), px(r.y0 as f64));
            Object {
                x,
                y,
                width: px(r.x1 as f64) - x,
                height: px(r.y1 as f64) - y,
                ..o
            }
        }
        Shape::Poly(pts) => {
            let (x, y) = (px(pts[0][0] as f64), px(pts[0][1] as f64));
            Object {
                x,
                y,
                polygon: Some(rel_points(pts, x, y)),
                ..o
            }
        }
        Shape::Circle(c, r) => {
            let (x, y) = (px((c[0] - r) as f64), px((c[1] - r) as f64));
            Object {
                x,
                y,
                width: px(2.0 * *r as f64),
                height: px(2.0 * *r as f64),
                ellipse: true,
                ..o
            }
        }
    }
}

/// The embedded ground tileset: one 16 px tile per ground kind, drawn from
/// `ground.png` next to the map.
fn ground_tileset() -> Tileset {
    Tileset {
        firstgid: 1,
        name: "ground".into(),
        tilewidth: TILE_PX,
        tileheight: TILE_PX,
        tilecount: GROUND_NAMES.len() as u32,
        columns: GROUND_NAMES.len() as u32,
        image: "ground.png".into(),
        imagewidth: TILE_PX * GROUND_NAMES.len() as u32,
        imageheight: TILE_PX,
        tiles: GROUND_NAMES
            .iter()
            .enumerate()
            .map(|(id, g)| TileDef {
                id: id as u32,
                properties: vec![Property::string("ground", g)],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// RGB of each ground kind in `ground.png`.
pub const GROUND_COLOURS: [[u8; 3]; 5] = [
    [0xb9, 0xb5, 0xad],
    [0x5a, 0x5d, 0x62],
    [0x86, 0xb0, 0x5e],
    [0xdc, 0xcb, 0xa4],
    [0x4f, 0x86, 0xc6],
];

/// `ground.png`, the image of the ground tileset: one flat 16 px tile per
/// ground kind, in tile id order.
pub fn ground_png() -> Vec<u8> {
    let (w, h) = (TILE_PX * GROUND_COLOURS.len() as u32, TILE_PX);
    // Each row: filter type 0 (none), then RGB pixels.
    let row: Vec<u8> = std::iter::once(0)
        .chain((0..w).flat_map(|x| GROUND_COLOURS[(x / TILE_PX) as usize]))
        .collect();
    let raw = row.repeat(h as usize);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(b"IHDR", &ihdr);
    chunk(
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 9),
    );
    chunk(b"IEND", &[]);
    png
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}
