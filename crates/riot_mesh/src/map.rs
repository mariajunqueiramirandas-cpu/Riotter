//! Static map geometry (DESIGN §18.2 layers 1 and 7), built once from the
//! [`MapDef`] as textured pixel-art meshes ([`crate::mesh`]):
//!
//! - **Ground**: pavement slabs, asphalt, setts, grass and water from the
//!   painted ground layer (or the road graph), granite kerbs and gutters
//!   along every road edge, road markings and zebra crossings, decals
//!   (manholes, drains, stains, cracks, leaves, litter), and shadows swept
//!   from buildings, walls and trees towards the south-east.
//! - **Roofs**: hipped roofs in clay tiles or slate with lit facets, flat
//!   roofs in gravel, bitumen or metal behind concrete parapets, with
//!   rooftop plant and its shadows, walls and fences, tree canopies (a
//!   little translucent) and arcades (dithered, so people show through),
//!   and one-pixel outlines.
//!
//! Each has a detailed and a simplified version for far zoom. Everything is
//! placed by hashes of map data, so a map always looks the same.

use riot_art::{self as art, Rgb};
use riot_defs::mapdef::{Ground, MapDef, Polygon, SolidKind};
use riot_materials::Mat;
use riot_sheets::{Decal, DecalAtlas};

use crate::mesh::{
    self, MeshBuilder, Pass, Rgba, Tex, WHITE, centroid, inset, is_convex, point_in_polygon, rgba,
    signed_area2,
};

/// Background colour outside the map.
pub const OUTSIDE: [u8; 4] = [22, 24, 28, 255];

/// Road width drawn from the road graph when the map has no ground layer.
const ROAD_W: f32 = 7.0;

/// Below this zoom (px/m) the simplified meshes are drawn: details there
/// would be smaller than a pixel.
pub const DETAIL_ZOOM: f32 = 1.0;

/// Shadows: a cool, translucent blue-black.
const SHADOW: Rgba = [18, 22, 48, 92];
/// Shadow offset per metre of height (light from the north-west).
const SUN: [f32; 2] = [0.42, 0.58];
const OUTLINE: Rgba = [26, 22, 30, 255];

fn hash(i: u32) -> u32 {
    let mut x = i.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA77;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^ (x >> 12)
}

/// A hash of a position, for placing details.
fn hash_at(p: [f32; 2], salt: u32) -> u32 {
    art::hash((p[0] * 4.0) as i32, (p[1] * 4.0) as i32, salt)
}

fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

fn tint(c: Rgb) -> Rgba {
    rgba(c, 255)
}

