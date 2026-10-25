//! Pixel-art building blocks shared by every procedural sheet: art levels,
//! images, hard-edged shapes, colour ramps, tileable noise and dithering.
//!
//! Everything the renderer draws is pixel art generated at start-up, once
//! per **art level**. Level 0 is drawn at [`DENSITY`] pixels per metre and
//! each further level at half the one before. The world is rendered into a
//! buffer at one of these densities (see `riot_render::pixel`), so texels and
//! buffer pixels line up one to one: every level is drawn as its own clean
//! pixel art, never a blurred downscale of a bigger one. Sheets keep the same
//! layout at every level, so normalized texture coordinates never change.
//!
//! The sheets themselves live in `riot_materials`, `riot_sheets` and
//! `riot_sprites`; [`png`] writes them out for the `#[ignore]`d dump tests.

pub mod png;

use std::f32::consts::TAU;

/// Art levels: 48, 24, 12 and 6 pixels per metre.
pub const LEVELS: usize = 4;
/// Pixels per metre at level 0.
pub const DENSITY: f32 = 48.0;

/// Pixels per metre at `level`.
pub fn density(level: usize) -> f32 {
    DENSITY / (1u32 << level) as f32
}

/// The scale of `level` against level 0 (1, 2, 4, 8).
pub fn step(level: usize) -> usize {
    1 << level
}

pub type Rgb = [u8; 3];

/// A straight-alpha RGBA8 image.
#[derive(Clone, Debug)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

impl Img {
    pub fn new(w: usize, h: usize) -> Img {
        Img {
            w,
            h,
            px: vec![[0; 4]; w * h],
        }
    }

    pub fn filled(w: usize, h: usize, c: Rgb) -> Img {
        Img {
            w,
            h,
            px: vec![[c[0], c[1], c[2], 255]; w * h],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 4] {
        self.px[y * self.w + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: Rgb) {
        self.px[y * self.w + x] = [c[0], c[1], c[2], 255];
    }

    #[inline]
    pub fn set_rgba(&mut self, x: usize, y: usize, c: [u8; 4]) {
        self.px[y * self.w + x] = c;
    }

    /// Set a pixel, wrapping around the edges (tileable textures).
    #[inline]
    pub fn set_wrap(&mut self, x: i32, y: i32, c: Rgb) {
        let x = x.rem_euclid(self.w as i32) as usize;
        let y = y.rem_euclid(self.h as i32) as usize;
        self.set(x, y, c);
    }

    /// Bytes for a texture upload.
    pub fn bytes(&self) -> Vec<u8> {
        self.px.iter().flatten().copied().collect()
    }

    /// Give every opaque region inside `rect` a one-pixel outline: each
    /// transparent pixel that touches an opaque one (4-neighbours) becomes
    /// `c`.
    pub fn outline(&mut self, rect: [usize; 4], c: Rgb) {
        let [x0, y0, w, h] = rect;
        let mut add = Vec::new();
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                if self.get(x, y)[3] > 0 {
                    continue;
                }
                let solid = |xx: usize, yy: usize| self.get(xx, yy)[3] > 127;
                let touch = (x > x0 && solid(x - 1, y))
                    || (x + 1 < x0 + w && solid(x + 1, y))
                    || (y > y0 && solid(x, y - 1))
                    || (y + 1 < y0 + h && solid(x, y + 1));
                if touch {
                    add.push((x, y));
                }
            }
        }
        for (x, y) in add {
            self.set(x, y, c);
        }
    }
}

// ---------------------------------------------------------------------------
// Colour

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t + 0.5) as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

