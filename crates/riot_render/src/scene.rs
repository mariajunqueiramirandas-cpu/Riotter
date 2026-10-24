//! Props, vehicles and projectiles from the snapshot (DESIGN §18.2 layers
//! 1, 4 and 5), drawn as oriented boxes with macroquad shapes in map
//! coordinates. There are at most a few thousand of them.

use macroquad::prelude::*;
use riot_sim::snapshot::Snapshot;

/// Fill an oriented box: centre, unit axis, half extents along and across.
pub fn obb(c: Vec2, ax: Vec2, half: Vec2, col: Color) {
    let ay = vec2(-ax.y, ax.x);
    let (u, v) = (ax * half.x, ay * half.y);
    let p = [c - u - v, c + u - v, c + u + v, c - u + v];
    draw_triangle(p[0], p[1], p[2], col);
    draw_triangle(p[0], p[2], p[3], col);
}

/// An oriented box offset along its own axes.
fn part(c: Vec2, ax: Vec2, along: f32, across: f32, half: Vec2, col: Color) {
    let ay = vec2(-ax.y, ax.x);
    obb(c + ax * along + ay * across, ax, half, col);
}

fn hash(i: u32) -> u32 {
    let mut x = i.wrapping_mul(0x9E37_79B1) ^ 0x7F4A_7C15;
    x ^= x >> 16;
    x.wrapping_mul(0x85EB_CA6B) ^ (x >> 13)
}

const CAR_COLOURS: [Color; 8] = [
    Color::new(0.72, 0.12, 0.12, 1.0),
    Color::new(0.16, 0.3, 0.62, 1.0),
    Color::new(0.9, 0.9, 0.88, 1.0),
    Color::new(0.14, 0.14, 0.16, 1.0),
    Color::new(0.62, 0.64, 0.66, 1.0),
    Color::new(0.2, 0.45, 0.3, 1.0),
    Color::new(0.85, 0.65, 0.15, 1.0),
    Color::new(0.4, 0.42, 0.46, 1.0),
];

const OUTLINE: Color = Color::new(0.08, 0.08, 0.09, 0.9);

