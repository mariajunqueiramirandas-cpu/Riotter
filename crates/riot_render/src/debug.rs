//! Debug and player-facing markers (DESIGN §18.2 debug overlays): objective
//! rings, supply drops, police squads and their formation targets, the
//! police commander's plan (where it thinks each crowd is going and what it
//! does about it, its barricades and ambushes), entry points, the selected
//! organizer and the cursor tool preview. Shapes are drawn in map
//! coordinates; labels are collected and drawn in screen space afterwards so
//! text stays crisp at every zoom.

use macroquad::prelude::*;
use riot_defs::mapdef::{EntryDef, EntryKind};
use riot_snapshot::Snapshot;

use crate::camera::Camera;

/// Text to draw at a map position after the world pass.
#[derive(Clone, Debug)]
pub struct Label {
    pub at: Vec2,
    pub text: String,
    pub color: Color,
    pub size: f32,
}

/// Objective colours by `ObjectiveKind`: march-to, hold, disperse.
pub const OBJECTIVE_COLOURS: [Color; 3] = [
    Color::new(0.2, 0.85, 0.35, 1.0),
    Color::new(0.3, 0.6, 1.0, 1.0),
    Color::new(1.0, 0.62, 0.15, 1.0),
];

/// Supply colours by `SupplyKind`: water, medic, shields, barricade.
pub const SUPPLY_COLOURS: [Color; 4] = [
    Color::new(0.25, 0.6, 1.0, 1.0),
    Color::new(0.95, 0.95, 0.95, 1.0),
    Color::new(0.62, 0.66, 0.72, 1.0),
    Color::new(0.62, 0.42, 0.22, 1.0),
];

/// Zoom (px/m) from which supply drops are drawn as pallets, not markers.
pub const DROP_SPRITES_FROM: f32 = 6.0;

/// Supply names by `SupplyKind`.
pub const SUPPLY_NAMES: [&str; 4] = ["water", "medic", "shields", "barricade"];
/// Objective names by `ObjectiveKind`.
pub const OBJECTIVE_NAMES: [&str; 3] = ["march-to", "hold", "disperse"];
const DOCTRINES: [&str; riot_defs::police::DOCTRINES] = [
    "idle",
    "travel",
    "hold line",
    "advance",
    "kettle",
    "disperse",
    "snatch",
    "retreat",
    "reserve",
    "barricade",
    "ambush",
    "charge",
    "escort",
];

/// Police operation colours by `OpKind`: none, watch, escort, cordon,
/// protect, contain, disperse.
pub const OP_COLOURS: [Color; 7] = [
    Color::new(0.6, 0.6, 0.6, 1.0),
    Color::new(0.55, 0.75, 1.0, 1.0),
    Color::new(0.4, 0.9, 0.95, 1.0),
    Color::new(1.0, 0.72, 0.2, 1.0),
    Color::new(0.35, 0.5, 1.0, 1.0),
    Color::new(0.8, 0.45, 1.0, 1.0),
    Color::new(1.0, 0.35, 0.3, 1.0),
];

fn with_alpha(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, a)
}

/// A dashed circle `px` pixels wide.
fn dashed_ring(c: Vec2, r: f32, px: f32, zoom: f32, col: Color, phase: f32) {
    let w = px / zoom;
    let n = ((r * zoom / 6.0) as usize).clamp(12, 180);
    for k in 0..n {
        if k % 2 == 1 {
            continue;
        }
        let a0 = (k as f32 + phase) / n as f32 * std::f32::consts::TAU;
        let a1 = (k as f32 + 1.0 + phase) / n as f32 * std::f32::consts::TAU;
        let p0 = c + vec2(a0.cos(), a0.sin()) * r;
        let p1 = c + vec2(a1.cos(), a1.sin()) * r;
        draw_line(p0.x, p0.y, p1.x, p1.y, w, col);
    }
}

/// A dashed segment `px` pixels wide, dashes about 7 pixels long.
fn dashed_line(a: Vec2, b: Vec2, px: f32, zoom: f32, col: Color, phase: f32) {
    let len = (b - a).length();
    if len < 1e-3 {
        return;
    }
    let dash = 7.0 / zoom;
    let dir = (b - a) / len;
    let mut t = (phase.fract() - 1.0) * 2.0 * dash;
    while t < len {
        let (t0, t1) = (t.max(0.0), (t + dash).min(len));
        if t1 > t0 {
            let (p0, p1) = (a + dir * t0, a + dir * t1);
            draw_line(p0.x, p0.y, p1.x, p1.y, px / zoom, col);
        }
        t += 2.0 * dash;
    }
}

