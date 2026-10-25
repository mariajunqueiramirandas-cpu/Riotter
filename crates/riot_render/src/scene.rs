//! Props, vehicles and projectiles from the snapshot (DESIGN §18.2 layers
//! 1, 4 and 5). Props and vehicles are pixel-art sprites from the thing
//! sheet ([`crate::sheets`]) with depth-tested shadows, rebuilt into one
//! stream mesh per frame; there are at most a few thousand of them.

use macroquad::prelude::*;
use riot_sim::snapshot::Snapshot;

use crate::mesh::{MeshBuilder, Pass, Rgba, Tex};
use crate::pixel::View;
use crate::sheets::{CAR_PAINT, Thing, ThingAtlas};

fn hash(i: u32) -> u32 {
    let mut x = i.wrapping_mul(0x9E37_79B1) ^ 0x7F4A_7C15;
    x ^= x >> 16;
    x.wrapping_mul(0x85EB_CA6B) ^ (x >> 13)
}

/// Shadow colour and the offset per metre of height, as on the map.
const SHADOW: Rgba = [18, 22, 48, 92];
const SUN: [f32; 2] = [0.42, 0.58];

/// Layers of the per-frame meshes.
const SHADOWS: usize = 0;
const SPRITES: usize = 1;

fn shade(k: f32, a: u8) -> Rgba {
    let v = (255.0 * k).clamp(0.0, 255.0) as u8;
    [v, v, v, a]
}

/// Snap a moving sprite's centre to whole buffer pixels.
fn snap(p: Vec2, view: &View) -> [f32; 2] {
    if view.pixel {
        let d = view.cam.zoom;
        [(p.x * d).round() / d, (p.y * d).round() / d]
    } else {
        [p.x, p.y]
    }
}

/// Props in `area` and their shadows into `m`.
pub fn build(
    m: &mut MeshBuilder,
    snap_: &Snapshot,
    area: Rect,
    view: &View,
    things: &ThingAtlas,
    _time: f32,
) {
    if view.cam.zoom < 0.4 {
        return;
    }
    for (i, p) in snap_.props.iter().enumerate() {
        let r = p.half_x.max(p.half_y);
        if !area.overlaps(&Rect::new(p.x - r, p.y - r, 2.0 * r, 2.0 * r)) {
            continue;
        }
        let burning = p.flags & 1 != 0;
        let building = p.flags & 2 != 0;
        let destroyed = p.flags & 4 != 0;
        let overturned = p.flags & 8 != 0;
        let health = p.health as f32 / 255.0;
        let (thing, height) = match p.kind {
            0 if destroyed => (Thing::CarBurnt, 1.0),
            0 if overturned => (Thing::CarOverturned, 1.2),
            0 => (
                Thing::Car((hash(i as u32) % CAR_PAINT.len() as u32) as u8),
                1.4,
            ),
            1 => (Thing::Bin, 1.1),
            2 => (Thing::Barrier, 1.0),
            3 => (Thing::Bench, 0.5),
            _ => (Thing::Barricade, 1.2),
        };
        let c = [p.x, p.y];
        let cs = [p.cos, p.sin];
        let half = [p.half_x, p.half_y];
        let sh = [c[0] + SUN[0] * height, c[1] + SUN[1] * height];
        m.at(
            SHADOWS,
            Tex::Mat(crate::materials::Mat::Solid),
            Pass::Shadow,
        )
        .obb(sh, cs, [half[0] * 0.92, half[1] * 0.92], SHADOW);
        let mut col = shade(0.5 + 0.5 * health, 255);
        if burning {
            col = [150, 96, 80, 255];
        }
        if building {
            col[3] = 150;
        }
        let s = things.get(thing);
        m.at(SPRITES, Tex::Things, Pass::Normal)
            .sprite(&s, c, cs, half, col);
    }
}

