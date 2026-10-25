//! Textured triangle meshes: the map's ground and roofs, and each frame's
//! props and vehicles.
//!
//! A mesh is built on the CPU in **layers**, drawn in order. Within a layer,
//! triangles are grouped by texture and pass (nothing in one layer overlaps,
//! so their order does not matter), which keeps a whole city to a few dozen
//! batches. Every texture exists once per art level and the batch binds the
//! one matching the frame's view.
//!
//! Vertices carry a world position, texture coordinates (world-mapped for
//! tiling materials, or a sprite's atlas rectangle), a tint and an
//! **extrusion** in buffer pixels: lines built with [`Part::px_line`] are a
//! whole number of pixels wide at every zoom.
//!
//! Passes: `Normal` blends; `Shadow` is depth-tested so overlapping shadows
//! darken once; `Dither` turns alpha into an ordered-dither pattern of
//! opaque pixels (see-through tree canopies).
//!
//! This is the CPU side: `riot_render::mesh` uploads and draws the meshes.

use riot_art::{self as art, Rgb};
use riot_materials::{Mat, TILE_M};
use riot_sheets::Sprite;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    /// Offset in buffer pixels, scaled by metres per pixel in the shader.
    pub ext: [f32; 2],
    pub color: [u8; 4],
}

/// Which texture a batch samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tex {
    Mat(Mat),
    Decals,
    Things,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pass {
    Normal,
    Shadow,
    Dither,
}

/// An 8-bit straight-alpha colour.
pub type Rgba = [u8; 4];

