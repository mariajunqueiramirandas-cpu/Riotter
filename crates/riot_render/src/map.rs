//! Static map geometry (DESIGN §18.2 layers 1 and 7), built once from the
//! [`MapDef`]: the ground (pavement, roads, plazas, water, building shadows)
//! and the roofs (buildings seen from above, walls, fences, overhead
//! canopies). Each is one vertex buffer and one draw call.

use macroquad::miniquad::*;
use riot_sim::mapdef::{Ground, MapDef, Polygon, SolidKind};

use crate::camera::Camera;

/// One vertex of the flat-colour meshes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FlatVertex {
    pub pos: [f32; 2],
    pub color: [u8; 4],
}

/// CPU-side triangle mesh.
#[derive(Default)]
pub struct MeshBuilder {
    pub verts: Vec<FlatVertex>,
    pub idx: Vec<u32>,
}

/// An 8-bit straight-alpha colour.
pub type Rgba = [u8; 4];

impl MeshBuilder {
    fn base(&self) -> u32 {
        self.verts.len() as u32
    }

    fn push(&mut self, p: [f32; 2], c: Rgba) {
        self.verts.push(FlatVertex { pos: p, color: c });
    }

    /// A simple polygon (any winding), triangulated by ear clipping.
    pub fn polygon(&mut self, pts: &[[f32; 2]], c: Rgba) {
        let mut tris = Vec::new();
        triangulate(pts, &mut tris);
        let b = self.base();
        for &p in pts {
            self.push(p, c);
        }
        for t in tris {
            self.idx.extend_from_slice(&[b + t[0], b + t[1], b + t[2]]);
        }
    }

    /// A convex quadrilateral, corners in order.
    pub fn quad(&mut self, q: [[f32; 2]; 4], c: Rgba) {
        let b = self.base();
        for p in q {
            self.push(p, c);
        }
        self.idx
            .extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }

    /// One triangle.
    pub fn triangle(&mut self, t: [[f32; 2]; 3], c: Rgba) {
        let b = self.base();
        for p in t {
            self.push(p, c);
        }
        self.idx.extend_from_slice(&[b, b + 1, b + 2]);
    }

    /// An axis-aligned rectangle.
    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgba) {
        self.quad([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], c);
    }

    /// A segment of width `w`, square ends.
    pub fn line(&mut self, a: [f32; 2], b: [f32; 2], w: f32, c: Rgba) {
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 1e-6 {
            return;
        }
        let n = [-d[1] / l * w * 0.5, d[0] / l * w * 0.5];
        self.quad(
            [
                [a[0] + n[0], a[1] + n[1]],
                [b[0] + n[0], b[1] + n[1]],
                [b[0] - n[0], b[1] - n[1]],
                [a[0] - n[0], a[1] - n[1]],
            ],
            c,
        );
    }

    /// A filled circle.
    pub fn disc(&mut self, c: [f32; 2], r: f32, col: Rgba) {
        let n = ((r * 1.2) as usize).clamp(32, 160);
        let b = self.base();
        self.push(c, col);
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            self.push([c[0] + r * a.cos(), c[1] + r * a.sin()], col);
        }
        for k in 0..n as u32 {
            self.idx
                .extend_from_slice(&[b, b + 1 + k, b + 1 + (k + 1) % n as u32]);
        }
    }

    /// A circle outline of width `w`.
    pub fn ring(&mut self, c: [f32; 2], r: f32, w: f32, col: Rgba) {
        let n = ((r * 1.2) as usize).clamp(32, 160);
        for k in 0..n {
            let a0 = k as f32 / n as f32 * std::f32::consts::TAU;
            let a1 = (k + 1) as f32 / n as f32 * std::f32::consts::TAU;
            let (r0, r1) = (r - w * 0.5, r + w * 0.5);
            self.quad(
                [
                    [c[0] + r0 * a0.cos(), c[1] + r0 * a0.sin()],
                    [c[0] + r1 * a0.cos(), c[1] + r1 * a0.sin()],
                    [c[0] + r1 * a1.cos(), c[1] + r1 * a1.sin()],
                    [c[0] + r0 * a1.cos(), c[1] + r0 * a1.sin()],
                ],
                col,
            );
        }
    }

    /// The outline of a closed polygon.
    pub fn outline(&mut self, pts: &[[f32; 2]], w: f32, c: Rgba) {
        for i in 0..pts.len() {
            self.line(pts[i], pts[(i + 1) % pts.len()], w, c);
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers

/// Twice the signed area (positive = counter-clockwise in a y-up frame).
pub fn signed_area2(pts: &[[f32; 2]]) -> f32 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Ear-clipping triangulation of a simple polygon. Indices into `pts`.
pub fn triangulate(pts: &[[f32; 2]], out: &mut Vec<[u32; 3]>) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let mut idx: Vec<u32> = (0..n as u32).collect();
    if signed_area2(pts) < 0.0 {
        idx.reverse();
    }
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (pts[ia as usize], pts[ib as usize], pts[ic as usize]);
            if cross(a, b, c) <= 0.0 {
                continue; // reflex or degenerate
            }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = pts[j as usize];
                cross(a, b, p) > 0.0 && cross(b, c, p) > 0.0 && cross(c, a, p) > 0.0
            });
            if inside {
                continue;
            }
            out.push([ia, ib, ic]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    // Whatever is left (a triangle, or a degenerate rest) becomes a fan.
    for k in 1..idx.len().saturating_sub(1) {
        out.push([idx[0], idx[k], idx[k + 1]]);
    }
}