fn scale(c: Rgba, k: f32) -> Rgba {
    let f = |v: u8| (v as f32 * k).clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

fn ground_mat(g: u8) -> Mat {
    match g {
        x if x == Ground::Road as u8 => Mat::Asphalt,
        x if x == Ground::Grass as u8 => Mat::Grass,
        x if x == Ground::Plaza as u8 => Mat::Plaza,
        x if x == Ground::Water as u8 => Mat::Water,
        _ => Mat::Pavement,
    }
}

/// Is a polygon of the overhead layer a tree canopy (an ellipse) rather
/// than an arcade (a rectangle)?
fn is_canopy(p: &Polygon) -> bool {
    p.points.len() > 6
}

/// Ground layers, bottom to top.
mod gl {
    pub const BASE: usize = 0;
    pub const CELLS: usize = 1;
    pub const AREAS: usize = 2;
    pub const KERBS: usize = 3;
    pub const WATER: usize = 4;
    pub const MARKINGS: usize = 5;
    pub const DECALS: usize = 6;
    pub const SHADOWS: usize = 7;
}

/// Roof layers, bottom to top.
mod rl {
    pub const BASE: usize = 0;
    pub const SURFACE: usize = 1;
    pub const SHADOWS: usize = 2;
    pub const OBJECTS: usize = 3;
    pub const LINES: usize = 4;
    pub const CANOPY: usize = 5;
    pub const CANOPY_LINES: usize = 6;
}

/// The ground mesh. Without `detail`: no kerbs, markings, decals or
/// shadows.
pub fn build_ground(map: &MapDef, detail: bool, decals: &DecalAtlas) -> MeshBuilder {
    let mut m = MeshBuilder::default();
    let (hw, hh) = (map.width * 0.5, map.height * 0.5);
    m.mat(gl::BASE, Mat::Pavement).rect(-hw, -hh, hw, hh, WHITE);
    let g = &map.ground;
    let painted = g.cell > 0.0 && !g.kinds.is_empty();
    if painted {
        for y in 0..g.h as usize {
            let mut x = 0usize;
            while x < g.w as usize {
                let k = g.kinds[y * g.w as usize + x];
                let mut e = x + 1;
                while e < g.w as usize && g.kinds[y * g.w as usize + e] == k {
                    e += 1;
                }
                if k != Ground::Pavement as u8 {
                    m.mat(gl::CELLS, ground_mat(k)).rect(
                        -hw + x as f32 * g.cell,
                        -hh + y as f32 * g.cell,
                        -hw + e as f32 * g.cell,
                        -hh + (y + 1) as f32 * g.cell,
                        WHITE,
                    );
                }
                x = e;
            }
        }
        if detail {
            kerbs(map, &mut m);
        }
    }
    // Named areas: a paved centrepiece on squares, grass in parks.
    for a in &map.areas {
        match a.tag.as_str() {
            "square" => {
                let r = a.radius * if painted { 0.9 } else { 1.45 };
                if !painted {
                    m.mat(gl::AREAS, Mat::Plaza).disc(a.pos, r, WHITE);
                }
                if detail {
                    // A compass of darker setts at the centre.
                    let dark = tint([226, 218, 206]);
                    for k in [2.4, 5.5] {
                        m.mat(gl::AREAS, Mat::Plaza).ring(a.pos, k, 0.35, dark);
                    }
                    for q in 0..8 {
                        let ang = q as f32 * std::f32::consts::FRAC_PI_4;
                        let len = if q % 2 == 0 { 7.5 } else { 4.5 };
                        let tip = [a.pos[0] + ang.cos() * len, a.pos[1] + ang.sin() * len];
                        let base = [a.pos[0] + ang.cos() * 2.4, a.pos[1] + ang.sin() * 2.4];
                        m.mat(gl::AREAS, Mat::Plaza).line(base, tip, 0.3, dark);
                    }
                    m.mat(gl::AREAS, Mat::Concrete).disc(a.pos, 1.2, WHITE);
                    m.mat(gl::AREAS, Mat::Water).disc(a.pos, 0.9, WHITE);
                }
            }
            "park" if !painted => m.mat(gl::AREAS, Mat::Grass).disc(a.pos, a.radius, WHITE),
            "government" if !painted => {
                m.mat(gl::AREAS, Mat::Plaza)
                    .disc(a.pos, a.radius, tint([228, 224, 216]))
            }
            _ => {}
        }
    }
    if !painted {
        roads_from_graph(map, &mut m, detail);
    }
    for p in &map.solids {
        if p.kind == SolidKind::Water {
            m.mat(gl::WATER, Mat::Water).polygon(&p.points, WHITE);
            if detail {
                m.mat(gl::WATER, Mat::Concrete)
                    .outline(&p.points, 0.6, WHITE);
            }
        }
    }
    if detail {
        markings(map, &mut m, painted);
        ground_decals(map, &mut m, decals, painted);
        ground_shadows(map, &mut m);
    }
    m
}

/// Kerb stones on the pavement side and a gutter on the road side of every
/// edge between a road cell and a walkable non-road cell.
fn kerbs(map: &MapDef, m: &mut MeshBuilder) {
    let g = &map.ground;
    let (hw, hh) = (map.width * 0.5, map.height * 0.5);
    let (w, h) = (g.w as i64, g.h as i64);
    let kind = |x: i64, y: i64| -> u8 {
        if x < 0 || y < 0 || x >= w || y >= h {
            Ground::Pavement as u8
        } else {
            g.kinds[(y * w + x) as usize]
        }
    };
    let road = Ground::Road as u8;
    let edge = |a: u8, b: u8| a == road && b != road && b != Ground::Water as u8;
    let gutter = tint([168, 168, 172]);
    // Horizontal edges between rows y-1 and y, merged along x; then the
    // same for vertical edges.
    for vertical in [false, true] {
        let (outer, inner) = if vertical { (w + 1, h) } else { (h + 1, w) };
        for o in 0..outer {
            for side in [false, true] {
                let mut i = 0;
                while i < inner {
                    let at = |i: i64| {
                        let (a, b) = if vertical {
                            (kind(o - 1, i), kind(o, i))
                        } else {
                            (kind(i, o - 1), kind(i, o))
                        };
                        if side { edge(b, a) } else { edge(a, b) }
                    };
                    if !at(i) {
                        i += 1;
                        continue;
                    }
                    let mut e = i + 1;
                    while e < inner && at(e) {
                        e += 1;
                    }
                    // Road on the low side (`!side`) or the high side.
                    let line = -hh + o as f32 * g.cell;
                    let linex = -hw + o as f32 * g.cell;
                    let (s, t) = (i as f32 * g.cell, e as f32 * g.cell);
                    let dir = if side { -1.0 } else { 1.0 };
                    let kerb_c = 0.15 * dir;
                    let gut_c = -0.14 * dir;
                    if vertical {
                        let (y0, y1) = (-hh + s, -hh + t);
                        m.mat(gl::KERBS, Mat::Kerb).strip(
                            [linex + kerb_c, y0],
                            [linex + kerb_c, y1],
                            0.3,
                            WHITE,
                        );
                        m.mat(gl::KERBS, Mat::Asphalt).strip(
                            [linex + gut_c, y0],
                            [linex + gut_c, y1],
                            0.28,
                            gutter,
                        );
                    } else {
                        let (x0, x1) = (-hw + s, -hw + t);
                        m.mat(gl::KERBS, Mat::Kerb).strip(
                            [x0, line + kerb_c],
                            [x1, line + kerb_c],
                            0.3,
                            WHITE,
                        );
                        m.mat(gl::KERBS, Mat::Asphalt).strip(
                            [x0, line + gut_c],
                            [x1, line + gut_c],
                            0.28,
                            gutter,
                        );
                    }
                    i = e;
                }
            }
        }
    }
}

/// Road graph edges that are drawn, with node degrees.
fn road_edges(map: &MapDef) -> (Vec<(usize, usize)>, Vec<u32>) {
    let r = &map.roads;
    let mut degree = vec![0u32; r.nodes.len()];
    let mut edges = Vec::new();
    for e in &r.edges {
        let (i, j) = (e[0] as usize, e[1] as usize);
        if i >= r.nodes.len() || j >= r.nodes.len() || i == j {
            continue;
        }
        let (a, b) = (r.nodes[i], r.nodes[j]);
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        if in_plaza(map, mid) {
            continue;
        }
        degree[i] += 1;
        degree[j] += 1;
        edges.push((i, j));
    }
    (edges, degree)
}

fn in_plaza(map: &MapDef, p: [f32; 2]) -> bool {
    map.areas.iter().any(|a| {
        a.tag == "square" && {
            let (dx, dy) = (p[0] - a.pos[0], p[1] - a.pos[1]);
            dx * dx + dy * dy < (a.radius * 1.45).powi(2)
        }
    })
}

/// Asphalt and kerbs along the road graph, for maps without a ground layer.
fn roads_from_graph(map: &MapDef, m: &mut MeshBuilder, detail: bool) {
    let r = &map.roads;
    let (edges, degree) = road_edges(map);
    if detail {
        for &(i, j) in &edges {
            m.mat(gl::CELLS, Mat::Kerb)
                .line(r.nodes[i], r.nodes[j], ROAD_W + 0.7, WHITE);
        }
        for (i, &d) in degree.iter().enumerate() {
            if d > 0 {
                let p = r.nodes[i];
                let h = (ROAD_W + 0.7) * 0.5;
                m.mat(gl::CELLS, Mat::Kerb)
                    .rect(p[0] - h, p[1] - h, p[0] + h, p[1] + h, WHITE);
            }
        }
    }
    for &(i, j) in &edges {
        m.mat(gl::KERBS, Mat::Asphalt)
            .line(r.nodes[i], r.nodes[j], ROAD_W, WHITE);
    }
    for (i, &d) in degree.iter().enumerate() {
        if d > 0 {
            let p = r.nodes[i];
            let h = ROAD_W * 0.5;
            m.mat(gl::KERBS, Mat::Asphalt)
                .rect(p[0] - h, p[1] - h, p[0] + h, p[1] + h, WHITE);
        }
    }
}

/// Width of the road at `p` across direction `n`, and the offset of its
/// centre from `p` (painted maps), or the graph road width.
fn road_span(map: &MapDef, painted: bool, p: [f32; 2], n: [f32; 2]) -> Option<(f32, f32)> {
    if !painted {
        return Some((ROAD_W, 0.0));
    }
    let is_road = |o: f32| map.ground_at(p[0] + n[0] * o, p[1] + n[1] * o) == Ground::Road;
    if !is_road(0.0) {
        return None;
    }
    let (mut lo, mut hi) = (0.0f32, 0.0f32);
    while hi < 30.0 && is_road(hi + 0.5) {
        hi += 0.5;
    }
    while lo > -30.0 && is_road(lo - 0.5) {
        lo -= 0.5;
    }
    // Cells are 4 m: snap to the edges between them.
    let (lo, hi) = (lo - 0.25, hi + 0.25);
    Some((hi - lo, (hi + lo) * 0.5))
}

/// Centre and lane lines, and zebra crossings at junctions.
fn markings(map: &MapDef, m: &mut MeshBuilder, painted: bool) {
    let r = &map.roads;
    let (edges, degree) = road_edges(map);
    let white = tint([238, 236, 226]);
    for &(i, j) in &edges {
        let (a, b) = (r.nodes[i], r.nodes[j]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 12.0 {
            continue;
        }
        let u = [d[0] / l, d[1] / l];
        let n = [-u[1], u[0]];
        let mid = [a[0] + d[0] * 0.5, a[1] + d[1] * 0.5];
        let Some((w, off)) = road_span(map, painted, mid, n) else {
            continue;
        };
        let at = |t: f32, o: f32| {
            [
                a[0] + u[0] * t + n[0] * (o + off),
                a[1] + u[1] * t + n[1] * (o + off),
            ]
        };
        let on_road = |p: [f32; 2]| !painted || map.ground_at(p[0], p[1]) == Ground::Road;
        // Junction boxes: how far from each end the crossing road reaches.
        let clear = |from_a: bool| -> f32 {
            let mut t = 0.0;
            while t < l * 0.5 {
                let tt = if from_a { t } else { l - t };
                let side = w * 0.5 + 1.0;
                if !on_road(at(tt, side)) && !on_road(at(tt, -side)) {
                    break;
                }
                t += 0.5;
            }
            t
        };
        let (ca, cb) = if painted {
            (clear(true), clear(false))
        } else {
            (ROAD_W * 0.5, ROAD_W * 0.5)
        };
        let (t0, t1) = (ca + 4.5, l - cb - 4.5);
        let dash = |m: &mut MeshBuilder, o: f32, len: f32, gap: f32| {
            let mut t = t0;
            while t + len < t1 {
                let (p, q) = (at(t, o), at(t + len, o));
                if on_road(p) && on_road(q) {
                    m.mat(gl::MARKINGS, Mat::Paint).line(p, q, 0.15, white);
                }
                t += len + gap;
            }
        };
        if w >= 12.0 {
            // Avenue: a double centre line and lane lines.
            for o in [-0.15, 0.15] {
                if t1 > t0 {
                    m.mat(gl::MARKINGS, Mat::Paint)
                        .line(at(t0, o), at(t1, o), 0.12, white);
                }
            }
            dash(m, -w * 0.25, 3.0, 4.0);
            dash(m, w * 0.25, 3.0, 4.0);
        } else if w >= 6.0 {
            dash(m, 0.0, 3.0, 3.0);
        }
        // Zebra crossings at busy junctions, stop lines before them.
        if w < 6.0 {
            continue;
        }
        for (end, deg, c) in [(false, degree[i], ca), (true, degree[j], cb)] {
            if deg < 3 {
                continue;
            }
            let t = if end { l - c - 3.0 } else { c + 0.5 };
            let mut o = -w * 0.5 + 0.7;
            while o < w * 0.5 - 0.4 {
                m.mat(gl::MARKINGS, Mat::Paint)
                    .line(at(t, o), at(t + 2.5, o), 0.5, white);
                o += 1.0;
            }
            let ts = if end { t - 1.0 } else { t + 3.5 };
            let (s0, s1) = if end {
                (-w * 0.5 + 0.3, -0.2)
            } else {
                (0.2, w * 0.5 - 0.3)
            };
            m.mat(gl::MARKINGS, Mat::Paint)
                .line(at(ts, s0), at(ts, s1), 0.3, white);
        }
    }
}

/// Manholes and stains on roads, drains by the kerbs, cracks on pavements,
/// leaves under trees and litter on squares.
fn ground_decals(map: &MapDef, m: &mut MeshBuilder, decals: &DecalAtlas, painted: bool) {
    let mut put = |d: Decal, p: [f32; 2], a: f32| {
        let s = decals.get(d);
        let half = [s.body[0] * 0.5, s.body[1] * 0.5];
        m.at(gl::DECALS, Tex::Decals, Pass::Normal)
            .sprite(&s, p, [a.cos(), a.sin()], half, WHITE);
    };
    let g = &map.ground;
    let (hw, hh) = (map.width * 0.5, map.height * 0.5);
    if painted {
        for y in 0..g.h as i64 {
            for x in 0..g.w as i64 {
                let k = g.kinds[(y * g.w as i64 + x) as usize];
                let c = [
                    -hw + (x as f32 + 0.5) * g.cell,
                    -hh + (y as f32 + 0.5) * g.cell,
                ];
                let h = art::hash(x as i32, y as i32, 501);
                let jitter = |s: u32| (unit(hash(h ^ s)) - 0.5) * g.cell * 0.7;
                let p = [c[0] + jitter(1), c[1] + jitter(2)];
                let a = unit(hash(h ^ 3)) * std::f32::consts::TAU;
                match k {
                    x if x == Ground::Road as u8 => {
                        if h.is_multiple_of(53) {
                            put(Decal::Manhole, p, a);
                        } else if h % 41 == 1 {
                            put(Decal::Stain, p, a);
                        } else if h % 97 == 2 {
                            put(Decal::Crack, p, a);
                        }
                    }
                    x if x == Ground::Pavement as u8 => {
                        if h.is_multiple_of(37) {
                            put(Decal::Crack, p, a);
                        } else if h % 61 == 1 {
                            put(Decal::Litter, p, a);
                        }
                    }
                    x if x == Ground::Plaza as u8 && h.is_multiple_of(29) => {
                        put(Decal::Litter, p, a);
                    }
                    _ => {}
                }
                // Drains along the kerb, every few cells.
                if k == Ground::Road as u8 && h.is_multiple_of(5) {
                    let n = |dx: i64, dy: i64| {
                        let (xx, yy) = (x + dx, y + dy);
                        xx >= 0
                            && yy >= 0
                            && xx < g.w as i64
                            && yy < g.h as i64
                            && g.kinds[(yy * g.w as i64 + xx) as usize] == Ground::Pavement as u8
                    };
                    for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                        if n(dx, dy) {
                            let e = [
                                c[0] + dx as f32 * (g.cell * 0.5 - 0.42),
                                c[1] + dy as f32 * (g.cell * 0.5 - 0.42),
                            ];
                            let a = if dx != 0 {
                                std::f32::consts::FRAC_PI_2
                            } else {
                                0.0
                            };
                            put(Decal::Drain, e, a);
                            break;
                        }
                    }
                }
            }
        }
    } else {
        let r = &map.roads;
        let (edges, _) = road_edges(map);
        for &(i, j) in &edges {
            let (a, b) = (r.nodes[i], r.nodes[j]);
            let d = [b[0] - a[0], b[1] - a[1]];
            let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
            let mut t = 15.0;
            while t < l - 15.0 {
                let p = [a[0] + d[0] * t / l, a[1] + d[1] * t / l];
                put(Decal::Manhole, p, 0.0);
                t += 40.0;
            }
        }
    }
    for o in &map.overhead {
        if !is_canopy(o) {
            continue;
        }
        let c = centroid(&o.points);
        let h = hash_at(c, 502);
        if h.is_multiple_of(3) {
            let p = [
                c[0] + (unit(h) - 0.5) * 3.0,
                c[1] + (unit(hash(h)) - 0.5) * 3.0,
            ];
            put(Decal::Leaves, p, unit(hash(h ^ 9)) * 6.0);
        }
    }
}

/// A shadow swept from a footprint by `d` (the footprint moved, and the
/// band between). Depth testing keeps the overlaps from darkening twice.
fn sweep(part: &mut mesh::Part, pts: &[[f32; 2]], d: [f32; 2], c: Rgba) {
    let moved: Vec<[f32; 2]> = pts.iter().map(|p| [p[0] + d[0], p[1] + d[1]]).collect();
    part.polygon(&moved, c);
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        part.quad(
            [a, b, [b[0] + d[0], b[1] + d[1]], [a[0] + d[0], a[1] + d[1]]],
            c,
        );
    }
}

