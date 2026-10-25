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

use std::collections::BTreeMap;

use macroquad::miniquad::*;

use crate::art::{self, Img, Rgb};
use crate::materials::{self, Mat, TILE_M};
use crate::pixel::View;
use crate::sheets::Sprite;

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
        let mut tris = Vec::new();
        triangulate(pts, &mut tris);
        let b = self.base();
        for &p in pts {
            self.v(p, uv(p), [0.0, 0.0], c);
        }
        for t in tris {
            self.idx.extend_from_slice(&[b + t[0], b + t[1], b + t[2]]);
        }
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

/// Layers of parts, drawn in order.
#[derive(Default)]
pub struct MeshBuilder {
    layers: Vec<BTreeMap<(Pass, Tex), Part>>,
}

impl MeshBuilder {
    /// The part of `layer` for a texture and pass.
    pub fn at(&mut self, layer: usize, tex: Tex, pass: Pass) -> &mut Part {
        if self.layers.len() <= layer {
            self.layers.resize_with(layer + 1, BTreeMap::new);
        }
        self.layers[layer].entry((pass, tex)).or_default()
    }

    /// Shortcut for a material in the normal pass.
    pub fn mat(&mut self, layer: usize, m: Mat) -> &mut Part {
        self.at(layer, Tex::Mat(m), Pass::Normal)
    }

    /// Shadows (plain colour, depth-tested).
    pub fn shadow(&mut self, layer: usize) -> &mut Part {
        self.at(layer, Tex::Mat(Mat::Solid), Pass::Shadow)
    }