pub fn scale(c: Rgb, k: f32) -> Rgb {
    let f = |x: u8| (x as f32 * k + 0.5).clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

/// A pixel-art ramp step: `t < 0` darkens towards a cool shadow, `t > 0`
/// lightens towards a warm highlight (hue-shifted shading).
pub fn tone(c: Rgb, t: f32) -> Rgb {
    if t < 0.0 {
        let d = scale(c, 1.0 + t * 0.75);
        mix(d, [d[0] / 2, d[1] / 2, d[2] / 2 + 24], -t * 0.35)
    } else {
        let l = mix(c, [255, 250, 225], t * 0.55);
        scale(l, 1.0 + t * 0.12)
    }
}

pub fn hex(v: u32) -> Rgb {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

// ---------------------------------------------------------------------------
// Noise and dithering

#[inline]
pub fn hash(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ seed.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5BD1_E995);
    h ^ (h >> 15)
}

/// A value in `[0, 1)` from integer coordinates.
#[inline]
pub fn rnd(x: i32, y: i32, seed: u32) -> f32 {
    (hash(x, y, seed) >> 8) as f32 / 16_777_216.0
}

/// Tileable value noise: `period` lattice cells across one tile; `u, v` in
/// tile units (0..1 covers the tile).
pub fn vnoise(u: f32, v: f32, period: i32, seed: u32) -> f32 {
    let (x, y) = (u * period as f32, v * period as f32);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let (i, j) = (x0 as i32, y0 as i32);
    // A lattice index and the next one, wrapped into the tile: the same as
    // `i.rem_euclid(period)` and `(i + 1).rem_euclid(period)` with one
    // division (none for a power of two).
    let wrap = |i: i32| {
        if period <= 0 || i == i32::MAX {
            return (i.rem_euclid(period), (i + 1).rem_euclid(period));
        }
        let r = if period & (period - 1) == 0 {
            i & (period - 1)
        } else {
            i.rem_euclid(period)
        };
        (r, if r + 1 == period { 0 } else { r + 1 })
    };
    let ((i0, i1), (j0, j1)) = (wrap(i), wrap(j));
    let at = |i: i32, j: i32| rnd(i, j, seed);
    let a = at(i0, j0) + (at(i1, j0) - at(i0, j0)) * sx;
    let b = at(i0, j1) + (at(i1, j1) - at(i0, j1)) * sx;
    a + (b - a) * sy
}

/// Tileable fractal noise in `[0, 1)`.
pub fn fbm(u: f32, v: f32, period: i32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut p) = (0.0, 0.5, 0.0, period);
    for o in 0..octaves {
        sum += vnoise(u, v, p, seed.wrapping_add(o * 7919)) * amp;
        norm += amp;
        amp *= 0.5;
        p *= 2;
    }
    sum / norm
}

const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Ordered-dither threshold in `(0, 1)` for a pixel.
#[inline]
pub fn bayer(x: i32, y: i32) -> f32 {
    (BAYER[((y & 3) * 4 + (x & 3)) as usize] as f32 + 0.5) / 16.0
}

/// Pick `a` or `b` by ordered dithering at mix `t` (0 = all `a`).
#[inline]
pub fn dither(x: i32, y: i32, a: Rgb, b: Rgb, t: f32) -> Rgb {
    if t > bayer(x, y) { b } else { a }
}

// ---------------------------------------------------------------------------
// Shapes

pub type P = [f32; 2];

/// A signed-distance shape in metres (negative inside).
#[derive(Clone, Debug)]
pub enum Shape {
    Circle(P, f32),
    /// Centre, radii (along x, along y).
    Ellipse(P, P),
    /// Segment end points and radius.
    Capsule(P, P, f32),
    /// Centre, half extents, rotation, corner radius.
    Box(P, P, f32, f32),
    /// Convex polygon, either winding.
    Poly(Vec<P>),
}

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn len(a: P) -> f32 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}
fn dot(a: P, b: P) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

impl Shape {
    /// Signed distance and a 0..1 "radius" used for bulge shading.
    pub fn eval(&self, p: P) -> (f32, f32) {
        match self {
            Shape::Circle(c, r) => {
                let d = len(sub(p, *c));
                (d - r, (d / r).min(1.0))
            }
            Shape::Ellipse(c, r) => {
                let q = sub(p, *c);
                let k = len([q[0] / r[0], q[1] / r[1]]);
                ((k - 1.0) * r[0].min(r[1]), k.min(1.0))
            }
            Shape::Capsule(a, b, r) => {
                let pa = sub(p, *a);
                let ba = sub(*b, *a);
                let h = (dot(pa, ba) / dot(ba, ba).max(1e-9)).clamp(0.0, 1.0);
                let d = len([pa[0] - ba[0] * h, pa[1] - ba[1] * h]);
                (d - r, (d / r).min(1.0))
            }
            Shape::Box(c, half, ang, round) => {
                let q = sub(p, *c);
                let (s, co) = ang.sin_cos();
                let l = [q[0] * co + q[1] * s, -q[0] * s + q[1] * co];
                let dx = l[0].abs() - (half[0] - round);
                let dy = l[1].abs() - (half[1] - round);
                let outside = len([dx.max(0.0), dy.max(0.0)]);
                let inside = dx.max(dy).min(0.0);
                let n = (l[0].abs() / half[0]).max(l[1].abs() / half[1]);
                (outside + inside - round, n.min(1.0))
            }
            Shape::Poly(pts) => {
                let area: f32 = (0..pts.len())
                    .map(|i| {
                        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                        a[0] * b[1] - b[0] * a[1]
                    })
                    .sum();
                let sign = if area >= 0.0 { 1.0 } else { -1.0 };
                let mut d = f32::NEG_INFINITY;
                for i in 0..pts.len() {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    let e = sub(b, a);
                    let n = [e[1] * sign, -e[0] * sign];
                    let nl = len(n).max(1e-9);
                    d = d.max(dot(sub(p, a), n) / nl);
                }
                (d, 0.5)
            }
        }
    }