/// Supply drops in `area` as pallets, close up (zoomed out they are
/// markers, see [`crate::debug`]).
pub fn build_drops(
    m: &mut MeshBuilder,
    snap_: &Snapshot,
    area: Rect,
    view: &View,
    things: &ThingAtlas,
) {
    if view.cam.zoom < crate::debug::DROP_SPRITES_FROM {
        return;
    }
    for d in &snap_.drops {
        if d.amount == 0 || !area.contains(vec2(d.x, d.y)) {
            continue;
        }
        let s = things.get(Thing::Supply(d.kind.min(3)));
        let half = [s.body[0] * 0.5, s.body[1] * 0.5];
        let c = [d.x, d.y];
        let sh = [c[0] + SUN[0] * 0.8, c[1] + SUN[1] * 0.8];
        m.at(
            SHADOWS,
            Tex::Mat(crate::materials::Mat::Solid),
            Pass::Shadow,
        )
        .obb(sh, [1.0, 0.0], [half[0] * 0.95, half[1] * 0.95], SHADOW);
        m.at(SPRITES, Tex::Things, Pass::Normal).sprite(
            &s,
            c,
            [1.0, 0.0],
            half,
            [255, 255, 255, 255],
        );
    }
}

/// Vehicles in `area` and their shadows into `m`, lights flashing on the
/// render clock.
pub fn build_vehicles(
    m: &mut MeshBuilder,
    snap_: &Snapshot,
    area: Rect,
    view: &View,
    things: &ThingAtlas,
    time: f32,
) {
    let blink = (time * 4.0) as u32 % 2;
    for (i, v) in snap_.vehicles.iter().enumerate() {
        let r = v.half_len.max(v.half_wid);
        if !area.overlaps(&Rect::new(v.x - r, v.y - r, 2.0 * r, 2.0 * r)) {
            continue;
        }
        let a = v.heading as f32 * std::f32::consts::TAU / 256.0;
        let cs = [a.cos(), a.sin()];
        let c = snap(vec2(v.x, v.y), view);
        let phase = if v.flags & 1 != 0 || v.flags & 2 != 0 {
            1 + ((blink + i as u32) % 2) as u8
        } else {
            0
        };
        let (thing, height) = match v.kind {
            1 => (Thing::WaterCannon(phase), 3.2),
            2 => (Thing::Ambulance(phase), 2.6),
            _ => (Thing::Van(phase), 2.4),
        };
        let half = [v.half_len, v.half_wid];
        let sh = [c[0] + SUN[0] * height, c[1] + SUN[1] * height];
        m.at(
            SHADOWS,
            Tex::Mat(crate::materials::Mat::Solid),
            Pass::Shadow,
        )
        .obb(sh, cs, [half[0] * 0.95, half[1] * 0.92], SHADOW);
        let col = if v.flags & 4 != 0 {
            shade(0.45, 255)
        } else {
            shade(1.0, 255)
        };
        let s = things.get(thing);
        m.at(SPRITES, Tex::Things, Pass::Normal)
            .sprite(&s, c, cs, half, col);
    }
}

/// Layer 5: projectiles in flight, as small pixel blobs (a world camera
/// into the buffer must be set).
pub fn draw_projectiles(snap: &Snapshot, view: Rect, zoom: f32) {
    let px = 1.0 / zoom.max(1e-3);
    let blob = |x: f32, y: f32, r: f32, c: Color| {
        let r = r.max(px);
        draw_rectangle(x - r, y - r, 2.0 * r, 2.0 * r, c);
    };
    for p in &snap.projectiles {
        if !view.contains(vec2(p.x, p.y)) {
            continue;
        }
        let (x, y) = ((p.x / px).round() * px, (p.y / px).round() * px);
        match p.kind {
            1 => {
                blob(x, y, 0.35, Color::new(1.0, 0.45, 0.1, 0.35));
                blob(x, y, 0.16, Color::new(1.0, 0.72, 0.2, 1.0));
                blob(x, y, 0.07, Color::new(1.0, 0.95, 0.7, 1.0));
            }
            2 => {
                blob(x, y, 0.13, Color::new(0.22, 0.24, 0.26, 1.0));
                blob(x, y, 0.08, Color::new(0.75, 0.78, 0.8, 1.0));
            }
            _ => {
                blob(x, y, 0.1, Color::new(0.12, 0.12, 0.14, 1.0));
                blob(x, y, 0.06, Color::new(0.4, 0.4, 0.42, 1.0));
            }
        }
    }
}