/// Polygon moved inwards by `d` metres (miter joins, clamped).
pub fn inset(pts: &[[f32; 2]], d: f32) -> Vec<[f32; 2]> {
    let n = pts.len();
    let ccw = signed_area2(pts) > 0.0;
    let s = if ccw { 1.0 } else { -1.0 };
    let norm = |a: [f32; 2], b: [f32; 2]| {
        let e = [b[0] - a[0], b[1] - a[1]];
        let l = (e[0] * e[0] + e[1] * e[1]).sqrt().max(1e-6);
        // Interior is to the left of each edge for a positive-area polygon.
        [-e[1] / l * s, e[0] / l * s]
    };
    (0..n)
        .map(|i| {
            let (p0, p1, p2) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let (n0, n1) = (norm(p0, p1), norm(p1, p2));
            let m = [n0[0] + n1[0], n0[1] + n1[1]];
            let ml = (m[0] * m[0] + m[1] * m[1]).sqrt().max(1e-6);
            let m = [m[0] / ml, m[1] / ml];
            let k = (d / (m[0] * n0[0] + m[1] * n0[1]).max(0.3)).min(d * 3.0);
            [p1[0] + m[0] * k, p1[1] + m[1] * k]
        })
        .collect()
}

/// Even-odd point-in-polygon test.
pub fn point_in_polygon(p: [f32; 2], pts: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn hash(i: u32) -> u32 {
    let mut x = i.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA77;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^ (x >> 12)
}

fn shade(c: Rgba, k: f32) -> Rgba {
    let f = |v: u8| ((v as f32 * k).clamp(0.0, 255.0)) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

// ---------------------------------------------------------------------------
// Colours

/// Background colour outside the map.
pub const OUTSIDE: Rgba = [26, 28, 32, 255];
const PAVEMENT: Rgba = [186, 182, 172, 255];
const KERB: Rgba = [204, 200, 190, 255];
const ASPHALT: Rgba = [72, 74, 79, 255];
const MARKING: Rgba = [222, 220, 206, 255];
const PLAZA: Rgba = [208, 199, 180, 255];
const GRASS: Rgba = [104, 144, 76, 255];
const WATER: Rgba = [62, 104, 148, 255];
const SHADOW: Rgba = [0, 0, 0, 64];
const WALL: Rgba = [70, 70, 74, 255];
const FENCE: Rgba = [120, 96, 70, 255];
const ROOFS: [Rgba; 6] = [
    [160, 92, 70, 255],
    [124, 124, 130, 255],
    [98, 100, 108, 255],
    [176, 168, 152, 255],
    [122, 90, 74, 255],
    [140, 150, 138, 255],
];

/// Road width drawn from the road graph when the map has no ground layer.
const ROAD_W: f32 = 7.0;

fn ground_colour(g: u8) -> Rgba {
    match g {
        x if x == Ground::Road as u8 => ASPHALT,
        x if x == Ground::Grass as u8 => GRASS,
        x if x == Ground::Plaza as u8 => PLAZA,
        x if x == Ground::Water as u8 => WATER,
        _ => PAVEMENT,
    }
}

/// Below this zoom (px/m) the simplified meshes are drawn: details there
/// would be smaller than a pixel.
pub const DETAIL_ZOOM: f32 = 1.0;

/// The ground mesh: pavement, roads with markings, plazas, parks, water
/// and building shadows. Painted ground cells are drawn as quads merged
/// along rows. Without `detail`: no markings, kerbs, patterns or shadows.
pub fn build_ground(map: &MapDef, detail: bool) -> MeshBuilder {
    let mut m = MeshBuilder::default();
    let (hw, hh) = (map.width * 0.5, map.height * 0.5);
    m.rect(-hw, -hh, hw, hh, PAVEMENT);
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
                    m.rect(
                        -hw + x as f32 * g.cell,
                        -hh + y as f32 * g.cell,
                        -hw + e as f32 * g.cell,
                        -hh + (y + 1) as f32 * g.cell,
                        ground_colour(k),
                    );
                }
                x = e;
            }
        }
    }
    // Named areas as decals: plazas and parks.
    for a in &map.areas {
        match a.tag.as_str() {
            "square" => {
                let r = a.radius * 1.45;
                m.disc(a.pos, r, PLAZA);
                let mut k = 6.0;
                while detail && k < r {
                    m.ring(a.pos, k, 0.25, shade(PLAZA, 0.9));
                    k += 6.0;
                }
            }
            "park" => m.disc(a.pos, a.radius, GRASS),
            "government" => m.disc(a.pos, a.radius, shade(PLAZA, 0.92)),
            _ => {}
        }
    }
    if !painted {
        roads_from_graph(map, &mut m, detail);
    }
    for p in &map.solids {
        if p.kind == SolidKind::Water {
            m.polygon(&p.points, WATER);
            if detail {
                m.outline(&p.points, 0.6, shade(WATER, 1.25));
            }
        }
    }
    // Building shadows (light from the north-west).
    for p in &map.solids {
        if detail && p.kind == SolidKind::Building {
            let s: Vec<[f32; 2]> = p.points.iter().map(|q| [q[0] + 1.6, q[1] + 2.2]).collect();
            m.polygon(&s, SHADOW);
        }
    }
    m
}