/// Building, wall, tree and arcade shadows on the ground.
fn ground_shadows(map: &MapDef, m: &mut MeshBuilder) {
    for (i, p) in map.solids.iter().enumerate() {
        let height = match p.kind {
            SolidKind::Building => 3.5 + unit(hash(i as u32 ^ 0x5bd1)) * 3.5,
            SolidKind::Wall => 2.2,
            SolidKind::Fence => 1.2,
            SolidKind::Water => continue,
        };
        let d = [SUN[0] * height, SUN[1] * height];
        sweep(m.shadow(gl::SHADOWS), &p.points, d, SHADOW);
    }
    for o in &map.overhead {
        let height = if is_canopy(o) { 3.0 } else { 2.6 };
        let d = [SUN[0] * height, SUN[1] * height];
        let moved: Vec<[f32; 2]> = o
            .points
            .iter()
            .map(|p| [p[0] + d[0], p[1] + d[1]])
            .collect();
        m.shadow(gl::SHADOWS).polygon(&moved, SHADOW);
    }
}

/// The roof mesh. Without `detail`: flat fills, no shadows, plant or
/// lines.
pub fn build_roofs(map: &MapDef, detail: bool, decals: &DecalAtlas) -> MeshBuilder {
    let mut m = MeshBuilder::default();
    for (i, p) in map.solids.iter().enumerate() {
        match p.kind {
            SolidKind::Building => building(&mut m, p, hash(i as u32), detail, decals),
            SolidKind::Wall => {
                m.mat(rl::BASE, Mat::Concrete)
                    .polygon(&p.points, tint([200, 200, 206]));
                if detail {
                    m.mat(rl::LINES, Mat::Solid)
                        .px_outline(&p.points, 1.0, OUTLINE);
                }
            }
            SolidKind::Fence => {
                m.mat(rl::BASE, Mat::Wood).polygon(&p.points, WHITE);
                if detail {
                    m.mat(rl::LINES, Mat::Solid)
                        .px_outline(&p.points, 1.0, [52, 36, 24, 255]);
                }
            }
            SolidKind::Water => {}
        }
    }
    if detail {
        // Street lamps: a dark head with a pale lens.
        for l in crate::lamps::street_lamps(map) {
            m.mat(rl::LINES, Mat::Solid)
                .disc(l.pos, 0.2, [44, 46, 52, 255]);
            m.mat(rl::CANOPY_LINES, Mat::Solid)
                .disc(l.pos, 0.1, [250, 236, 190, 255]);
        }
    }
    for (i, o) in map.overhead.iter().enumerate() {
        let h = hash(i as u32 ^ 0x77);
        if is_canopy(o) {
            // Varied greens, a dark rim and a lit north-west edge.
            let shades = [
                [255, 255, 255],
                [236, 255, 214],
                [214, 236, 246],
                [255, 244, 214],
            ];
            let t = tint(shades[(h % 4) as usize]);
            if detail {
                // Nearly opaque: people underneath show through faintly.
                m.mat(rl::CANOPY, Mat::Foliage)
                    .polygon(&o.points, [t[0], t[1], t[2], 238]);
                let rim = [24, 44, 22, 255];
                let lit = [150, 196, 98, 255];
                let pts = &o.points;
                let ccw = signed_area2(pts) > 0.0;
                for k in 0..pts.len() {
                    let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                    m.mat(rl::CANOPY_LINES, Mat::Solid).px_line(a, b, 1.0, rim);
                    // Outward normal facing the light: a highlight inside.
                    let e = [b[0] - a[0], b[1] - a[1]];
                    let n = if ccw { [e[1], -e[0]] } else { [-e[1], e[0]] };
                    if n[0] * art::LIGHT[0] + n[1] * art::LIGHT[1] > 0.25 * (e[0].hypot(e[1])) {
                        let inward = if ccw { 1.5 } else { -1.5 };
                        m.mat(rl::CANOPY_LINES, Mat::Solid)
                            .px_line_off(a, b, 1.0, inward, lit);
                    }
                }
            } else {
                m.mat(rl::CANOPY, Mat::Foliage).polygon(&o.points, t);
            }
        } else if detail {
            m.at(rl::CANOPY, Tex::Mat(Mat::Metal), Pass::Dither)
                .polygon(&o.points, [214, 222, 232, 190]);
            m.mat(rl::CANOPY_LINES, Mat::Solid)
                .px_outline(&o.points, 1.0, OUTLINE);
        } else {
            m.mat(rl::CANOPY, Mat::Metal).polygon(&o.points, WHITE);
        }
    }
    m
}