/// Layer 1: props on the ground (parked cars, bins, barriers, benches,
/// barricades). Zoomed out they become plain boxes, then disappear.
pub fn draw_props(snap: &Snapshot, view: Rect, zoom: f32) {
    if zoom < 0.4 {
        return;
    }
    for (i, p) in snap.props.iter().enumerate() {
        let c = vec2(p.x, p.y);
        let r = p.half_x.max(p.half_y);
        if !view.overlaps(&Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r)) {
            continue;
        }
        let ax = vec2(p.cos, p.sin);
        let half = vec2(p.half_x, p.half_y);
        let burning = p.flags & 1 != 0;
        let building = p.flags & 2 != 0;
        let destroyed = p.flags & 4 != 0;
        let overturned = p.flags & 8 != 0;
        let health = p.health as f32 / 255.0;
        if zoom < 3.0 {
            let col = match p.kind {
                0 if destroyed => Color::new(0.16, 0.14, 0.13, 1.0),
                0 => CAR_COLOURS[(hash(i as u32) % 8) as usize],
                1 => Color::new(0.18, 0.32, 0.22, 1.0),
                2 => Color::new(0.95, 0.75, 0.1, 1.0),
                3 => Color::new(0.45, 0.3, 0.18, 1.0),
                _ => Color::new(0.5, 0.36, 0.2, 1.0),
            };
            obb(c, ax, half, col);
            continue;
        }
        obb(c, ax, half + Vec2::splat(0.08), OUTLINE);
        match p.kind {
            0 => {
                // Car.
                if destroyed || overturned {
                    let base = if destroyed {
                        Color::new(0.16, 0.14, 0.13, 1.0)
                    } else {
                        Color::new(0.3, 0.3, 0.32, 1.0)
                    };
                    obb(c, ax, half, base);
                    for (a, b) in [(-0.6, -0.8), (-0.6, 0.8), (0.6, -0.8), (0.6, 0.8)] {
                        part(c, ax, a * half.x, b * half.y, vec2(0.35, 0.15), OUTLINE);
                    }
                } else {
                    let mut body = CAR_COLOURS[(hash(i as u32) % 8) as usize];
                    if burning {
                        body = Color::new(body.r * 0.4, body.g * 0.3, body.b * 0.25, 1.0);
                    }
                    let k = 0.4 + 0.6 * health;
                    body = Color::new(body.r * k, body.g * k, body.b * k, 1.0);
                    obb(c, ax, half, body);
                    let glass = Color::new(0.16, 0.2, 0.26, 1.0);
                    part(
                        c,
                        ax,
                        0.32 * half.x,
                        0.0,
                        vec2(0.13 * half.x, 0.82 * half.y),
                        glass,
                    );
                    part(
                        c,
                        ax,
                        -0.55 * half.x,
                        0.0,
                        vec2(0.09 * half.x, 0.78 * half.y),
                        glass,
                    );
                    let roof = Color::new(
                        (body.r * 1.12).min(1.0),
                        (body.g * 1.12).min(1.0),
                        (body.b * 1.12).min(1.0),
                        1.0,
                    );
                    part(c, ax, -0.1, 0.0, vec2(0.28 * half.x, 0.75 * half.y), roof);
                }
            }
            1 => obb(c, ax, half, Color::new(0.18, 0.32, 0.22, 1.0)),
            2 => {
                obb(c, ax, half, Color::new(0.68, 0.7, 0.72, 1.0));
                let n = ((half.x * 2.0) as i32).clamp(1, 12);
                for k in 0..n {
                    let along = -half.x + (k as f32 + 0.5) * 2.0 * half.x / n as f32;
                    part(
                        c,
                        ax,
                        along,
                        0.0,
                        vec2(0.18, half.y),
                        Color::new(0.95, 0.75, 0.1, 1.0),
                    );
                }
            }
            3 => {
                obb(c, ax, half, Color::new(0.45, 0.3, 0.18, 1.0));
                for s in [-0.5, 0.0, 0.5] {
                    part(
                        c,
                        ax,
                        0.0,
                        s * half.y,
                        vec2(half.x, 0.06),
                        Color::new(0.3, 0.2, 0.12, 1.0),
                    );
                }
            }
            _ => {
                // Barricade: stacked planks, see-through while being built.
                let a = if building { 0.55 } else { 1.0 };
                let wood = Color::new(0.5, 0.36, 0.2, a);
                obb(c, ax, half, wood);
                let n = ((half.x * 2.0) as i32).clamp(2, 10);
                for k in 0..n {
                    let along = -half.x + (k as f32 + 0.5) * 2.0 * half.x / n as f32;
                    let tilt = if k % 2 == 0 { 0.25 } else { -0.25 };
                    let d = vec2(ax.x - ax.y * tilt, ax.y + ax.x * tilt).normalize();
                    let ay = vec2(-ax.y, ax.x);
                    obb(
                        c + ax * along + ay * 0.0,
                        d,
                        vec2(0.12, half.y * 1.1),
                        Color::new(0.35, 0.24, 0.13, a),
                    );
                }
            }
        }
    }
}