    /// Flatten into one vertex and index list with batches in draw order.
    pub fn finish(self) -> (Vec<Vertex>, Vec<u32>, Vec<BatchDesc>) {
        let mut verts = Vec::new();
        let mut idx = Vec::new();
        let mut batches = Vec::new();
        for layer in self.layers {
            for ((pass, tex), part) in layer {
                if part.idx.is_empty() {
                    continue;
                }
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
        self.layers
            .iter()
            .flat_map(|l| l.values())
            .map(|p| p.idx.len() / 3)
            .sum()
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
// GPU

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

struct Batch {
    pass: Pass,
    tex: Tex,
    runs: Vec<Run>,
}

/// An uploaded static mesh.
pub struct Mesh {
    vb: BufferId,
    ib: BufferId,
    batches: Vec<Batch>,
}

/// A mesh rebuilt every frame (props and vehicles).
pub struct StreamMesh {
    vb: BufferId,
    ib: BufferId,
    vcap: usize,
    icap: usize,
    batches: Vec<BatchDesc>,
}

#[repr(C)]
struct Uniforms {
    cam: [f32; 4],
    /// metres per buffer pixel, uv scroll x, uv scroll y, unused
    params: [f32; 4],
}

/// Pipelines and every texture of the textured meshes.
pub struct MeshRenderer {
    normal: Pipeline,
    shadow: Pipeline,
    dither: Pipeline,
    /// Per material, per level.
    mats: Vec<Vec<TextureId>>,
    decals: Vec<TextureId>,
    things: Vec<TextureId>,
}

/// Upload one level of a texture. Only the coarsest level, which is also
/// drawn minified when zoomed out past it, keeps mipmaps.
fn texture(ctx: &mut dyn RenderingBackend, img: &Img, level: usize, repeat: bool) -> TextureId {
    let last = level == art::LEVELS - 1;
    let t = ctx.new_texture(
        TextureAccess::Static,
        TextureSource::Bytes(&img.bytes()),
        TextureParams {
            width: img.w as u32,
            height: img.h as u32,
            format: TextureFormat::RGBA8,
            wrap: if repeat {
                TextureWrap::Repeat
            } else {
                TextureWrap::Clamp
            },
            min_filter: if last {
                FilterMode::Linear
            } else {
                FilterMode::Nearest
            },
            mag_filter: FilterMode::Nearest,
            mipmap_filter: if last {
                MipmapFilterMode::Linear
            } else {
                MipmapFilterMode::None
            },
            allocate_mipmaps: last,
            ..Default::default()
        },
    );
    if last {
        ctx.texture_generate_mipmaps(t);
    }
    t
}

impl MeshRenderer {
    /// Generate every material and sheet at every level and upload them.
    pub fn new(ctx: &mut dyn RenderingBackend, decals: &[Img], things: &[Img]) -> MeshRenderer {
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("mesh shader: {e}"));
        let dither_shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: DITHER_FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("dither shader: {e}"));
        let layout = [BufferLayout::default()];
        let attrs = [
            VertexAttribute::new("pos", VertexFormat::Float2),
            VertexAttribute::new("uv", VertexFormat::Float2),
            VertexAttribute::new("ext", VertexFormat::Float2),
            VertexAttribute::new("color", VertexFormat::Byte4),
        ];
        let blend = Some(BlendState::new(
            Equation::Add,
            BlendFactor::Value(BlendValue::SourceAlpha),
            BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
        ));
        let normal = ctx.new_pipeline(
            &layout,
            &attrs,
            shader,
            PipelineParams {
                color_blend: blend,
                ..Default::default()
            },
        );
        let shadow = ctx.new_pipeline(
            &layout,
            &attrs,
            shader,
            PipelineParams {
                color_blend: blend,
                depth_test: Comparison::Less,
                depth_write: true,
                ..Default::default()
            },
        );
        let dither = ctx.new_pipeline(
            &layout,
            &attrs,
            dither_shader,
            PipelineParams {
                color_blend: blend,
                ..Default::default()
            },
        );
        // Materials are generated in parallel: a few hundred ms otherwise.
        let imgs: Vec<Vec<Img>> = std::thread::scope(|s| {
            let jobs: Vec<_> = Mat::ALL
                .iter()
                .map(|&m| {
                    s.spawn(move || {
                        (0..art::LEVELS)
                            .map(|l| materials::generate(m, l))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|j| j.join().expect("material generator"))
                .collect()
        });
        let mats = imgs
            .iter()
            .map(|levels| {
                levels
                    .iter()
                    .enumerate()
                    .map(|(l, img)| texture(ctx, img, l, true))
                    .collect()
            })
            .collect();
        let decals = decals
            .iter()
            .enumerate()
            .map(|(l, img)| texture(ctx, img, l, false))
            .collect();
        let things = things
            .iter()
            .enumerate()
            .map(|(l, img)| texture(ctx, img, l, false))
            .collect();
        MeshRenderer {
            normal,
            shadow,
            dither,
            mats,
            decals,
            things,
        }
    }

    fn texture_for(&self, tex: Tex, level: usize) -> TextureId {
        match tex {
            Tex::Mat(m) => self.mats[m.index()][level],
            Tex::Decals => self.decals[level],
            Tex::Things => self.things[level],
        }
    }

    /// Upload a static mesh.
    pub fn upload(&self, ctx: &mut dyn RenderingBackend, m: MeshBuilder) -> Option<Mesh> {
        let (verts, idx, descs) = m.finish();
        if idx.is_empty() {
            return None;
        }
        let vb = ctx.new_buffer(
            BufferType::VertexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&verts),
        );
        let ib = ctx.new_buffer(
            BufferType::IndexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&idx),
        );
        let batches = descs
            .iter()
            .map(|d| Batch {
                pass: d.pass,
                tex: d.tex,
                runs: runs(&verts, &idx, d.start, d.count),
            })
            .collect();
        Some(Mesh { vb, ib, batches })
    }

    fn pipeline(&self, pass: Pass) -> &Pipeline {
        match pass {
            Pass::Normal => &self.normal,
            Pass::Shadow => &self.shadow,
            Pass::Dither => &self.dither,
        }
    }

    fn uniforms(&self, view: &View, tex: Tex, time: f32) -> Uniforms {
        let scroll = if tex == Tex::Mat(Mat::Water) {
            [time * 0.011, time * 0.004]
        } else {
            [0.0, 0.0]
        };
        Uniforms {
            cam: view.cam.uniform(),
            params: [view.px(), scroll[0], scroll[1], 0.0],
        }
    }

    /// Draw a static mesh, culled to the view. Returns triangles drawn.
    /// Must run inside a render pass.
    pub fn draw(
        &self,
        ctx: &mut dyn RenderingBackend,
        mesh: &Mesh,
        view: &View,
        time: f32,
    ) -> usize {
        let v = view.visible;
        let mut tris = 0;
        for b in &mesh.batches {
            let visible: Vec<&Run> = b
                .runs
                .iter()
                .filter(|r| {
                    r.max[0] >= v.x
                        && r.min[0] <= v.x + v.w
                        && r.max[1] >= v.y
                        && r.min[1] <= v.y + v.h
                })
                .collect();
            if visible.is_empty() {
                continue;
            }
            ctx.apply_pipeline(self.pipeline(b.pass));
            ctx.apply_bindings(&Bindings {
                vertex_buffers: vec![mesh.vb],
                index_buffer: mesh.ib,
                images: vec![self.texture_for(b.tex, view.level)],
            });
            ctx.apply_uniforms(UniformsSource::table(&self.uniforms(view, b.tex, time)));
            for r in visible {
                ctx.draw(r.start, r.count, 1);
                tris += r.count as usize / 3;
            }
        }
        tris
    }

    pub fn new_stream(&self, ctx: &mut dyn RenderingBackend) -> StreamMesh {
        let (vcap, icap) = (4096, 6144);
        StreamMesh {
            vb: ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(vcap),
            ),
            ib: ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(icap),
            ),
            vcap,
            icap,
            batches: Vec::new(),
        }
    }

    /// Replace a stream mesh's contents.
    pub fn update_stream(
        &self,
        ctx: &mut dyn RenderingBackend,
        s: &mut StreamMesh,
        m: MeshBuilder,
    ) {
        let (verts, idx, batches) = m.finish();
        if verts.len() > s.vcap || idx.len() > s.icap {
            ctx.delete_buffer(s.vb);
            ctx.delete_buffer(s.ib);
            s.vcap = verts.len().next_power_of_two();
            s.icap = idx.len().next_power_of_two();
            s.vb = ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(s.vcap),
            );
            s.ib = ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(s.icap),
            );
        }
        if !idx.is_empty() {
            ctx.buffer_update(s.vb, BufferSource::slice(&verts));
            ctx.buffer_update(s.ib, BufferSource::slice(&idx));
        }
        s.batches = batches;
    }

    /// Draw a stream mesh. Must run inside a render pass.
    pub fn draw_stream(
        &self,
        ctx: &mut dyn RenderingBackend,
        s: &StreamMesh,
        view: &View,
        time: f32,
    ) {
        for b in &s.batches {
            ctx.apply_pipeline(self.pipeline(b.pass));
            ctx.apply_bindings(&Bindings {
                vertex_buffers: vec![s.vb],
                index_buffer: s.ib,
                images: vec![self.texture_for(b.tex, view.level)],
            });
            ctx.apply_uniforms(UniformsSource::table(&self.uniforms(view, b.tex, time)));
            ctx.draw(b.start as i32, b.count as i32, 1);
        }
    }
}

const VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 pos;
attribute vec2 uv;
attribute vec2 ext;
attribute vec4 color;
uniform vec4 cam;
uniform vec4 params;
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
void main() {
    vec2 p = pos + ext * params.x;
    v_world = p;
    v_uv = uv + params.yz;
    v_color = color / 255.0;
    gl_Position = vec4((p - cam.xy) * cam.zw, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
precision mediump float;
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
uniform sampler2D tex;
void main() {
    vec4 c = texture2D(tex, v_uv) * v_color;
    // Shadows write depth: fully transparent texels must not.
    if (c.a < 0.004) discard;
    gl_FragColor = c;
}
"#;

const DITHER_FRAGMENT: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
uniform sampler2D tex;
uniform vec4 params;
// Ordered dithering on a 4×4 Bayer matrix, anchored to the world's pixels.
float bayer2(vec2 a) {
    a = floor(a);
    return fract(dot(a, vec2(0.5, a.y * 0.75)));
}
float bayer4(vec2 a) {
    return bayer2(0.5 * a) * 0.25 + bayer2(a);
}
void main() {
    vec4 c = texture2D(tex, v_uv) * v_color;
    if (c.a <= bayer4(v_world / params.x) + 0.03) discard;
    gl_FragColor = vec4(c.rgb, 1.0);
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