/// Roofing of a building, from its hash.
#[derive(Clone, Copy, Debug)]
enum Roof {
    Hipped(Mat, Rgba),
    Flat(Mat, Rgba, Rgba),
}

fn roof_style(p: &Polygon, h: u32) -> Roof {
    let pts = &p.points;
    let area = signed_area2(pts).abs() * 0.5;
    if pts.len() == 4 && (h >> 8) % 100 < 45 && is_convex(pts) && area < 900.0 {
        let tiles = [
            (Mat::Tiles, [255, 255, 255]),
            (Mat::Tiles, [214, 186, 170]),
            (Mat::Tiles, [236, 206, 196]),
            (Mat::Slate, [255, 255, 255]),
            (Mat::Slate, [226, 236, 255]),
        ];
        let (mat, t) = tiles[((h >> 4) % 5) as usize];
        Roof::Hipped(mat, tint(t))
    } else {
        let surface = match (h >> 4) % 20 {
            0..=8 => Mat::Gravel,
            9..=15 => Mat::Tar,
            _ if area > 400.0 => Mat::Metal,
            _ => Mat::Gravel,
        };
        let parapets = [
            [255, 255, 255],
            [240, 226, 206],
            [222, 228, 240],
            [236, 214, 204],
        ];
        let surface_tint = [[255, 255, 255], [238, 234, 226], [226, 230, 236]];
        Roof::Flat(
            surface,
            tint(surface_tint[((h >> 9) % 3) as usize]),
            tint(parapets[((h >> 12) % 4) as usize]),
        )
    }
}