/// Layer 4: police vans, water cannon trucks and ambulances.
pub fn draw_vehicles(snap: &Snapshot, view: Rect, time: f32) {
    for v in &snap.vehicles {
        let c = vec2(v.x, v.y);
        if !view.contains(c) {
            continue;
        }
        let a = v.heading as f32 * std::f32::consts::TAU / 256.0;
        let ax = vec2(a.cos(), a.sin());
        let half = vec2(v.half_len, v.half_wid);
        let overturned = v.flags & 4 != 0;
        obb(c, ax, half + Vec2::splat(0.1), OUTLINE);
        let glass = Color::new(0.14, 0.18, 0.24, 1.0);
        match v.kind {
            1 => {
                // Water cannon: dark truck, cab in front, turret on top.
                obb(c, ax, half, Color::new(0.26, 0.3, 0.36, 1.0));
                part(
                    c,
                    ax,
                    0.72 * half.x,
                    0.0,
                    vec2(0.26 * half.x, half.y * 0.96),
                    Color::new(0.32, 0.37, 0.44, 1.0),
                );
                part(
                    c,
                    ax,
                    0.9 * half.x,
                    0.0,
                    vec2(0.07 * half.x, half.y * 0.85),
                    glass,
                );
                let t = c + ax * 0.2 * half.x;
                draw_circle(t.x, t.y, half.y * 0.55, Color::new(0.2, 0.22, 0.26, 1.0));
                let tip = t + ax * half.y * 1.4;
                draw_line(
                    t.x,
                    t.y,
                    tip.x,
                    tip.y,
                    0.22,
                    Color::new(0.1, 0.1, 0.12, 1.0),
                );
            }
            2 => {
                obb(c, ax, half, Color::new(0.94, 0.94, 0.92, 1.0));
                part(
                    c,
                    ax,
                    0.82 * half.x,
                    0.0,
                    vec2(0.08 * half.x, half.y * 0.85),
                    glass,
                );
                let red = Color::new(0.85, 0.12, 0.12, 1.0);
                part(c, ax, -0.1 * half.x, 0.0, vec2(0.5, 0.15), red);
                part(c, ax, -0.1 * half.x, 0.0, vec2(0.15, 0.5), red);
            }
            _ => {
                // Police van: white, blue band, light bar.
                obb(c, ax, half, Color::new(0.93, 0.93, 0.95, 1.0));
                part(
                    c,
                    ax,
                    0.0,
                    0.0,
                    vec2(half.x * 0.98, 0.18),
                    Color::new(0.12, 0.25, 0.6, 1.0),
                );
                part(
                    c,
                    ax,
                    0.82 * half.x,
                    0.0,
                    vec2(0.1 * half.x, half.y * 0.85),
                    glass,
                );
                let on = v.flags & 1 != 0 && (time * 4.0) as i32 % 2 == 0;
                let (l, r) = if on {
                    (
                        Color::new(0.2, 0.4, 1.0, 1.0),
                        Color::new(0.9, 0.15, 0.15, 1.0),
                    )
                } else {
                    (
                        Color::new(0.1, 0.2, 0.5, 1.0),
                        Color::new(0.45, 0.08, 0.08, 1.0),
                    )
                };
                part(
                    c,
                    ax,
                    0.45 * half.x,
                    -0.35 * half.y,
                    vec2(0.15, 0.3 * half.y),
                    l,
                );
                part(
                    c,
                    ax,
                    0.45 * half.x,
                    0.35 * half.y,
                    vec2(0.15, 0.3 * half.y),
                    r,
                );
            }
        }
        if overturned {
            obb(c, ax, half * 0.9, Color::new(0.1, 0.1, 0.1, 0.6));
        }
    }
}

/// Layer 5: projectiles in flight.
pub fn draw_projectiles(snap: &Snapshot, view: Rect, zoom: f32) {
    let min = 1.6 / zoom.max(1e-3);
    for p in &snap.projectiles {
        if !view.contains(vec2(p.x, p.y)) {
            continue;
        }
        match p.kind {
            1 => {
                draw_circle(
                    p.x,
                    p.y,
                    (0.5f32).max(min * 2.0),
                    Color::new(1.0, 0.5, 0.1, 0.35),
                );
                draw_circle(
                    p.x,
                    p.y,
                    (0.12f32).max(min),
                    Color::new(1.0, 0.75, 0.2, 1.0),
                );
            }
            2 => {
                draw_circle(
                    p.x,
                    p.y,
                    (0.13f32).max(min),
                    Color::new(0.75, 0.78, 0.8, 1.0),
                );
                draw_circle(
                    p.x,
                    p.y,
                    (0.06f32).max(min * 0.5),
                    Color::new(0.2, 0.2, 0.2, 1.0),
                );
            }
            _ => draw_circle(
                p.x,
                p.y,
                (0.09f32).max(min),
                Color::new(0.2, 0.2, 0.22, 1.0),
            ),
        }
    }
}