fn in_plaza(map: &MapDef, p: [f32; 2]) -> bool {
    map.areas.iter().any(|a| {
        a.tag == "square" && {
            let (dx, dy) = (p[0] - a.pos[0], p[1] - a.pos[1]);
            dx * dx + dy * dy < (a.radius * 1.45).powi(2)
        }
    })
}

/// Asphalt, kerbs, centre lines and zebra crossings along the road graph
/// (only asphalt without `detail`).
fn roads_from_graph(map: &MapDef, m: &mut MeshBuilder, detail: bool) {
    let r = &map.roads;
    let mut degree = vec![0u32; r.nodes.len()];
    let mut edges = Vec::new();
    for e in &r.edges {
        let (a, b) = (r.nodes[e[0] as usize], r.nodes[e[1] as usize]);
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        if in_plaza(map, mid) {
            continue;
        }
        degree[e[0] as usize] += 1;
        degree[e[1] as usize] += 1;
        edges.push((e[0] as usize, e[1] as usize));
    }
    for &(i, j) in edges.iter().filter(|_| detail) {
        m.line(r.nodes[i], r.nodes[j], ROAD_W + 0.7, KERB);
    }
    for (i, &d) in degree.iter().enumerate() {
        if detail && d > 0 {
            let p = r.nodes[i];
            let h = (ROAD_W + 0.7) * 0.5;
            m.rect(p[0] - h, p[1] - h, p[0] + h, p[1] + h, KERB);
        }
    }
    for &(i, j) in &edges {
        m.line(r.nodes[i], r.nodes[j], ROAD_W, ASPHALT);
    }
    for (i, &d) in degree.iter().enumerate() {
        if d > 0 {
            let p = r.nodes[i];
            let h = ROAD_W * 0.5;
            m.rect(p[0] - h, p[1] - h, p[0] + h, p[1] + h, ASPHALT);
        }
    }
    if !detail {
        return;
    }
    for &(i, j) in &edges {
        let (a, b) = (r.nodes[i], r.nodes[j]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 20.0 {
            continue;
        }
        let u = [d[0] / l, d[1] / l];
        let n = [-u[1], u[0]];
        let at = |t: f32, o: f32| [a[0] + u[0] * t + n[0] * o, a[1] + u[1] * t + n[1] * o];
        // Dashed centre line, clear of the junctions.
        let mut t = 9.0;
        while t + 3.0 < l - 9.0 {
            m.line(at(t, 0.0), at(t + 3.0, 0.0), 0.16, MARKING);
            t += 6.0;
        }
        // Zebra crossings at busy junctions.
        for (end, deg) in [(0.0, degree[i]), (l, degree[j])] {
            if deg < 3 {
                continue;
            }
            let t0 = if end == 0.0 { 5.0 } else { l - 7.5 };
            let mut o = -ROAD_W * 0.5 + 0.6;
            while o < ROAD_W * 0.5 - 0.3 {
                m.line(at(t0, o), at(t0 + 2.5, o), 0.5, MARKING);
                o += 1.0;
            }
        }
    }
}

/// The roof mesh: buildings from above (flat roofs with parapets and
/// plant, or hipped roofs with shaded facets), walls, fences and
/// semi-transparent overhead canopies. Without `detail`: flat fills.
pub fn build_roofs(map: &MapDef, detail: bool) -> MeshBuilder {
    let mut m = MeshBuilder::default();
    for (i, p) in map.solids.iter().enumerate() {
        match p.kind {
            SolidKind::Building if !detail => {
                // Muted towards grey: far away, varied roofs read as noise.
                let c = ROOFS[(hash(i as u32) % ROOFS.len() as u32) as usize];
                let mix = |v: u8| ((v as u32 + 128) / 2) as u8;
                m.polygon(&p.points, [mix(c[0]), mix(c[1]), mix(c[2]), 255]);
            }
            SolidKind::Building => building(&mut m, p, hash(i as u32)),
            SolidKind::Wall => {
                m.polygon(&p.points, WALL);
                m.outline(&p.points, 0.25, shade(WALL, 0.6));
            }
            SolidKind::Fence => {
                m.polygon(&p.points, [FENCE[0], FENCE[1], FENCE[2], 150]);
                m.outline(&p.points, 0.2, shade(FENCE, 0.7));
            }
            SolidKind::Water => {}
        }
    }
    for p in &map.overhead {
        m.polygon(&p.points, [58, 98, 52, 150]);
        m.outline(&p.points, 0.4, [40, 70, 36, 170]);
    }
    m
}

fn building(m: &mut MeshBuilder, p: &Polygon, h: u32) {
    let pts = &p.points;
    if pts.len() < 3 {
        return;
    }
    let base = ROOFS[(h % ROOFS.len() as u32) as usize];
    let area = signed_area2(pts).abs() * 0.5;
    let hipped = pts.len() == 4 && (h >> 8) % 100 < 45 && is_convex(pts);
    if hipped {
        hipped_roof(m, pts, base);
    } else {
        m.polygon(pts, shade(base, 1.12));
        let inner = inset(pts, 0.9);
        if (signed_area2(&inner).abs() * 0.5) > area * 0.3 {
            m.polygon(&inner, base);
            // Rooftop plant: a few units near the middle.
            if area > 250.0 {
                let c = centroid(pts);
                for k in 0..(1 + (h >> 12) % 3) {
                    let hh = hash(h ^ (k + 1));
                    let dx = ((hh % 1000) as f32 / 1000.0 - 0.5) * 8.0;
                    let dy = (((hh >> 10) % 1000) as f32 / 1000.0 - 0.5) * 8.0;
                    let (w, d) = (1.2 + (hh >> 20) as f32 % 3.0 * 0.6, 1.0);
                    let q = [
                        [c[0] + dx - w, c[1] + dy - d],
                        [c[0] + dx + w, c[1] + dy - d],
                        [c[0] + dx + w, c[1] + dy + d],
                        [c[0] + dx - w, c[1] + dy + d],
                    ];
                    if q.iter().all(|&v| point_in_polygon(v, &inner)) {
                        let shadow = q.map(|v| [v[0] + 0.4, v[1] + 0.5]);
                        m.quad(shadow, shade(base, 0.7));
                        m.quad(
                            q,
                            if k % 2 == 0 {
                                [196, 198, 200, 255]
                            } else {
                                [86, 108, 128, 255]
                            },
                        );
                    }
                }
            }
        }
    }
    m.outline(pts, 0.35, shade(base, 0.45));
}

fn is_convex(pts: &[[f32; 2]]) -> bool {
    let n = pts.len();
    let s = signed_area2(pts).signum();
    (0..n).all(|i| cross(pts[i], pts[(i + 1) % n], pts[(i + 2) % n]) * s >= 0.0)
}

fn centroid(pts: &[[f32; 2]]) -> [f32; 2] {
    let n = pts.len() as f32;
    let s = pts
        .iter()
        .fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
    [s[0] / n, s[1] / n]
}

/// A hipped roof over a convex quadrilateral: a ridge along the long axis
/// and four facets lit from the north-west.
fn hipped_roof(m: &mut MeshBuilder, pts: &[[f32; 2]], base: Rgba) {
    let c = centroid(pts);
    let e0 = [pts[1][0] - pts[0][0], pts[1][1] - pts[0][1]];
    let e1 = [pts[2][0] - pts[1][0], pts[2][1] - pts[1][1]];
    let (l0, l1) = (
        (e0[0] * e0[0] + e0[1] * e0[1]).sqrt(),
        (e1[0] * e1[0] + e1[1] * e1[1]).sqrt(),
    );
    let (u, long, short) = if l0 >= l1 {
        ([e0[0] / l0, e0[1] / l0], l0, l1)
    } else {
        ([e1[0] / l1, e1[1] / l1], l1, l0)
    };
    let half_ridge = ((long - short) * 0.5).max(0.0);
    let r0 = [c[0] - u[0] * half_ridge, c[1] - u[1] * half_ridge];
    let r1 = [c[0] + u[0] * half_ridge, c[1] + u[1] * half_ridge];
    // Each corner connects to the nearer ridge end.
    let nearest = |p: [f32; 2]| {
        let d = |r: [f32; 2]| (r[0] - p[0]).powi(2) + (r[1] - p[1]).powi(2);
        if d(r0) <= d(r1) { r0 } else { r1 }
    };
    let light = [-0.6f32, -0.8f32];
    for i in 0..4 {
        let (a, b) = (pts[i], pts[(i + 1) % 4]);
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let out = [mid[0] - c[0], mid[1] - c[1]];
        let ol = (out[0] * out[0] + out[1] * out[1]).sqrt().max(1e-6);
        let lit = (out[0] * light[0] + out[1] * light[1]) / ol;
        let col = shade(base, 1.0 + 0.22 * lit);
        let (ra, rb) = (nearest(a), nearest(b));
        if ra == rb {
            m.triangle([a, b, ra], col);
        } else {
            m.quad([a, b, rb, ra], col);
        }
    }
    m.line(r0, r1, 0.3, shade(base, 0.8));
    for &p in pts {
        m.line(p, nearest(p), 0.2, shade(base, 0.85));
    }
}

// ---------------------------------------------------------------------------
// GPU

/// Triangles per culling run.
const RUN_TRIANGLES: usize = 2048;

/// A consecutive range of a mesh's indices and the area it covers. Runs are
/// drawn in order, so culling them never changes what covers what.
#[derive(Clone, Copy, Debug)]
struct Run {
    start: i32,
    count: i32,
    min: [f32; 2],
    max: [f32; 2],
}

/// An uploaded mesh.
pub struct Mesh {
    bindings: Bindings,
    runs: Vec<Run>,
}

/// Split a mesh's triangles into runs with bounding boxes.
fn runs(m: &MeshBuilder) -> Vec<Run> {
    let mut out = Vec::new();
    for (k, tris) in m.idx.chunks(RUN_TRIANGLES * 3).enumerate() {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for &i in tris {
            let p = m.verts[i as usize].pos;
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        out.push(Run {
            start: (k * RUN_TRIANGLES * 3) as i32,
            count: tris.len() as i32,
            min,
            max,
        });
    }
    out
}

/// The flat-colour pipeline and the static map meshes.
pub struct MapRenderer {
    pipeline: Pipeline,
    ground: Option<Mesh>,
    roofs: Option<Mesh>,
    /// Simplified meshes for far zoom.
    ground_far: Option<Mesh>,
    roofs_far: Option<Mesh>,
    /// Map half extents, for the outside-the-map frame.
    pub half: [f32; 2],
}

impl MapRenderer {
    /// Build and upload the ground and roof meshes, detailed and simplified.
    pub fn new(ctx: &mut dyn RenderingBackend, map: &MapDef) -> MapRenderer {
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: FLAT_VERTEX,
                    fragment: FLAT_FRAGMENT,
                },
                ShaderMeta {
                    images: vec![],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![UniformDesc::new("cam", UniformType::Float4)],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("flat shader: {e}"));
        let pipeline = ctx.new_pipeline(
            &[BufferLayout::default()],
            &[
                VertexAttribute::new("pos", VertexFormat::Float2),
                VertexAttribute::new("color", VertexFormat::Byte4),
            ],
            shader,
            PipelineParams {
                color_blend: Some(BlendState::new(
                    Equation::Add,
                    BlendFactor::Value(BlendValue::SourceAlpha),
                    BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
                )),
                ..Default::default()
            },
        );
        MapRenderer {
            pipeline,
            ground: upload(ctx, &build_ground(map, true)),
            roofs: upload(ctx, &build_roofs(map, true)),
            ground_far: upload(ctx, &build_ground(map, false)),
            roofs_far: upload(ctx, &build_roofs(map, false)),
            half: [map.width * 0.5, map.height * 0.5],
        }
    }

    fn draw_mesh(&self, ctx: &mut dyn RenderingBackend, mesh: &Option<Mesh>, cam: &Camera) {
        let Some(mesh) = mesh else { return };
        ctx.apply_pipeline(&self.pipeline);
        ctx.apply_bindings(&mesh.bindings);
        ctx.apply_uniforms(UniformsSource::table(&cam.uniform()));
        let v = cam.view_rect(1.0);
        for r in &mesh.runs {
            let visible = r.max[0] >= v.x
                && r.min[0] <= v.x + v.w
                && r.max[1] >= v.y
                && r.min[1] <= v.y + v.h;
            if visible {
                ctx.draw(r.start, r.count, 1);
            }
        }
    }

    /// Layer 1: the ground. Must run inside a render pass.
    pub fn draw_ground(&self, ctx: &mut dyn RenderingBackend, cam: &Camera) {
        let far = cam.zoom < DETAIL_ZOOM;
        self.draw_mesh(ctx, if far { &self.ground_far } else { &self.ground }, cam);
    }

    /// Layer 7: roofs and canopies. Must run inside a render pass.
    pub fn draw_roofs(&self, ctx: &mut dyn RenderingBackend, cam: &Camera) {
        let far = cam.zoom < DETAIL_ZOOM;
        self.draw_mesh(ctx, if far { &self.roofs_far } else { &self.roofs }, cam);
    }
}

fn upload(ctx: &mut dyn RenderingBackend, m: &MeshBuilder) -> Option<Mesh> {
    if m.idx.is_empty() {
        return None;
    }
    let vb = ctx.new_buffer(
        BufferType::VertexBuffer,
        BufferUsage::Immutable,
        BufferSource::slice(&m.verts),
    );
    let ib = ctx.new_buffer(
        BufferType::IndexBuffer,
        BufferUsage::Immutable,
        BufferSource::slice(&m.idx),
    );
    Some(Mesh {
        bindings: Bindings {
            vertex_buffers: vec![vb],
            index_buffer: ib,
            images: vec![],
        },
        runs: runs(m),
    })
}

const FLAT_VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 pos;
attribute vec4 color;
uniform vec4 cam;
varying lowp vec4 v_color;
void main() {
    gl_Position = vec4((pos - cam.xy) * cam.zw, 0.0, 1.0);
    v_color = color / 255.0;
}
"#;

const FLAT_FRAGMENT: &str = r#"#version 100
varying lowp vec4 v_color;
void main() {
    gl_FragColor = v_color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn area_of(pts: &[[f32; 2]], tris: &[[u32; 3]]) -> f32 {
        tris.iter()
            .map(|t| {
                signed_area2(&[pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]]).abs()
                    * 0.5
            })
            .sum()
    }

    #[test]
    fn triangulates_concave_polygons_of_either_winding() {
        // An L shape.
        let mut l = vec![
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        for _ in 0..2 {
            let mut t = Vec::new();
            triangulate(&l, &mut t);
            assert_eq!(t.len(), 4);
            assert!((area_of(&l, &t) - 6.0).abs() < 1e-4);
            l.reverse();
        }
    }

    #[test]
    fn inset_shrinks_a_rectangle() {
        let r = [[0.0, 0.0], [10.0, 0.0], [10.0, 6.0], [0.0, 6.0]];
        for pts in [r.to_vec(), r.iter().rev().copied().collect()] {
            let i = inset(&pts, 1.0);
            assert!((signed_area2(&i).abs() * 0.5 - 32.0).abs() < 1e-3);
            assert!(i.iter().all(|&p| point_in_polygon(p, &pts)));
        }
    }

    #[test]
    fn runs_cover_every_triangle_in_order() {
        let map = riot_sim::testkit::district(1024.0, 64.0);
        let g = build_ground(&map, true);
        let r = runs(&g);
        assert_eq!(
            r.iter().map(|r| r.count as usize).sum::<usize>(),
            g.idx.len()
        );
        assert!(r.windows(2).all(|w| w[0].start + w[0].count == w[1].start));
        assert!(
            r.iter()
                .all(|r| r.min[0] <= r.max[0] && r.min[1] <= r.max[1])
        );
    }

    #[test]
    fn district_meshes_build() {
        let map = riot_sim::testkit::district(512.0, 64.0);
        let g = build_ground(&map, true);
        let r = build_roofs(&map, true);
        assert!(g.idx.len() > 1000 && r.idx.len() > 1000);
        let (gf, rf) = (build_ground(&map, false), build_roofs(&map, false));
        assert!(gf.idx.len() * 4 < g.idx.len() && rf.idx.len() * 4 < r.idx.len());
        assert!(g.idx.iter().all(|&i| (i as usize) < g.verts.len()));
        assert!(r.idx.iter().all(|&i| (i as usize) < r.verts.len()));
    }
}