fn building(m: &mut MeshBuilder, p: &Polygon, h: u32, detail: bool, decals: &DecalAtlas) {
    let pts = &p.points;
    if pts.len() < 3 {
        return;
    }
    match roof_style(p, h) {
        Roof::Hipped(mat, t) => {
            hipped_roof(m, pts, mat, t, detail);
            if detail {
                let c = centroid(pts);
                let hh = hash(h ^ 0x51);
                if hh % 5 < 3 {
                    let s = decals.get(Decal::Chimney);
                    let q = [
                        c[0] + (unit(hh) - 0.5) * 3.0,
                        c[1] + (unit(hash(hh)) - 0.5) * 3.0,
                    ];
                    if point_in_polygon(q, &inset(pts, 1.5)) {
                        rooftop(m, &s, Decal::Chimney, q, [1.0, 0.0]);
                    }
                }
            }
        }
        Roof::Flat(surface, st, pt) => {
            let area = signed_area2(pts).abs() * 0.5;
            m.mat(rl::BASE, Mat::Concrete).polygon(pts, pt);
            let inner = inset(pts, 0.45);
            let inner_ok = signed_area2(&inner).abs() * 0.5 > area * 0.3;
            if inner_ok {
                m.mat(rl::SURFACE, surface).polygon_uv(&inner, st, |q| {
                    // Membranes and sheets follow the building's long side.
                    let (u, v) = long_axis(pts);
                    let k = 1.0 / riot_materials::TILE_M;
                    [
                        (q[0] * u[0] + q[1] * u[1]) * k,
                        (q[0] * v[0] + q[1] * v[1]) * k,
                    ]
                });
            }
            if detail && inner_ok {
                parapet_shadows(m, &inner);
                rooftop_plant(m, pts, &inner, h, decals);
            }
        }
    }
    if detail {
        // A dark outline, and the rim catching the light on its north-west.
        m.mat(rl::LINES, Mat::Solid).px_outline(pts, 1.0, OUTLINE);
        let ccw = signed_area2(pts) > 0.0;
        for k in 0..pts.len() {
            let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
            let e = [b[0] - a[0], b[1] - a[1]];
            let l = e[0].hypot(e[1]);
            let n = if ccw { [e[1], -e[0]] } else { [-e[1], e[0]] };
            if n[0] * art::LIGHT[0] + n[1] * art::LIGHT[1] > 0.3 * l {
                let inward = if ccw { 1.0 } else { -1.0 };
                m.mat(rl::LINES, Mat::Solid)
                    .px_line_off(a, b, 1.0, inward, [255, 250, 232, 90]);
            }
        }
    }
}

