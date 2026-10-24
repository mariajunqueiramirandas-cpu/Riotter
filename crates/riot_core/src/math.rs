//! Small deterministic math: a 2D vector, quantized headings and `u8`
//! quantization (DESIGN §6.2). Only `+ - * /` and `sqrt` are used, which are
//! correctly rounded IEEE operations (DESIGN §12 rule 7).

use core::f32::consts::{FRAC_PI_2, PI};
use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }
    #[inline]
    pub fn dot(self, o: Vec2) -> f32 {
        self.x * o.x + self.y * o.y
    }
    /// z of the 3D cross product.
    #[inline]
    pub fn cross(self, o: Vec2) -> f32 {
        self.x * o.y - self.y * o.x
    }
    #[inline]
    pub fn len2(self) -> f32 {
        self.dot(self)
    }
    #[inline]
    pub fn len(self) -> f32 {
        self.len2().sqrt()
    }
    #[inline]
    pub fn normalize_or_zero(self) -> Vec2 {
        let l = self.len();
        if l > 1e-9 {
            self * (1.0 / l)
        } else {
            Vec2::ZERO
        }
    }
    /// Rotated +90° (towards +y from +x).
    #[inline]
    pub fn perp(self) -> Vec2 {
        Vec2::new(-self.y, self.x)
    }
    #[inline]
    pub fn clamp_len(self, max: f32) -> Vec2 {
        let l2 = self.len2();
        if l2 > max * max {
            self * (max / l2.sqrt())
        } else {
            self
        }
    }
    #[inline]
    pub fn lerp(self, o: Vec2, t: f32) -> Vec2 {
        self + (o - self) * t
    }
    #[inline]
    pub fn from_heading(h: u8) -> Vec2 {
        let [c, s] = HEADING_DIRS[h as usize];
        Vec2::new(c, s)
    }
    #[inline]
    pub fn heading(self) -> u8 {
        heading_of(self.x, self.y)
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    #[inline]
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}
impl AddAssign for Vec2 {
    #[inline]
    fn add_assign(&mut self, o: Vec2) {
        self.x += o.x;
        self.y += o.y;
    }
}
impl Sub for Vec2 {
    type Output = Vec2;
    #[inline]
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}
impl SubAssign for Vec2 {
    #[inline]
    fn sub_assign(&mut self, o: Vec2) {
        self.x -= o.x;
        self.y -= o.y;
    }
}
impl Mul<f32> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn mul(self, k: f32) -> Vec2 {
        Vec2::new(self.x * k, self.y * k)
    }
}
impl Neg for Vec2 {
    type Output = Vec2;
    #[inline]
    fn neg(self) -> Vec2 {
        Vec2::new(-self.x, -self.y)
    }
}

// ---------------------------------------------------------------------------
// Headings: 256 steps of about 1.4°. 0 points along +x, 64 along +y.

const fn taylor_sin(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    let mut n = 1;
    while n < 14 {
        term = -term * x2 / ((2 * n * (2 * n + 1)) as f64);
        sum += term;
        n += 1;
    }
    sum
}

const fn build_dirs() -> [[f32; 2]; 256] {
    let mut t = [[0.0f32; 2]; 256];
    let mut k = 0;
    while k < 64 {
        let a = k as f64 * (core::f64::consts::PI / 128.0);
        let s = taylor_sin(a);
        let c = taylor_sin(core::f64::consts::FRAC_PI_2 - a);
        t[k] = [c as f32, s as f32];
        t[k + 64] = [-s as f32, c as f32];
        t[k + 128] = [-c as f32, -s as f32];
        t[k + 192] = [s as f32, -c as f32];
        k += 1;
    }
    t
}

/// `(cos, sin)` for each of the 256 headings, evaluated at compile time, so
/// there is no runtime trigonometry (DESIGN §9).
pub const HEADING_DIRS: [[f32; 2]; 256] = build_dirs();