    /// Bounding box in metres.
    pub fn bounds(&self) -> (P, P) {
        match self {
            Shape::Circle(c, r) => ([c[0] - r, c[1] - r], [c[0] + r, c[1] + r]),
            Shape::Ellipse(c, r) => ([c[0] - r[0], c[1] - r[1]], [c[0] + r[0], c[1] + r[1]]),
            Shape::Capsule(a, b, r) => (
                [a[0].min(b[0]) - r, a[1].min(b[1]) - r],
                [a[0].max(b[0]) + r, a[1].max(b[1]) + r],
            ),
            Shape::Box(c, half, _, _) => {
                let e = len(*half);
                ([c[0] - e, c[1] - e], [c[0] + e, c[1] + e])
            }
            Shape::Poly(pts) => {
                let mut lo = [f32::INFINITY; 2];
                let mut hi = [f32::NEG_INFINITY; 2];
                for p in pts {
                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                }
                (lo, hi)
            }
        }
    }

    /// Outward surface normal at `p` (numerical gradient of the distance).
    pub fn normal(&self, p: P) -> P {
        self.prepare().normal(p)
    }

    /// The shape ready to be evaluated at many points: what [`Shape::eval`]
    /// derives from the shape alone (a box's rotation, a polygon's winding
    /// and edge normals) is worked out once. Its `eval` and `normal` give
    /// exactly the shape's.
    pub fn prepare(&self) -> Prepared<'_> {
        let kind = match self {
            Shape::Box(_, _, ang, _) => {
                let (s, co) = ang.sin_cos();
                Prep::Box(s, co)
            }
            Shape::Poly(pts) => {
                let area: f32 = (0..pts.len())
                    .map(|i| {
                        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                        a[0] * b[1] - b[0] * a[1]
                    })
                    .sum();
                let sign = if area >= 0.0 { 1.0 } else { -1.0 };
                Prep::Poly(
                    (0..pts.len())
                        .map(|i| {
                            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                            let e = sub(b, a);
                            let n = [e[1] * sign, -e[0] * sign];
                            (a, n, len(n).max(1e-9))
                        })
                        .collect(),
                )
            }
            _ => Prep::Plain,
        };
        Prepared { shape: self, kind }
    }
}

/// A [`Shape`] with its per-shape terms worked out ([`Shape::prepare`]).
pub struct Prepared<'a> {
    shape: &'a Shape,
    kind: Prep,
}

enum Prep {
    Plain,
    /// The rotation's sine and cosine.
    Box(f32, f32),
    /// Per edge: start point, outward normal and its length.
    Poly(Vec<(P, P, f32)>),
}

impl Prepared<'_> {
    /// [`Shape::eval`], with the same operations on the same values.
    #[inline]
    pub fn eval(&self, p: P) -> (f32, f32) {
        match (&self.kind, self.shape) {
            (&Prep::Box(s, co), &Shape::Box(c, half, _, round)) => {
                let q = sub(p, c);
                let l = [q[0] * co + q[1] * s, -q[0] * s + q[1] * co];
                let dx = l[0].abs() - (half[0] - round);
                let dy = l[1].abs() - (half[1] - round);
                let outside = len([dx.max(0.0), dy.max(0.0)]);
                let inside = dx.max(dy).min(0.0);
                let n = (l[0].abs() / half[0]).max(l[1].abs() / half[1]);
                (outside + inside - round, n.min(1.0))
            }
            (Prep::Poly(edges), _) => {
                let mut d = f32::NEG_INFINITY;
                for &(a, n, nl) in edges {
                    d = d.max(dot(sub(p, a), n) / nl);
                }
                (d, 0.5)
            }
            _ => self.shape.eval(p),
        }
    }

    /// [`Shape::normal`]: the numerical gradient of the distance.
    pub fn normal(&self, p: P) -> P {
        let e = 0.01;
        let dx = self.eval([p[0] + e, p[1]]).0 - self.eval([p[0] - e, p[1]]).0;
        let dy = self.eval([p[0], p[1] + e]).0 - self.eval([p[0], p[1] - e]).0;
        let l = (dx * dx + dy * dy).sqrt().max(1e-9);
        [dx / l, dy / l]
    }
}