/// Unit vectors along and across a polygon's longest edge.
fn long_axis(pts: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let mut best = ([1.0, 0.0], 0.0);
    for k in 0..pts.len() {
        let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
        let e = [b[0] - a[0], b[1] - a[1]];
        let l = e[0].hypot(e[1]);
        if l > best.1 {
            best = ([e[0] / l, e[1] / l], l);
        }
    }
    let u = best.0;
    (u, [-u[1], u[0]])
}

/// Shadows the parapet casts on the roof inside it: along inner edges on
/// the north-west side.
fn parapet_shadows(m: &mut MeshBuilder, inner: &[[f32; 2]]) {
    let ccw = signed_area2(inner) > 0.0;
    for k in 0..inner.len() {
        let (a, b) = (inner[k], inner[(k + 1) % inner.len()]);
        let e = [b[0] - a[0], b[1] - a[1]];
        let l = e[0].hypot(e[1]);
        if l < 1e-3 {
            continue;
        }
        // Inward normal.
        let n = if ccw {
            [-e[1] / l, e[0] / l]
        } else {
            [e[1] / l, -e[0] / l]
        };
        // Shadow falls inward where the inward normal points away from the
        // light, i.e. along the sun.
        let facing = n[0] * SUN[0] + n[1] * SUN[1];
        if facing <= 0.1 {
            continue;
        }
        let w = 0.35;
        m.shadow(rl::SHADOWS).quad(
            [
                a,
                b,
                [b[0] + n[0] * w, b[1] + n[1] * w],
                [a[0] + n[0] * w, a[1] + n[1] * w],
            ],
            SHADOW,
        );
    }
}