/// Deterministic `atan2(y, x)` in radians, in (-π, π]. Max error about 1e-5
/// rad, far below the 1.4° heading step.
#[inline]
pub fn atan2(y: f32, x: f32) -> f32 {
    let ax = x.abs();
    let ay = y.abs();
    let (mn, mx) = if ax < ay { (ax, ay) } else { (ay, ax) };
    if mx == 0.0 {
        return 0.0;
    }
    let t = mn / mx;
    let t2 = t * t;
    let mut a = t
        * (0.999_866
            + t2 * (-0.330_299_5 + t2 * (0.180_141 + t2 * (-0.085_133 + t2 * 0.020_835_1))));
    if ay > ax {
        a = FRAC_PI_2 - a;
    }
    if x < 0.0 {
        a = PI - a;
    }
    if y < 0.0 { -a } else { a }
}

/// Quantized heading of a direction. `(0, 0)` gives 0.
#[inline]
pub fn heading_of(dx: f32, dy: f32) -> u8 {
    let steps = atan2(dy, dx) * (128.0 / PI);
    ((steps + 0.5).floor() as i32 & 255) as u8
}

/// Signed difference `to - from` in heading steps, in -128..=127.
#[inline]
pub fn heading_delta(from: u8, to: u8) -> i32 {
    to.wrapping_sub(from) as i8 as i32
}

/// Turn `from` towards `to` by at most `max_step` steps.
#[inline]
pub fn turn_toward(from: u8, to: u8, max_step: u8) -> u8 {
    let d = heading_delta(from, to).clamp(-(max_step as i32), max_step as i32);
    (from as i32 + d) as u8
}

// ---------------------------------------------------------------------------
// u8 quantization (DESIGN §6.2): widen, compute in f32, narrow with
// saturating rounding.

/// `0..=255` → `0.0..=1.0`.
#[inline]
pub fn widen_u8(x: u8) -> f32 {
    x as f32 * (1.0 / 255.0)
}

/// `0.0..=1.0` → `0..=255`, saturating; NaN gives 0.
#[inline]
pub fn narrow_u8(x: f32) -> u8 {
    (x * 255.0 + 0.5) as u8
}

/// Like [`narrow_u8`] but rounding up with the probability of the remainder
/// (`bits` supplies 16 random bits), so a value nudged by much less than one
/// step per tick still moves at the right rate on average.
#[inline]
pub fn dither_u8(x: f32, bits: u64) -> u8 {
    (x * 255.0 + (bits & 0xffff) as f32 * (1.0 / 65536.0)) as u8
}

/// Closest point on segment `a..b` to `p`.
#[inline]
pub fn closest_on_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let l2 = ab.len2();
    if l2 <= 0.0 {
        return a;
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    a + ab * t
}

/// Do segments `p0..p1` and `q0..q1` properly intersect? Returns the
/// parameter along `p` when they do.
#[inline]
pub fn segment_hit(p0: Vec2, p1: Vec2, q0: Vec2, q1: Vec2) -> Option<f32> {
    let r = p1 - p0;
    let s = q1 - q0;
    let den = r.cross(s);
    if den == 0.0 {
        return None;
    }
    let qp = q0 - p0;
    let t = qp.cross(s) / den;
    let u = qp.cross(r) / den;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some(t)
    } else {
        None
    }
}