/// Points of a regular polygon (a circle for meshes).
pub fn circle_points(c: P, r: f32, n: usize) -> Vec<P> {
    (0..n)
        .map(|k| {
            let a = k as f32 / n as f32 * TAU;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A colour sheet: sprites in final colours (props, vehicles, decals)

/// Light comes from the north-west (screen up-left), as for the shadows.
pub const LIGHT: P = [-0.6, -0.8];

/// A rectangle of an atlas in level-0 units of [`UNIT`] pixels, so the same
/// rectangle is exact at every level.
pub const UNIT: usize = 8;

/// Paints sprites into one level of a colour atlas. Coordinates are metres
/// relative to the sprite's centre; `x` right, `y` down.
pub struct Sheet {
    pub img: Img,
    pub level: usize,
    pub ppm: f32,
    /// Pixel origin and size of the sprite being painted.
    pub at: [usize; 4],
}

impl Sheet {
    pub fn new(w_units: usize, h_units: usize, level: usize) -> Sheet {
        let px = UNIT / step(level);
        Sheet {
            img: Img::new(w_units * px, h_units * px),
            level,
            ppm: density(level),
            at: [0, 0, 0, 0],
        }
    }

    /// Start painting the sprite in unit rectangle `r = [x, y, w, h]`.
    pub fn begin(&mut self, r: [usize; 4]) {
        let px = UNIT / step(self.level);
        self.at = [r[0] * px, r[1] * px, r[2] * px, r[3] * px];
    }

    /// Pixel centre of sprite pixel `(x, y)` in metres from the centre.
    #[inline]
    fn metres(&self, x: usize, y: usize) -> P {
        [
            (x as f32 + 0.5 - self.at[2] as f32 * 0.5) / self.ppm,
            (y as f32 + 0.5 - self.at[3] as f32 * 0.5) / self.ppm,
        ]
    }

    /// Fill a shape with a colour chosen per pixel by `f(metres, shape
    /// radius 0..1, outward normal)`. Pixels whose centre is inside are
    /// painted; tiny shapes keep at least their centre pixel.
    pub fn fill_with(&mut self, shape: &Shape, f: impl FnMut(P, f32, P) -> Option<Rgb>) {
        self.fill_px(shape, true, f);
    }

    /// [`Sheet::fill_with`]; without `normals`, `f` gets a zero normal
    /// (for colourings that ignore it: the normal costs four evaluations).
    fn fill_px(
        &mut self,
        shape: &Shape,
        normals: bool,
        mut f: impl FnMut(P, f32, P) -> Option<Rgb>,
    ) {
        let (lo, hi) = shape.bounds();
        let prep = shape.prepare();
        let [ox, oy, w, h] = self.at;
        let to_px = |m: f32, size: usize| m * self.ppm + size as f32 * 0.5;
        let x0 = (to_px(lo[0], w) - 1.0).floor().max(0.0) as usize;
        let y0 = (to_px(lo[1], h) - 1.0).floor().max(0.0) as usize;
        let x1 = ((to_px(hi[0], w) + 1.0).ceil() as usize).min(w);
        let y1 = ((to_px(hi[1], h) + 1.0).ceil() as usize).min(h);
        let mut any = false;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = self.metres(x, y);
                let (d, r) = prep.eval(p);
                if d * self.ppm < 0.05 {
                    any = true;
                    let n = if normals && r > 0.0 {
                        prep.normal(p)
                    } else {
                        [0.0, 0.0]
                    };
                    if let Some(c) = f(p, r, n) {
                        self.img.set(ox + x, oy + y, c);
                    }
                }
            }
        }
        if !any {
            // Smaller than a pixel: keep its centre.
            let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
            let x = (c[0] * self.ppm + w as f32 * 0.5).floor();
            let y = (c[1] * self.ppm + h as f32 * 0.5).floor();
            if x >= 0.0
                && y >= 0.0
                && (x as usize) < w
                && (y as usize) < h
                && let Some(col) = f(c, 0.0, [0.0, 0.0])
            {
                self.img.set(ox + x as usize, oy + y as usize, col);
            }
        }
    }

    /// Fill with a flat colour.
    pub fn fill(&mut self, shape: &Shape, c: Rgb) {
        self.fill_px(shape, false, |_, _, _| Some(c));
    }

    /// Fill lit from the north-west: three tones from the surface normal.
    /// `round` (0..1) is how much of the shape bulges (0 = flat top).
    pub fn shaded(&mut self, shape: &Shape, c: Rgb, round: f32) {
        let dark = tone(c, -0.35);
        let light = tone(c, 0.35);
        self.fill_with(shape, |_, r, n| {
            let lit = (n[0] * LIGHT[0] + n[1] * LIGHT[1]) * (r * r) * round;
            Some(if lit > 0.33 {
                light
            } else if lit < -0.33 {
                dark
            } else {
                c
            })
        });
    }

    /// Fill with a dithered blend of `a` and `b` by `t(metres)`.
    pub fn dithered(&mut self, shape: &Shape, a: Rgb, b: Rgb, t: impl Fn(P) -> f32) {
        let [ox, oy, _, _] = self.at;
        let ppm = self.ppm;
        let w = self.at[2] as f32;
        let h = self.at[3] as f32;
        self.fill_px(shape, false, |p, _, _| {
            let x = (p[0] * ppm + w * 0.5) as i32 + ox as i32;
            let y = (p[1] * ppm + h * 0.5) as i32 + oy as i32;
            Some(dither(x, y, a, b, t(p)))
        });
    }

    /// A line of `px` pixels (at least one) between two points.
    pub fn line(&mut self, a: P, b: P, px: f32, c: Rgb) {
        let r = (px * 0.5 / self.ppm).max(0.5 / self.ppm);
        self.fill(&Shape::Capsule(a, b, r * 0.999), c);
    }

    /// Outline everything painted in the current sprite.
    pub fn outline(&mut self, c: Rgb) {
        self.img.outline(self.at, c);
    }

    /// Metres per pixel at this level.
    pub fn px(&self) -> f32 {
        1.0 / self.ppm
    }

    /// The finished image.
    pub fn finish(self) -> Img {
        self.img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_tiles() {
        for k in 0..20 {
            let v = k as f32 / 20.0;
            let a = fbm(0.0, v, 4, 3, 7);
            let b = fbm(1.0, v, 4, 3, 7);
            assert!((a - b).abs() < 1e-5);
            assert!((0.0..1.0).contains(&a));
        }
    }

    #[test]
    fn noise_wraps_like_rem_euclid() {
        // The noise as first written: four wrapping divisions per sample.
        let reference = |u: f32, v: f32, period: i32, seed: u32| {
            let (x, y) = (u * period as f32, v * period as f32);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let s = |t: f32| t * t * (3.0 - 2.0 * t);
            let (sx, sy) = (s(fx), s(fy));
            let at = |i: i32, j: i32| rnd(i.rem_euclid(period), j.rem_euclid(period), seed);
            let (i, j) = (x0 as i32, y0 as i32);
            let a = at(i, j) + (at(i + 1, j) - at(i, j)) * sx;
            let b = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * sx;
            a + (b - a) * sy
        };
        for period in 1..=24 {
            for k in -300..300 {
                let (u, v) = (k as f32 * 0.0137, k as f32 * -0.0291 + 0.5);
                let (a, b) = (vnoise(u, v, period, 9), reference(u, v, period, 9));
                assert_eq!(a.to_bits(), b.to_bits(), "period {period}, u {u}, v {v}");
            }
        }
    }

    #[test]
    fn levels_halve() {
        assert_eq!(density(0), 48.0);
        assert_eq!(density(3), 6.0);
        assert_eq!(UNIT % step(LEVELS - 1), 0);
    }

    #[test]
    fn sheet_paints_tiny_shapes() {
        let mut s = Sheet::new(2, 2, 3);
        s.begin([0, 0, 2, 2]);
        s.fill(&Shape::Circle([0.0, 0.0], 0.01), [255, 0, 0]);
        assert!(s.img.px.iter().any(|p| p[3] == 255));
    }
}