/// A rooftop object and its shadow.
fn rooftop(m: &mut MeshBuilder, s: &riot_sheets::Sprite, d: Decal, c: [f32; 2], cs: [f32; 2]) {
    let half = [s.body[0] * 0.5, s.body[1] * 0.5];
    let hgt = d.height();
    if hgt > 0.0 {
        let off = [c[0] + SUN[0] * hgt, c[1] + SUN[1] * hgt];
        let sh = [half[0] * 0.95, half[1] * 0.95];
        m.shadow(rl::SHADOWS).obb(off, cs, sh, SHADOW);
    }
    m.at(rl::OBJECTS, Tex::Decals, Pass::Normal)
        .sprite(s, c, cs, half, WHITE);
}

/// Air conditioning, vents, skylights, panels, tanks and stair huts on a
/// flat roof, placed by hash where they fit.
fn rooftop_plant(
    m: &mut MeshBuilder,
    pts: &[[f32; 2]],
    inner: &[[f32; 2]],
    h: u32,
    decals: &DecalAtlas,
) {
    let area = signed_area2(inner).abs() * 0.5;
    if area < 30.0 {
        return;
    }
    let (u, v) = long_axis(pts);
    let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for p in inner {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let want = ((area / 70.0) as usize).clamp(1, 7);
    let mut placed: Vec<([f32; 2], f32)> = Vec::new();
    let solar_roof = h.is_multiple_of(7);
    for k in 0..want as u32 * 6 {
        if placed.len() >= want {
            break;
        }
        let r = hash(h ^ (k.wrapping_mul(0x9E37) + 1));
        let d = if solar_roof {
            Decal::Solar
        } else {
            match r % 100 {
                0..=34 => Decal::AirCon,
                35..=59 => Decal::Vent,
                60..=74 => Decal::Skylight,
                75..=82 => Decal::Solar,
                83..=90 => Decal::Tank,
                _ if area > 150.0 => Decal::Hut,
                _ => Decal::Vent,
            }
        };
        let s = decals.get(d);
        let c = [
            lo[0] + unit(hash(r)) * (hi[0] - lo[0]),
            lo[1] + unit(hash(r ^ 7)) * (hi[1] - lo[1]),
        ];
        let half = [s.body[0] * 0.5 + 0.3, s.body[1] * 0.5 + 0.3];
        let corners = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]].map(|q: [f32; 2]| {
            [
                c[0] + u[0] * half[0] * q[0] + v[0] * half[1] * q[1],
                c[1] + u[1] * half[0] * q[0] + v[1] * half[1] * q[1],
            ]
        });
        if !corners.iter().all(|&q| point_in_polygon(q, inner)) {
            continue;
        }
        let r_needed = half[0].hypot(half[1]);
        if placed
            .iter()
            .any(|&(p, rr)| (p[0] - c[0]).hypot(p[1] - c[1]) < rr + r_needed)
        {
            continue;
        }
        placed.push((c, r_needed));
        rooftop(m, &s, d, c, u);
    }
}