/// Smallest `t` in [0, 1] at which a point moving `p0 → p1` comes within
/// `r` of `c`, if it does.
#[inline]
pub fn sweep_circle(p0: Vec2, p1: Vec2, c: Vec2, r: f32) -> Option<f32> {
    let d = p1 - p0;
    let f = p0 - c;
    let a = d.len2();
    let c2 = f.len2() - r * r;
    if c2 <= 0.0 {
        return Some(0.0);
    }
    if a <= 0.0 {
        return None;
    }
    let b = f.dot(d);
    let disc = b * b - a * c2;
    if disc < 0.0 || b > 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    if (0.0..=1.0).contains(&t) {
        Some(t)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dithering_keeps_small_rates() {
        // 0.0003 per step is a sixth of a byte step: rounding to nearest
        // loses all of it, dithering keeps it on average.
        let (mut near, mut dith) = (100u8, 100u8);
        for t in 0..1000u32 {
            near = narrow_u8(widen_u8(near) + 0.0003);
            dith = dither_u8(widen_u8(dith) + 0.0003, crate::rng::mix(t as u64));
        }
        assert_eq!(near, 100);
        let expected = 100.0 + 1000.0 * 0.0003 * 255.0;
        assert!((dith as f32 - expected).abs() < 8.0, "{dith} vs {expected}");
        assert_eq!(dither_u8(1.0, u64::MAX), 255);
        assert_eq!(dither_u8(-0.1, u64::MAX), 0);
        assert_eq!(dither_u8(0.0, 0), 0);
    }

    #[test]
    fn heading_table_is_accurate() {
        for (k, [c, s]) in HEADING_DIRS.iter().enumerate() {
            let a = k as f64 * std::f64::consts::PI / 128.0;
            assert!((*c as f64 - a.cos()).abs() < 1e-6, "cos {k}");
            assert!((*s as f64 - a.sin()).abs() < 1e-6, "sin {k}");
        }
    }

    #[test]
    fn atan2_matches_std() {
        let mut max_err = 0.0f32;
        for i in 0..2000 {
            let a = i as f32 * 0.0031415 - PI;
            let (x, y) = (a.cos() * 3.0, a.sin() * 3.0);
            max_err = max_err.max(
                (atan2(y, x) - y.atan2(x))
                    .abs()
                    .min(2.0 * PI - (atan2(y, x) - y.atan2(x)).abs()),
            );
        }
        assert!(max_err < 2e-5, "{max_err}");
    }

    #[test]
    fn heading_roundtrip() {
        for h in 0..=255u8 {
            let v = Vec2::from_heading(h) * 2.5;
            assert_eq!(v.heading(), h);
        }
        assert_eq!(heading_of(1.0, 0.0), 0);
        assert_eq!(heading_of(0.0, 1.0), 64);
        assert_eq!(heading_of(-1.0, 0.0), 128);
        assert_eq!(heading_of(0.0, -1.0), 192);
    }

    #[test]
    fn turning_wraps() {
        assert_eq!(turn_toward(250, 4, 3), 253);
        assert_eq!(turn_toward(4, 250, 3), 1);
        assert_eq!(turn_toward(10, 12, 5), 12);
        assert_eq!(heading_delta(0, 128), -128);
    }

    #[test]
    fn quantization_saturates() {
        assert_eq!(narrow_u8(-1.0), 0);
        assert_eq!(narrow_u8(2.0), 255);
        assert_eq!(narrow_u8(f32::NAN), 0);
        for x in 0..=255u8 {
            assert_eq!(narrow_u8(widen_u8(x)), x);
        }
    }

    #[test]
    fn geometry() {
        let hit = segment_hit(
            Vec2::new(0.0, -1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-1.0, 0.0),
            Vec2::new(1.0, 0.0),
        );
        assert_eq!(hit, Some(0.5));
        let t = sweep_circle(Vec2::new(-2.0, 0.0), Vec2::new(2.0, 0.0), Vec2::ZERO, 1.0).unwrap();
        assert!((t - 0.25).abs() < 1e-6);
        assert!(sweep_circle(Vec2::new(-2.0, 2.0), Vec2::new(2.0, 2.0), Vec2::ZERO, 1.0).is_none());
        let c = closest_on_segment(Vec2::new(0.5, 3.0), Vec2::ZERO, Vec2::new(1.0, 0.0));
        assert_eq!(c, Vec2::new(0.5, 0.0));
    }
}
