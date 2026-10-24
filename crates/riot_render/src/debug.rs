//! Debug and player-facing markers (DESIGN §18.2 debug overlays): objective
//! rings, supply drops, police squads and their formation targets, entry
//! points, the selected organizer and the cursor tool preview. Shapes are
//! drawn in map coordinates; labels are collected and drawn in screen space
//! afterwards so text stays crisp at every zoom.

use macroquad::prelude::*;
use riot_sim::mapdef::{EntryDef, EntryKind};
use riot_sim::snapshot::Snapshot;

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

/// Supply names by `SupplyKind`.
pub const SUPPLY_NAMES: [&str; 4] = ["water", "medic", "shields", "barricade"];
/// Objective names by `ObjectiveKind`.
pub const OBJECTIVE_NAMES: [&str; 3] = ["march-to", "hold", "disperse"];
const DOCTRINES: [&str; 9] = [
    "idle",
    "travel",
    "hold line",
    "advance",
    "kettle",
    "disperse",
    "snatch",
    "retreat",
    "reserve",
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
        let fill = if o.held { 0.22 } else { 0.1 };
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
        draw_circle(c.x, c.y, r * 1.15, Color::new(0.05, 0.05, 0.05, 0.8));
        draw_circle(c.x, c.y, r, col);
        if d.kind == 1 {
            let red = Color::new(0.85, 0.1, 0.1, 1.0);
            draw_rectangle(c.x - r * 0.6, c.y - r * 0.18, r * 1.2, r * 0.36, red);
            draw_rectangle(c.x - r * 0.18, c.y - r * 0.6, r * 0.36, r * 1.2, red);
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
    labels: &mut Vec<Label>,
) {
    let z = cam.zoom;
    let blue = Color::new(0.35, 0.55, 1.0, 0.9);
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
                    DOCTRINES[(s.doctrine as usize).min(8)],
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