/// Seconds as m:ss.
fn mmss(s: f32) -> String {
    let s = s.max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The police commander's plan, over everything: each crowd's predicted
/// route and what the police do about it, barricades by state, and squads
/// waiting in ambush around their corners.
pub fn draw_police_plan(snap: &Snapshot, cam: &Camera, time: f32, labels: &mut Vec<Label>) {
    let z = cam.zoom;
    for t in &snap.tracks {
        let col = OP_COLOURS[(t.op as usize).min(6)];
        let n = t.route_n as usize;
        if n >= 2 && t.op != 0 {
            let faint = with_alpha(col, 0.55);
            for k in 0..n - 1 {
                let a = vec2(t.route[k][0], t.route[k][1]);
                let b = vec2(t.route[k + 1][0], t.route[k + 1][1]);
                dashed_line(a, b, 1.5, z, faint, -time * 1.5);
            }
            // An arrowhead where the route ends.
            let a = vec2(t.route[n - 2][0], t.route[n - 2][1]);
            let b = vec2(t.route[n - 1][0], t.route[n - 1][1]);
            let d = (b - a).normalize_or_zero();
            let s = 7.0 / z;
            let l = b - d * s + d.perp() * s * 0.6;
            let r = b - d * s - d.perp() * s * 0.6;
            draw_triangle(b, l, r, faint);
        }
        if z > 0.35 && t.op != 0 {
            let name = riot_defs::police::OpKind::from_u8(t.op).name();
            let eta = if t.site && t.eta.is_finite() && t.eta < 3600.0 {
                format!(" · site in {}", mmss(t.eta))
            } else {
                String::new()
            };
            labels.push(Label {
                at: vec2(t.x, t.y - t.radius.min(30.0)),
                text: format!("{:.0} → {}{}", t.count, name, eta),
                color: col,
                size: 14.0,
            });
        }
    }
    for c in &snap.cordons {
        let p = vec2(c.x, c.y);
        let ax = vec2(c.axis[0], c.axis[1]);
        let face = vec2(c.face[0], c.face[1]);
        let (a, b) = (p - ax * c.half, p + ax * c.half);
        // Planned, building, up, breached.
        let (col, text) = match c.state {
            0 => (Color::new(1.0, 0.8, 0.3, 0.7), "barricade ordered"),
            1 => (Color::new(1.0, 0.75, 0.2, 0.85), "putting up a barricade"),
            2 => (Color::new(1.0, 0.62, 0.1, 0.95), "police barricade"),
            _ => (Color::new(1.0, 0.25, 0.2, 0.95), "barricade breached"),
        };
        // The plan, until the barriers stand and speak for themselves; zoomed
        // out, where a barrier is a pixel, a mark on the crowd's side.
        if c.state < 2 {
            dashed_line(a, b, 3.0, z, col, time);
        } else if z < 6.0 {
            let off = face * (3.0 / z);
            draw_line(
                a.x + off.x,
                a.y + off.y,
                b.x + off.x,
                b.y + off.y,
                2.0 / z,
                col,
            );
        }
        if z > 0.6 {
            labels.push(Label {
                at: p + face * (8.0 / z.min(1.5)),
                text: text.into(),
                color: col,
                size: 13.0,
            });
        }
        let at = vec2(c.ambush_at[0], c.ambush_at[1]);
        let j = vec2(c.junction[0], c.junction[1]);
        match c.ambush {
            1 => {
                // Waiting around the corner.
                let red = Color::new(1.0, 0.2, 0.25, 0.75);
                let r = (2.5f32).max(6.0 / z);
                let pulse = 0.6 + 0.4 * (time * 3.0).sin();
                draw_poly_lines(
                    at.x,
                    at.y,
                    4,
                    r,
                    45.0,
                    2.0 / z,
                    with_alpha(red, 0.5 + 0.4 * pulse),
                );
                dashed_line(at, j, 1.5, z, with_alpha(red, 0.5), time);
                if z > 0.8 {
                    labels.push(Label {
                        at: at + vec2(0.0, r + 6.0 / z),
                        text: "ambush".into(),
                        color: red,
                        size: 13.0,
                    });
                }
            }
            2 => {
                let red = Color::new(1.0, 0.15, 0.15, 0.9);
                draw_line(at.x, at.y, j.x, j.y, 3.0 / z, red);
                let d = (j - at).normalize_or_zero();
                let s = 9.0 / z;
                draw_triangle(
                    j + d * s,
                    j + d.perp() * s * 0.7,
                    j - d.perp() * s * 0.7,
                    red,
                );
            }
            _ => {}
        }
    }
}

/// Objective areas and supply drops, on the ground under people.
pub fn draw_ground_markers(
    snap: &Snapshot,
    cam: &Camera,
    highlight: Option<u8>,
    time: f32,
    labels: &mut Vec<Label>,
) {
    let z = cam.zoom;
    for o in &snap.objectives {
        let col = OBJECTIVE_COLOURS[(o.kind as usize).min(2)];
        let c = vec2(o.x, o.y);
        // Light enough to keep the ground readable under a crowd.
        let fill = if o.held { 0.09 } else { 0.05 };
        draw_circle(c.x, c.y, o.radius, with_alpha(col, fill));
        let hl = highlight == Some(o.slot);
        dashed_ring(
            c,
            o.radius,
            if hl { 3.5 } else { 2.0 },
            z,
            with_alpha(col, 0.9),
            time * 0.5,
        );
        labels.push(Label {
            at: c + vec2(0.0, -o.radius),
            text: format!(
                "{} {}{}",
                o.slot,
                OBJECTIVE_NAMES[(o.kind as usize).min(2)],
                if o.held { " (held)" } else { "" }
            ),
            color: col,
            size: if hl { 20.0 } else { 16.0 },
        });
    }
    for d in &snap.drops {
        let col = SUPPLY_COLOURS[(d.kind as usize).min(3)];
        let c = vec2(d.x, d.y);
        let r = (0.9f32).max(5.0 / z);
        // Close up the pallet itself is drawn (`scene::build_drops`).
        if z < DROP_SPRITES_FROM {
            draw_circle(c.x, c.y, r * 1.15, Color::new(0.05, 0.05, 0.05, 0.8));
            draw_circle(c.x, c.y, r, col);
            if d.kind == 1 {
                let red = Color::new(0.85, 0.1, 0.1, 1.0);
                draw_rectangle(c.x - r * 0.6, c.y - r * 0.18, r * 1.2, r * 0.36, red);
                draw_rectangle(c.x - r * 0.18, c.y - r * 0.6, r * 0.36, r * 1.2, red);
            }
        }
        if z > 2.0 {
            labels.push(Label {
                at: c + vec2(0.0, r * 1.2),
                text: format!("{} {}", SUPPLY_NAMES[(d.kind as usize).min(3)], d.amount),
                color: col,
                size: 14.0,
            });
        }
    }
}

/// Squads, entries and other debug markers, above people.
pub fn draw_debug_markers(
    snap: &Snapshot,
    entries: &[EntryDef],
    cam: &Camera,
    time: f32,
    labels: &mut Vec<Label>,
) {
    let z = cam.zoom;
    let blue = Color::new(0.35, 0.55, 1.0, 0.9);
    draw_police_plan(snap, cam, time, labels);
    for s in &snap.squads {
        let c = vec2(s.x, s.y);
        let t = vec2(s.tx, s.ty);
        draw_line(c.x, c.y, t.x, t.y, 1.5 / z, with_alpha(blue, 0.6));
        let r = 4.0 / z;
        draw_rectangle_lines(t.x - r, t.y - r, 2.0 * r, 2.0 * r, 1.5 / z, blue);
        if z > 0.8 {
            labels.push(Label {
                at: c + vec2(0.0, 6.0 / z.min(1.0)),
                text: format!(
                    "{} ×{}",
                    DOCTRINES[(s.doctrine as usize).min(DOCTRINES.len() - 1)],
                    s.officers
                ),
                color: blue,
                size: 13.0,
            });
        }
    }
    for e in entries {
        let c = vec2(e.pos[0], e.pos[1]);
        let col = if e.kind == EntryKind::Metro {
            Color::new(0.9, 0.2, 0.2, 0.9)
        } else {
            Color::new(0.9, 0.9, 0.9, 0.5)
        };
        draw_circle_lines(c.x, c.y, e.radius.max(3.0 / z), 1.5 / z, col);
        if e.kind == EntryKind::Metro && z > 0.5 {
            labels.push(Label {
                at: c,
                text: "M".into(),
                color: col,
                size: 18.0,
            });
        }
    }
}

/// A pulsing ring around the selected organizer.
pub fn draw_selection(at: Vec2, zoom: f32, time: f32, chanting: bool) {
    let r = 0.9 + 0.15 * (time * 5.0).sin();
    let r = r.max(9.0 / zoom);
    let col = Color::new(1.0, 0.9, 0.1, 0.95);
    draw_circle_lines(at.x, at.y, r, 2.5 / zoom, col);
    if chanting {
        let k = (time * 2.0).fract();
        draw_circle_lines(
            at.x,
            at.y,
            r + k * 10.0,
            1.5 / zoom,
            with_alpha(col, 1.0 - k),
        );
    }
}

/// A preview of the active tool at the cursor.
pub fn draw_tool_preview(at: Vec2, radius: f32, zoom: f32, col: Color) {
    draw_circle(at.x, at.y, radius, with_alpha(col, 0.08));
    draw_circle_lines(at.x, at.y, radius, 1.5 / zoom, with_alpha(col, 0.8));
    let k = 6.0 / zoom;
    draw_line(at.x - k, at.y, at.x + k, at.y, 1.5 / zoom, col);
    draw_line(at.x, at.y - k, at.x, at.y + k, 1.5 / zoom, col);
}

/// Draw collected labels in screen space (the default camera must be set).
pub fn draw_labels(labels: &[Label], cam: &Camera) {
    for l in labels {
        let s = cam.world_to_screen(l.at);
        if s.x < -100.0 || s.y < -20.0 || s.x > cam.screen.x + 100.0 || s.y > cam.screen.y + 20.0 {
            continue;
        }
        let d = measure_text(&l.text, None, l.size as u16, 1.0);
        let x = s.x - d.width * 0.5;
        let y = s.y - 4.0;
        draw_text(
            &l.text,
            x + 1.0,
            y + 1.0,
            l.size,
            Color::new(0.0, 0.0, 0.0, 0.8),
        );
        draw_text(&l.text, x, y, l.size, l.color);
    }
}