pub fn rgba(c: Rgb, a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

pub const WHITE: Rgba = [255, 255, 255, 255];

/// Triangles of one texture and pass.
#[derive(Default)]
pub struct Part {
    pub verts: Vec<Vertex>,
    pub idx: Vec<u32>,
}

#[inline]
fn wuv(p: [f32; 2]) -> [f32; 2] {
    [p[0] / TILE_M, p[1] / TILE_M]
}

impl Part {
    fn base(&self) -> u32 {
        self.verts.len() as u32
    }

    #[inline]
    fn v(&mut self, pos: [f32; 2], uv: [f32; 2], ext: [f32; 2], color: Rgba) {
        self.verts.push(Vertex {
            pos,
            uv,
            ext,
            color,
        });
    }

    /// A simple polygon (any winding), world-mapped texture.
    pub fn polygon(&mut self, pts: &[[f32; 2]], c: Rgba) {
        self.polygon_uv(pts, c, wuv);
    }

    /// A simple polygon with texture coordinates from `uv(point)`.
    pub fn polygon_uv(&mut self, pts: &[[f32; 2]], c: Rgba, uv: impl Fn([f32; 2]) -> [f32; 2]) {
        let b = self.base();
        for &p in pts {
            self.v(p, uv(p), [0.0, 0.0], c);
        }
        let idx = &mut self.idx;
        triangulate_with(pts, |t| {
            idx.extend_from_slice(&[b + t[0], b + t[1], b + t[2]])
        });
    }

    /// A quadrilateral with explicit texture coordinates.
    pub fn quad_uv(&mut self, q: [[f32; 2]; 4], uv: [[f32; 2]; 4], c: Rgba) {
        let b = self.base();
        for k in 0..4 {
            self.v(q[k], uv[k], [0.0, 0.0], c);
        }
        self.idx
            .extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }

    /// A convex quadrilateral, world-mapped.
    pub fn quad(&mut self, q: [[f32; 2]; 4], c: Rgba) {
        self.quad_uv(q, q.map(wuv), c);
    }

    /// An axis-aligned rectangle, world-mapped.
    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgba) {
        self.quad([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], c);
    }

    /// A strip `w` metres wide from `a` to `b` with the texture's `u`
    /// running along it (kerbs, planks), square ends.
    pub fn strip(&mut self, a: [f32; 2], b: [f32; 2], w: f32, c: Rgba) {
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 1e-6 {
            return;
        }
        let n = [-d[1] / l * w * 0.5, d[0] / l * w * 0.5];
        // u along the strip from a world-anchored start, v across.
        let u0 = (a[0] * d[0] + a[1] * d[1]) / l / TILE_M;
        let u1 = u0 + l / TILE_M;
        let v0 = (a[0] * -d[1] + a[1] * d[0]) / l / TILE_M;
        let v1 = v0 + w / TILE_M;
        self.quad_uv(
            [
                [a[0] + n[0], a[1] + n[1]],
                [b[0] + n[0], b[1] + n[1]],
                [b[0] - n[0], b[1] - n[1]],
                [a[0] - n[0], a[1] - n[1]],
            ],
            [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
            c,
        );
    }

    /// A segment of world width `w`, world-mapped.
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

    /// A segment `px` buffer pixels wide at any zoom, extended by half a
    /// width at each end so joints close.
    pub fn px_line(&mut self, a: [f32; 2], b: [f32; 2], px: f32, c: Rgba) {
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 1e-6 {
            return;
        }
        let (u, h) = ([d[0] / l, d[1] / l], px * 0.5);
        let n = [-u[1] * h, u[0] * h];
        let e = [u[0] * h, u[1] * h];
        let base = self.base();
        let uv = wuv(a);
        self.v(a, uv, [n[0] - e[0], n[1] - e[1]], c);
        self.v(b, uv, [n[0] + e[0], n[1] + e[1]], c);
        self.v(b, uv, [-n[0] + e[0], -n[1] + e[1]], c);
        self.v(a, uv, [-n[0] - e[0], -n[1] - e[1]], c);
        self.idx
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Like [`Part::px_line`], shifted `off` pixels to the left of `a → b`
    /// (negative: to the right), with square ends trimmed to the segment.
    pub fn px_line_off(&mut self, a: [f32; 2], b: [f32; 2], px: f32, off: f32, c: Rgba) {
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 1e-6 {
            return;
        }
        let n = [-d[1] / l, d[0] / l];
        let (o0, o1) = (off - px * 0.5, off + px * 0.5);
        let base = self.base();
        let uv = wuv(a);
        self.v(a, uv, [n[0] * o1, n[1] * o1], c);
        self.v(b, uv, [n[0] * o1, n[1] * o1], c);
        self.v(b, uv, [n[0] * o0, n[1] * o0], c);
        self.v(a, uv, [n[0] * o0, n[1] * o0], c);
        self.idx
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// The outline of a closed polygon, `px` pixels wide.
    pub fn px_outline(&mut self, pts: &[[f32; 2]], px: f32, c: Rgba) {
        for i in 0..pts.len() {
            self.px_line(pts[i], pts[(i + 1) % pts.len()], px, c);
        }
    }

    /// The outline of a closed polygon, `w` metres wide.
    pub fn outline(&mut self, pts: &[[f32; 2]], w: f32, c: Rgba) {
        for i in 0..pts.len() {
            self.line(pts[i], pts[(i + 1) % pts.len()], w, c);
        }
    }

    /// A filled circle, world-mapped.
    pub fn disc(&mut self, c: [f32; 2], r: f32, col: Rgba) {
        let n = ((r * 2.0) as usize).clamp(16, 96);
        self.polygon(&art::circle_points(c, r, n), col);
    }

    /// A ring of width `w`, world-mapped.
    pub fn ring(&mut self, c: [f32; 2], r: f32, w: f32, col: Rgba) {
        let n = ((r * 2.0) as usize).clamp(24, 160);
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

    /// A sprite centred at `c`, its x axis along `(cos, sin)`, drawn over an
    /// object of half size `half` (texel for pixel at the nominal size).
    pub fn sprite(&mut self, s: &Sprite, c: [f32; 2], cs: [f32; 2], half: [f32; 2], col: Rgba) {
        let h = s.quad_half(half);
        let ax = [cs[0] * h[0], cs[1] * h[0]];
        let ay = [-cs[1] * h[1], cs[0] * h[1]];
        let [u0, v0, u1, v1] = s.uv();
        self.quad_uv(
            [
                [c[0] - ax[0] - ay[0], c[1] - ax[1] - ay[1]],
                [c[0] + ax[0] - ay[0], c[1] + ax[1] - ay[1]],
                [c[0] + ax[0] + ay[0], c[1] + ax[1] + ay[1]],
                [c[0] - ax[0] + ay[0], c[1] - ax[1] + ay[1]],
            ],
            [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
            col,
        );
    }

    /// An oriented box (for shadows): centre, unit axis, half extents.
    pub fn obb(&mut self, c: [f32; 2], cs: [f32; 2], half: [f32; 2], col: Rgba) {
        let ax = [cs[0] * half[0], cs[1] * half[0]];
        let ay = [-cs[1] * half[1], cs[0] * half[1]];
        self.quad(
            [
                [c[0] - ax[0] - ay[0], c[1] - ax[1] - ay[1]],
                [c[0] + ax[0] - ay[0], c[1] + ax[1] - ay[1]],
                [c[0] + ax[0] + ay[0], c[1] + ax[1] + ay[1]],
                [c[0] - ax[0] + ay[0], c[1] - ax[1] + ay[1]],
            ],
            col,
        );
    }
}

/// Textures a batch can sample: every material, the decals, the things.
const TEXES: usize = Mat::ALL.len() + 2;
/// `(pass, tex)` keys of a layer.
const KEYS: usize = 3 * TEXES;

/// The slot of a `(pass, tex)` key in a layer: slots run in the keys' sort
/// order (pass, then texture), the order batches are drawn in.
#[inline]
fn slot(pass: Pass, tex: Tex) -> usize {
    let t = match tex {
        Tex::Mat(m) => m as usize,
        Tex::Decals => Mat::ALL.len(),
        Tex::Things => Mat::ALL.len() + 1,
    };
    pass as usize * TEXES + t
}

/// The key of a slot.
fn key(slot: usize) -> (Pass, Tex) {
    let pass = [Pass::Normal, Pass::Shadow, Pass::Dither][slot / TEXES];
    let t = slot % TEXES;
    let tex = match t.checked_sub(Mat::ALL.len()) {
        None => Tex::Mat(Mat::ALL[t]),
        Some(0) => Tex::Decals,
        Some(_) => Tex::Things,
    };
    (pass, tex)
}

/// Layers of parts, drawn in order. A layer holds one slot per `(pass,
/// tex)` key, so finding a part is an index, not a search.
#[derive(Default)]
pub struct MeshBuilder {
    layers: Vec<Vec<Option<Part>>>,
}

impl MeshBuilder {
    /// The part of `layer` for a texture and pass.
    #[inline]
    pub fn at(&mut self, layer: usize, tex: Tex, pass: Pass) -> &mut Part {
        if self.layers.len() <= layer {
            self.layers.resize_with(layer + 1, Vec::new);
        }
        let parts = &mut self.layers[layer];
        if parts.is_empty() {
            parts.resize_with(KEYS, || None);
        }
        parts[slot(pass, tex)].get_or_insert_with(Part::default)
    }

    /// Shortcut for a material in the normal pass.
    #[inline]
    pub fn mat(&mut self, layer: usize, m: Mat) -> &mut Part {
        self.at(layer, Tex::Mat(m), Pass::Normal)
    }

    /// Shadows (plain colour, depth-tested).
    #[inline]
    pub fn shadow(&mut self, layer: usize) -> &mut Part {
        self.at(layer, Tex::Mat(Mat::Solid), Pass::Shadow)
    }

    fn parts(&self) -> impl Iterator<Item = &Part> {
        self.layers.iter().flatten().flatten()
    }

    /// Flatten into one vertex and index list with batches in draw order.
    pub fn finish(self) -> (Vec<Vertex>, Vec<u32>, Vec<BatchDesc>) {
        let mut verts = Vec::with_capacity(self.parts().map(|p| p.verts.len()).sum());
        let mut idx = Vec::with_capacity(self.parts().map(|p| p.idx.len()).sum());
        let mut batches = Vec::new();
        for layer in self.layers {
            for (k, part) in layer.into_iter().enumerate() {
                let Some(part) = part.filter(|p| !p.idx.is_empty()) else {
                    continue;
                };
                let (pass, tex) = key(k);
                let base = verts.len() as u32;
                let start = idx.len();
                verts.extend_from_slice(&part.verts);
                idx.extend(part.idx.iter().map(|i| i + base));
                batches.push(BatchDesc {
                    pass,
                    tex,
                    start,
                    count: part.idx.len(),
                });
            }
        }
        (verts, idx, batches)
    }

    /// Triangles in all layers.
    pub fn triangles(&self) -> usize {
        self.parts().map(|p| p.idx.len() / 3).sum()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BatchDesc {
    pub pass: Pass,
    pub tex: Tex,
    pub start: usize,
    pub count: usize,
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

pub fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Ear-clipping triangulation of a simple polygon. Indices into `pts`.
pub fn triangulate(pts: &[[f32; 2]], out: &mut Vec<[u32; 3]>) {
    triangulate_with(pts, |t| out.push(t));
}

/// [`triangulate`], handing each triangle to `emit` in turn. The list of
/// remaining corners lives on the stack for polygons of up to 64 points
/// (nearly all of them), so building a city's meshes allocates nothing per
/// polygon.
fn triangulate_with(pts: &[[f32; 2]], mut emit: impl FnMut([u32; 3])) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let mut stack = [0u32; 64];
    let mut heap = Vec::new();
    let idx: &mut [u32] = if n <= stack.len() {
        &mut stack[..n]
    } else {
        heap.resize(n, 0);
        &mut heap
    };
    for (k, i) in idx.iter_mut().enumerate() {
        *i = k as u32;
    }
    if signed_area2(pts) < 0.0 {
        idx.reverse();
    }
    // The corners left are `idx[..m]`.
    let mut m = n;
    let mut guard = 0;
    while m > 3 && guard < n * n {
        guard += 1;
        // The first ear: a convex corner with no other corner inside.
        let ear = (0..m).find_map(|i| {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (pts[ia as usize], pts[ib as usize], pts[ic as usize]);
            if cross(a, b, c) <= 0.0 {
                return None; // reflex or degenerate
            }
            let inside = idx[..m].iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = pts[j as usize];
                cross(a, b, p) > 0.0 && cross(b, c, p) > 0.0 && cross(c, a, p) > 0.0
            });
            (!inside).then_some((i, [ia, ib, ic]))
        });
        let Some((i, tri)) = ear else {
            break;
        };
        emit(tri);
        idx.copy_within(i + 1..m, i);
        m -= 1;
    }
    // Whatever is left (a triangle, or a degenerate rest) becomes a fan.
    for k in 1..m.saturating_sub(1) {
        emit([idx[0], idx[k], idx[k + 1]]);
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

pub fn centroid(pts: &[[f32; 2]]) -> [f32; 2] {
    let n = pts.len() as f32;
    let s = pts
        .iter()
        .fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
    [s[0] / n, s[1] / n]
}

pub fn is_convex(pts: &[[f32; 2]]) -> bool {
    let n = pts.len();
    let s = signed_area2(pts).signum();
    (0..n).all(|i| cross(pts[i], pts[(i + 1) % n], pts[(i + 2) % n]) * s >= 0.0)
}

// ---------------------------------------------------------------------------
// Culling runs

/// Triangles per culling run.
const RUN_TRIANGLES: usize = 2048;

/// A consecutive range of indices and the area it covers. Runs of a batch
/// are drawn in order, so culling them never changes what covers what.
#[derive(Clone, Copy, Debug)]
pub struct Run {
    pub start: i32,
    pub count: i32,
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// Split `idx[start..start + count]` into runs with bounding boxes.
pub fn runs(verts: &[Vertex], idx: &[u32], start: usize, count: usize) -> Vec<Run> {
    let mut out = Vec::new();
    for (k, tris) in idx[start..start + count]
        .chunks(RUN_TRIANGLES * 3)
        .enumerate()
    {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for &i in tris {
            let p = verts[i as usize].pos;
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        out.push(Run {
            start: (start + k * RUN_TRIANGLES * 3) as i32,
            count: tris.len() as i32,
            // Pixel-wide lines reach a little past their vertices.
            min: [min[0] - 2.0, min[1] - 2.0],
            max: [max[0] + 2.0, max[1] + 2.0],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_follow_the_key_order() {
        let mut keys = Vec::new();
        for pass in [Pass::Normal, Pass::Shadow, Pass::Dither] {
            for tex in Mat::ALL
                .map(Tex::Mat)
                .into_iter()
                .chain([Tex::Decals, Tex::Things])
            {
                keys.push((pass, tex));
            }
        }
        assert_eq!(keys.len(), KEYS);
        for &a in &keys {
            assert_eq!(key(slot(a.0, a.1)), a);
            for &b in &keys {
                assert_eq!(a.cmp(&b), slot(a.0, a.1).cmp(&slot(b.0, b.1)));
            }
        }
    }

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
    fn layers_flatten_in_order_and_runs_cover_every_triangle() {
        let mut m = MeshBuilder::default();
        m.mat(1, Mat::Grass).rect(0.0, 0.0, 1.0, 1.0, WHITE);
        m.mat(0, Mat::Asphalt).rect(0.0, 0.0, 2.0, 2.0, WHITE);
        m.shadow(1).rect(0.0, 0.0, 1.0, 1.0, [0, 0, 0, 80]);
        for k in 0..3000 {
            m.mat(0, Mat::Asphalt)
                .rect(k as f32, 0.0, k as f32 + 1.0, 1.0, WHITE);
        }
        let (v, i, b) = m.finish();
        assert_eq!(b[0].tex, Tex::Mat(Mat::Asphalt));
        assert_eq!(b[1].tex, Tex::Mat(Mat::Grass));
        assert_eq!(b[2].pass, Pass::Shadow);
        let r = runs(&v, &i, b[0].start, b[0].count);
        assert_eq!(
            r.iter().map(|r| r.count as usize).sum::<usize>(),
            b[0].count
        );
        assert!(r.windows(2).all(|w| w[0].start + w[0].count == w[1].start));
    }
}
