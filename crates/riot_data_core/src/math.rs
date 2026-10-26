//! Small geometry helpers. Trigonometry here never calls the platform's libm,
//! so a map converts to bit-identical `MapDef`s everywhere and a generated
//! city is the same on every machine (replays depend on both, DESIGN §12).

use std::f64::consts::PI;

/// `(sin, cos)` of an angle in degrees: exact at multiples of 90°, otherwise
/// accurate to about 1e-14.
pub fn sin_cos_deg(deg: f64) -> (f64, f64) {
    let d = deg.rem_euclid(360.0);
    let q = (d / 90.0).round();
    // r ∈ [-π/4, π/4], exactly 0 at multiples of 90°.
    let r = (d - q * 90.0) * (PI / 180.0);
    let r2 = r * r;
    // Taylor series in Horner form, up to r^15 and r^14.
    let (mut s, mut c) = (1.0, 1.0);
    for k in (1..=7).rev() {
        let k = k as f64;
        s = 1.0 - r2 / (2.0 * k * (2.0 * k + 1.0)) * s;
        c = 1.0 - r2 / ((2.0 * k - 1.0) * 2.0 * k) * c;
    }
    let s = r * s;
    match (q as i64) & 3 {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// `sin` of an angle in radians (see [`sin_cos_deg`]).
pub fn sin(rad: f64) -> f64 {
    sin_cos_deg(rad * (180.0 / PI)).0
}

/// Is `p` inside the closed polygon `poly` (even-odd rule)?
pub fn point_in_polygon(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Squared distance from `p` to the segment `a → b`.
pub fn seg_dist2(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a[0] + dx * t - p[0], a[1] + dy * t - p[1]);
    x * x + y * y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_cos_matches_std() {
        for k in -1440..1440 {
            let deg = k as f64 * 0.37;
            let (s, c) = sin_cos_deg(deg);
            let r = deg.to_radians();
            assert!((s - r.sin()).abs() < 1e-13, "sin {deg}");
            assert!((c - r.cos()).abs() < 1e-13, "cos {deg}");
        }
        assert_eq!(sin_cos_deg(90.0), (1.0, 0.0));
        assert_eq!(sin_cos_deg(-90.0), (-1.0, 0.0));
        assert_eq!(sin_cos_deg(180.0), (0.0, -1.0));
        assert_eq!(sin_cos_deg(0.0), (0.0, 1.0));
    }

    #[test]
    fn polygon_helpers() {
        let sq = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        assert!(point_in_polygon([1.0, 1.0], &sq));
        assert!(!point_in_polygon([3.0, 1.0], &sq));
        assert_eq!(seg_dist2([1.0, 1.0], [0.0, 0.0], [2.0, 0.0]), 1.0);
        assert_eq!(seg_dist2([3.0, 0.0], [0.0, 0.0], [2.0, 0.0]), 1.0);
    }
}