/// A hipped roof over a convex quadrilateral: a ridge along the long axis
/// and four facets, each lit by how it faces the north-west light, with
/// courses running along its eave.
fn hipped_roof(m: &mut MeshBuilder, pts: &[[f32; 2]], mat: Mat, t: Rgba, detail: bool) {
    let c = centroid(pts);
    let e0 = [pts[1][0] - pts[0][0], pts[1][1] - pts[0][1]];
    let e1 = [pts[2][0] - pts[1][0], pts[2][1] - pts[1][1]];
    let (l0, l1) = (e0[0].hypot(e0[1]), e1[0].hypot(e1[1]));
    let (u, long, short) = if l0 >= l1 {
        ([e0[0] / l0, e0[1] / l0], l0, l1)
    } else {
        ([e1[0] / l1, e1[1] / l1], l1, l0)
    };
    let half_ridge = ((long - short) * 0.5).max(0.0);
    let r0 = [c[0] - u[0] * half_ridge, c[1] - u[1] * half_ridge];
    let r1 = [c[0] + u[0] * half_ridge, c[1] + u[1] * half_ridge];
    let nearest = |p: [f32; 2]| {
        let d = |r: [f32; 2]| (r[0] - p[0]).powi(2) + (r[1] - p[1]).powi(2);
        if d(r0) <= d(r1) { r0 } else { r1 }
    };
    let k = 1.0 / riot_materials::TILE_M;
    for i in 0..4 {
        let (a, b) = (pts[i], pts[(i + 1) % 4]);
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let out = [mid[0] - c[0], mid[1] - c[1]];
        let ol = out[0].hypot(out[1]).max(1e-6);
        let lit = (out[0] * art::LIGHT[0] + out[1] * art::LIGHT[1]) / ol;
        let col = scale(t, 0.84 + 0.16 * lit);
        // Courses along the eave: u along a → b, v up the slope.
        let el = (b[0] - a[0]).hypot(b[1] - a[1]).max(1e-6);
        let eu = [(b[0] - a[0]) / el, (b[1] - a[1]) / el];
        let ev = [-out[0] / ol, -out[1] / ol];
        let uv = |q: [f32; 2]| {
            [
                (q[0] * eu[0] + q[1] * eu[1]) * k,
                (q[0] * ev[0] + q[1] * ev[1]) * k,
            ]
        };
        let (ra, rb) = (nearest(a), nearest(b));
        let facet: Vec<[f32; 2]> = if ra == rb {
            vec![a, b, ra]
        } else {
            vec![a, b, rb, ra]
        };
        m.mat(rl::BASE, mat).polygon_uv(&facet, col, uv);
    }
    if detail {
        let ridge = scale(t, 0.5);
        let line = [ridge[0], ridge[1], ridge[2], 255];
        m.mat(rl::LINES, Mat::Solid).px_line(r0, r1, 1.0, line);
        for &p in pts {
            m.mat(rl::LINES, Mat::Solid)
                .px_line(p, nearest(p), 1.0, line);
        }
        // A light ridge cap beside the dark line.
        if half_ridge > 0.5 {
            m.mat(rl::LINES, Mat::Solid)
                .px_line_off(r0, r1, 1.0, 1.0, [255, 240, 220, 110]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn district_meshes_build() {
        let map = riot_testkit::district(512.0, 64.0);
        let decals = DecalAtlas::layout();
        let g = build_ground(&map, true, &decals);
        let r = build_roofs(&map, true, &decals);
        assert!(g.triangles() > 1000 && r.triangles() > 1000);
        let (gf, rf) = (
            build_ground(&map, false, &decals),
            build_roofs(&map, false, &decals),
        );
        assert!(gf.triangles() * 2 < g.triangles() && rf.triangles() * 2 < r.triangles());
        let (v, i, b) = g.finish();
        assert!(i.iter().all(|&k| (k as usize) < v.len()));
        assert!(b.iter().any(|b| b.pass == Pass::Shadow));
    }
}
