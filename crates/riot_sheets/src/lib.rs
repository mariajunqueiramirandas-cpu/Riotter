//! Colour sprite sheets in pixel art: decals on the ground and on roofs, and
//! the props and vehicles of a riot.
//!
//! Sprites are painted from shapes in metres (see [`riot_art::Sheet`]) at
//! every art level, into atlases with the same layout in units of
//! [`UNIT`] level-0 pixels. Each sprite's cell is sized in whole units around
//! its nominal size, so a quad of the cell's size in metres shows it texel
//! for pixel.

use riot_art::{self as art, Img, P, Rgb, Shape, Sheet, UNIT, hex, mix, rnd, tone};

/// Atlas width in units.
const ATLAS_UNITS: usize = 64;

/// Metres per unit.
pub const UNIT_M: f32 = UNIT as f32 / art::DENSITY;

/// Where a sprite is: a unit rectangle of its atlas and the nominal size of
/// what it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub rect: [usize; 4],
    /// Nominal size in metres (the drawn object, inside its cell).
    pub body: [f32; 2],
    /// Atlas size in units.
    pub atlas: [usize; 2],
}

impl Sprite {
    /// Cell size in metres.
    pub fn cell_m(&self) -> [f32; 2] {
        [self.rect[2] as f32 * UNIT_M, self.rect[3] as f32 * UNIT_M]
    }

    /// Normalized texture rectangle: u0, v0, u1, v1.
    pub fn uv(&self) -> [f32; 4] {
        let (w, h) = (self.atlas[0] as f32, self.atlas[1] as f32);
        [
            self.rect[0] as f32 / w,
            self.rect[1] as f32 / h,
            (self.rect[0] + self.rect[2]) as f32 / w,
            (self.rect[1] + self.rect[3]) as f32 / h,
        ]
    }

    /// Half extents of the quad that draws this sprite over an object of
    /// half size `half` (texel for pixel when `half` is the nominal size).
    pub fn quad_half(&self, half: [f32; 2]) -> [f32; 2] {
        let c = self.cell_m();
        [
            c[0] * 0.5 * (2.0 * half[0] / self.body[0]),
            c[1] * 0.5 * (2.0 * half[1] / self.body[1]),
        ]
    }
}

/// Shelf-pack cells (in units, with a one-unit gutter) into rows of
/// [`ATLAS_UNITS`]. Returns the rectangles and the atlas height in units.
fn pack(sizes: &[[usize; 2]]) -> (Vec<[usize; 4]>, usize) {
    let (mut x, mut y, mut row_h) = (0usize, 0usize, 0usize);
    let mut out = Vec::with_capacity(sizes.len());
    for &[w, h] in sizes {
        if x + w + 1 > ATLAS_UNITS {
            x = 0;
            y += row_h + 1;
            row_h = 0;
        }
        out.push([x, y, w, h]);
        x += w + 1;
        row_h = row_h.max(h);
    }
    let h = (y + row_h + 1).next_power_of_two();
    (out, h)
}

/// Cell size in units for an object of `body` metres plus a margin.
fn cell(body: [f32; 2], margin: f32) -> [usize; 2] {
    let f = |m: f32| ((m + 2.0 * margin) / UNIT_M).ceil() as usize;
    [f(body[0]), f(body[1])]
}

/// One painted atlas level.
pub struct AtlasLevel {
    pub img: Img,
}

// ---------------------------------------------------------------------------
// Decals

/// Ground and roof decals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decal {
    Manhole,
    Drain,
    Stain,
    Crack,
    Litter,
    Leaves,
    AirCon,
    Vent,
    Skylight,
    Solar,
    Tank,
    Hut,
    Chimney,
}

impl Decal {
    pub const ALL: [Decal; 13] = [
        Decal::Manhole,
        Decal::Drain,
        Decal::Stain,
        Decal::Crack,
        Decal::Litter,
        Decal::Leaves,
        Decal::AirCon,
        Decal::Vent,
        Decal::Skylight,
        Decal::Solar,
        Decal::Tank,
        Decal::Hut,
        Decal::Chimney,
    ];

    /// Nominal size in metres.
    pub fn body(self) -> [f32; 2] {
        match self {
            Decal::Manhole => [0.7, 0.7],
            Decal::Drain => [0.6, 0.32],
            Decal::Stain => [1.5, 1.0],
            Decal::Crack => [2.2, 0.7],
            Decal::Litter => [0.9, 0.9],
            Decal::Leaves => [1.3, 1.3],
            Decal::AirCon => [1.3, 0.85],
            Decal::Vent => [0.5, 0.5],
            Decal::Skylight => [1.7, 1.1],
            Decal::Solar => [1.1, 1.8],
            Decal::Tank => [1.5, 1.5],
            Decal::Hut => [2.4, 1.8],
            Decal::Chimney => [0.7, 0.7],
        }
    }

    /// Height above the roof (m), for shadows; 0 for flat decals.
    pub fn height(self) -> f32 {
        match self {
            Decal::AirCon => 0.5,
            Decal::Vent => 0.4,
            Decal::Skylight => 0.15,
            Decal::Solar => 0.25,
            Decal::Tank => 0.9,
            Decal::Hut => 1.4,
            Decal::Chimney => 0.9,
            _ => 0.0,
        }
    }
}

/// Where the decals are.
pub struct DecalAtlas {
    pub sprites: Vec<Sprite>,
    pub units: [usize; 2],
}

impl DecalAtlas {
    pub fn layout() -> DecalAtlas {
        let sizes: Vec<[usize; 2]> = Decal::ALL.iter().map(|d| cell(d.body(), 0.08)).collect();
        let (rects, h) = pack(&sizes);
        let units = [ATLAS_UNITS, h];
        DecalAtlas {
            sprites: Decal::ALL
                .iter()
                .zip(rects)
                .map(|(d, rect)| Sprite {
                    rect,
                    body: d.body(),
                    atlas: units,
                })
                .collect(),
            units,
        }
    }

    pub fn get(&self, d: Decal) -> Sprite {
        self.sprites[Decal::ALL.iter().position(|&x| x == d).unwrap_or(0)]
    }

    /// Paint every decal at an art level.
    pub fn paint(&self, level: usize) -> AtlasLevel {
        let mut s = Sheet::new(self.units[0], self.units[1], level);
        for (k, &d) in Decal::ALL.iter().enumerate() {
            s.begin(self.sprites[k].rect);
            paint_decal(&mut s, d);
        }
        AtlasLevel { img: s.finish() }
    }
}

fn paint_decal(s: &mut Sheet, d: Decal) {
    let px = s.px();
    let dark = hex(0x24232a);
    match d {
        Decal::Manhole => {
            let iron = hex(0x5a5b60);
            s.fill(&Shape::Circle([0.0, 0.0], 0.35), hex(0x3a3b40));
            s.shaded(&Shape::Circle([0.0, 0.0], 0.33), iron, 0.4);
            s.fill(
                &Shape::Circle([0.0, 0.0], 0.33 - 1.5 * px),
                tone(iron, -0.15),
            );
            if s.level <= 1 {
                // A grid of raised studs.
                for i in -3..=3 {
                    for j in -3..=3 {
                        let p = [i as f32 * 0.075, j as f32 * 0.075];
                        if p[0].hypot(p[1]) < 0.26 && (i + j) % 2 == 0 {
                            s.fill(&Shape::Circle(p, 0.6 * px), tone(iron, 0.25));
                        }
                    }
                }
            } else {
                s.fill(&Shape::Circle([0.0, 0.0], 0.12), tone(iron, 0.1));
            }
        }
        Decal::Drain => {
            s.fill(
                &Shape::Box([0.0, 0.0], [0.3, 0.16], 0.0, 0.0),
                hex(0x56575c),
            );
            s.fill(&Shape::Box([0.0, 0.0], [0.26, 0.12], 0.0, 0.0), dark);
            for k in -3..=3 {
                let x = k as f32 * 0.07;
                s.line([x, -0.11], [x, 0.11], 1.0, hex(0x6c6d72));
            }
        }
        Decal::Stain => {
            let c = hex(0x2e3036);
            for (cx, cy, rx, ry) in [
                (-0.2, 0.0, 0.55, 0.38),
                (0.35, 0.1, 0.35, 0.3),
                (0.1, -0.25, 0.25, 0.2),
            ] {
                let sh = Shape::Ellipse([cx, cy], [rx, ry]);
                s.dithered(&sh, c, c, |_| 0.0);
            }
            // Thin it out to a stain: clear every other pixel.
            let [ox, oy, w, h] = s.at;
            for y in oy..oy + h {
                for x in ox..ox + w {
                    if s.img.get(x, y)[3] > 0 && art::bayer(x as i32, y as i32) > 0.45 {
                        s.img.set_rgba(x, y, [0, 0, 0, 0]);
                    }
                }
            }
        }
        Decal::Crack => {
            let c = hex(0x3a3935);
            let mut p: P = [-1.0, 0.05];
            for i in 0..14 {
                let q = [
                    p[0] + 0.15,
                    (p[1] + (rnd(i, 0, 301) - 0.5) * 0.18).clamp(-0.3, 0.3),
                ];
                s.line(p, q, 1.0, c);
                if i == 5 || i == 10 {
                    let b = [q[0] + 0.1, q[1] + 0.22 * if i == 5 { 1.0 } else { -1.0 }];
                    s.line(q, b, 1.0, c);
                }
                p = q;
            }
        }
        Decal::Litter => {
            let bits = [
                ([-0.25, -0.2], hex(0xeeeee6)),
                ([0.2, 0.1], hex(0xd8d4c4)),
                ([0.05, -0.3], hex(0xc23a2e)),
                ([-0.1, 0.28], hex(0x3d7ac2)),
                ([0.3, -0.15], hex(0xe8e2ce)),
            ];
            for (k, (p, c)) in bits.into_iter().enumerate() {
                let a = rnd(k as i32, 1, 302) * 3.0;
                s.fill(&Shape::Box(p, [0.07, 0.045], a, 0.0), c);
            }
            s.fill(
                &Shape::Capsule([-0.3, 0.1], [-0.18, 0.12], 0.035),
                hex(0xb02a24),
            );
        }
        Decal::Leaves => {
            let cols = [hex(0xc8742c), hex(0xd99a3a), hex(0x8a5a2a), hex(0xb55a22)];
            for k in 0..26 {
                let a = rnd(k, 0, 303) * std::f32::consts::TAU;
                let r = rnd(k, 1, 303).sqrt() * 0.55;
                let p = [a.cos() * r, a.sin() * r];
                s.fill(&Shape::Ellipse(p, [0.05, 0.032]), cols[(k % 4) as usize]);
            }
        }
        Decal::AirCon => {
            let body = hex(0xc9cbcd);
            s.shaded(&Shape::Box([0.0, 0.0], [0.65, 0.42], 0.0, 0.04), body, 0.3);
            s.fill(&Shape::Circle([0.25, 0.0], 0.3), hex(0x45474c));
            s.fill(&Shape::Circle([0.25, 0.0], 0.3 - 1.5 * px), hex(0x2c2d31));
            if s.level <= 1 {
                for k in 0..4 {
                    let a = k as f32 * std::f32::consts::FRAC_PI_2 + 0.4;
                    s.line(
                        [0.25, 0.0],
                        [0.25 + a.cos() * 0.24, a.sin() * 0.24],
                        1.5,
                        hex(0x6a6c71),
                    );
                }
            }
            s.fill(&Shape::Circle([0.25, 0.0], 0.06), hex(0x8d8f94));
            for k in 0..4 {
                let y = -0.24 + k as f32 * 0.16;
                s.line([-0.55, y], [-0.15, y], 1.0, tone(body, -0.3));
            }
            s.outline(hex(0x3a3b40));
        }
        Decal::Vent => {
            s.shaded(&Shape::Circle([0.0, 0.0], 0.24), hex(0x9aa0a6), 0.9);
            s.fill(&Shape::Circle([0.0, 0.0], 0.08), hex(0x6a6f75));
            s.outline(hex(0x3a3b40));
        }
        Decal::Skylight => {
            let frame = hex(0xd6d6d2);
            s.fill(&Shape::Box([0.0, 0.0], [0.85, 0.55], 0.0, 0.0), frame);
            for x in [-0.4, 0.4] {
                let glass = Shape::Box([x, 0.0], [0.37, 0.47], 0.0, 0.0);
                s.dithered(&glass, hex(0x4f6f8c), hex(0x6d8eaa), |p| {
                    // A diagonal reflection.
                    if (p[0] - x + p[1] * 0.8).abs() < 0.1 {
                        1.0
                    } else {
                        0.2
                    }
                });
            }
            s.outline(hex(0x3a3b40));
        }
        Decal::Solar => {
            s.fill(
                &Shape::Box([0.0, 0.0], [0.55, 0.9], 0.0, 0.0),
                hex(0xb9bcc0),
            );
            let cell_c = hex(0x26365e);
            s.fill(&Shape::Box([0.0, 0.0], [0.5, 0.85], 0.0, 0.0), cell_c);
            if s.level <= 1 {
                for k in -3..=3 {
                    let y = k as f32 * 0.24;
                    s.line([-0.5, y], [0.5, y], 1.0, hex(0x5a6c94));
                }
                s.line([0.0, -0.85], [0.0, 0.85], 1.0, hex(0x5a6c94));
            }
            s.fill(
                &Shape::Box([-0.2, -0.45], [0.06, 0.25], 0.5, 0.0),
                hex(0x4a5f8f),
            );
            s.outline(hex(0x2a2b30));
        }
        Decal::Tank => {
            let c = hex(0xb7bbc0);
            s.shaded(&Shape::Circle([0.0, 0.0], 0.74), c, 1.0);
            if s.level <= 2 {
                s.fill_with(&Shape::Circle([0.0, 0.0], 0.74), |p, _, _| {
                    let r = p[0].hypot(p[1]);
                    ((r * 9.0).fract() < 0.18 && r > 0.3).then_some(tone(c, -0.2))
                });
            }
            s.fill(&Shape::Circle([-0.1, -0.1], 0.18), tone(c, 0.2));
            s.outline(hex(0x3a3b40));
        }
        Decal::Hut => {
            let c = hex(0xbcb9b0);
            s.fill(&Shape::Box([0.0, 0.0], [1.2, 0.9], 0.0, 0.0), tone(c, 0.12));
            s.fill(
                &Shape::Box([0.0, 0.0], [1.2 - 2.0 * px, 0.9 - 2.0 * px], 0.0, 0.0),
                c,
            );
            s.fill(
                &Shape::Box([0.0, 0.0], [1.2 - 3.0 * px, 0.9 - 3.0 * px], 0.0, 0.0),
                hex(0x8c8a84),
            );
            s.fill(&Shape::Circle([-0.6, 0.3], 0.12), hex(0x9aa0a6));
            s.outline(hex(0x3a3b40));
        }
        Decal::Chimney => {
            let brick = hex(0x9a4a35);
            s.shaded(&Shape::Box([0.0, 0.0], [0.34, 0.34], 0.0, 0.0), brick, 0.3);
            s.fill(&Shape::Box([0.0, 0.0], [0.2, 0.2], 0.0, 0.0), dark);
            s.outline(hex(0x2e2224));
        }
    }
}

// ---------------------------------------------------------------------------
// Props and vehicles

/// Props and vehicles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Thing {
    /// A parked car in one of [`CAR_PAINT`]'s colours.
    Car(u8),
    CarBurnt,
    CarOverturned,
    Bin,
    Bench,
    Barrier,
    Barricade,
    /// A police crowd-control barrier: steel, hazard stripes, a blue panel.
    PoliceBarrier,
    /// Police van; lights off (0) or flashing (1, 2).
    Van(u8),
    WaterCannon(u8),
    Ambulance(u8),
    /// A dropped pallet of supplies by `SupplyKind`: water, medic, shields,
    /// barricade material.
    Supply(u8),
}

/// Car paints.
pub const CAR_PAINT: [u32; 8] = [
    0xb3262a, 0x2c4e9a, 0xe4e4de, 0x26272c, 0x9fa3a8, 0x2f6a46, 0xd9a52b, 0x6a6f78,
];

impl Thing {
    pub fn all() -> Vec<Thing> {
        let mut v: Vec<Thing> = (0..CAR_PAINT.len() as u8).map(Thing::Car).collect();
        v.extend([
            Thing::CarBurnt,
            Thing::CarOverturned,
            Thing::Bin,
            Thing::Bench,
            Thing::Barrier,
            Thing::Barricade,
            Thing::PoliceBarrier,
        ]);
        for k in 0..3 {
            v.extend([Thing::Van(k), Thing::WaterCannon(k), Thing::Ambulance(k)]);
        }
        v.extend((0..4).map(Thing::Supply));
        v
    }

    /// Nominal size in metres, length along the object's axis first.
    pub fn body(self) -> [f32; 2] {
        match self {
            Thing::Car(_) | Thing::CarBurnt | Thing::CarOverturned => [4.4, 1.8],
            Thing::Bin => [0.7, 0.7],
            Thing::Bench => [1.8, 0.6],
            Thing::Barrier => [2.4, 0.24],
            Thing::Barricade => [1.0, 4.8],
            Thing::PoliceBarrier => [2.66, 0.6],
            Thing::Van(_) | Thing::Ambulance(_) => [5.2, 2.0],
            Thing::WaterCannon(_) => [8.0, 2.5],
            Thing::Supply(_) => [1.3, 1.3],
        }
    }
}

/// Where the props and vehicles are.
pub struct ThingAtlas {
    pub things: Vec<Thing>,
    pub sprites: Vec<Sprite>,
    pub units: [usize; 2],
}

impl ThingAtlas {
    pub fn layout() -> ThingAtlas {
        let things = Thing::all();
        let sizes: Vec<[usize; 2]> = things.iter().map(|t| cell(t.body(), 0.1)).collect();
        let (rects, h) = pack(&sizes);
        let units = [ATLAS_UNITS, h];
        ThingAtlas {
            sprites: things
                .iter()
                .zip(rects)
                .map(|(t, rect)| Sprite {
                    rect,
                    body: t.body(),
                    atlas: units,
                })
                .collect(),
            things,
            units,
        }
    }

    pub fn get(&self, t: Thing) -> Sprite {
        self.sprites[self.things.iter().position(|&x| x == t).unwrap_or(0)]
    }

    /// Paint every prop and vehicle at an art level.
    pub fn paint(&self, level: usize) -> AtlasLevel {
        let mut s = Sheet::new(self.units[0], self.units[1], level);
        for (k, &t) in self.things.iter().enumerate() {
            s.begin(self.sprites[k].rect);
            paint_thing(&mut s, t);
        }
        AtlasLevel { img: s.finish() }
    }
}

const OUTLINE: Rgb = [28, 24, 32];
const GLASS: Rgb = [44, 58, 74];

/// A car body seen from above, nose towards +x.
fn car_body(s: &mut Sheet, paint: Rgb, half: [f32; 2]) {
    let [hx, hy] = half;
    s.shaded(&Shape::Box([0.0, 0.0], [hx, hy], 0.0, 0.32), paint, 0.7);
    // Bonnet crease and boot.
    let px = s.px();
    s.fill(
        &Shape::Box([hx * 0.72, 0.0], [hx * 0.2, hy * 0.62], 0.0, 0.08),
        tone(paint, 0.08),
    );
    // Windscreen, rear window, side windows.
    let glass = Shape::Poly(vec![
        [hx * 0.18, -hy * 0.78],
        [hx * 0.48, -hy * 0.7],
        [hx * 0.48, hy * 0.7],
        [hx * 0.18, hy * 0.78],
    ]);
    s.dithered(&glass, GLASS, hex(0x7f97ad), |p| {
        if (p[0] - hx * 0.3 + p[1] * 0.5).abs() < 0.07 {
            1.0
        } else {
            0.0
        }
    });
    s.fill(
        &Shape::Poly(vec![
            [-hx * 0.8, -hy * 0.7],
            [-hx * 0.62, -hy * 0.76],
            [-hx * 0.62, hy * 0.76],
            [-hx * 0.8, hy * 0.7],
        ]),
        GLASS,
    );
    for side in [-1.0, 1.0] {
        s.fill(
            &Shape::Box(
                [-hx * 0.22, side * hy * 0.82],
                [hx * 0.38, 0.6 * px.max(0.035)],
                0.0,
                0.0,
            ),
            GLASS,
        );
    }
    // Roof.
    s.shaded(
        &Shape::Box([-hx * 0.22, 0.0], [hx * 0.38, hy * 0.72], 0.0, 0.1),
        tone(paint, 0.1),
        0.4,
    );
    // Lights and mirrors.
    for side in [-1.0, 1.0] {
        s.fill(
            &Shape::Box([hx - 0.06, side * hy * 0.68], [0.05, 0.16], 0.0, 0.0),
            hex(0xf4f0c8),
        );
        s.fill(
            &Shape::Box([-hx + 0.05, side * hy * 0.7], [0.04, 0.14], 0.0, 0.0),
            hex(0xc8282a),
        );
        s.fill(
            &Shape::Box([hx * 0.22, side * (hy + 0.05)], [0.05, 0.07], 0.0, 0.0),
            tone(paint, -0.2),
        );
    }
}

fn paint_thing(s: &mut Sheet, t: Thing) {
    let px = s.px();
    match t {
        Thing::Car(k) => {
            car_body(s, hex(CAR_PAINT[k as usize % CAR_PAINT.len()]), [2.2, 0.9]);
            s.outline(OUTLINE);
        }
        Thing::CarBurnt => {
            let char_c = hex(0x2f2a28);
            s.dithered(
                &Shape::Box([0.0, 0.0], [2.2, 0.9], 0.0, 0.32),
                char_c,
                hex(0x6e4128),
                |p| art::fbm(p[0] * 0.2 + 0.5, p[1] * 0.2 + 0.5, 4, 2, 311) - 0.25,
            );
            // Empty window frames and a caved roof.
            for (x, w) in [(0.73, 0.33), (-1.55, 0.2)] {
                s.fill(
                    &Shape::Box([x, 0.0], [w * 0.5, 0.66], 0.0, 0.0),
                    hex(0x141214),
                );
            }
            s.fill(
                &Shape::Box([-0.48, 0.0], [0.82, 0.62], 0.0, 0.08),
                hex(0x3b3533),
            );
            s.fill(&Shape::Circle([-0.3, 0.1], 0.22), hex(0x232022));
            s.outline(OUTLINE);
        }
        Thing::CarOverturned => {
            let chassis = hex(0x3c3d42);
            s.shaded(&Shape::Box([0.0, 0.0], [2.2, 0.9], 0.0, 0.3), chassis, 0.5);
            for (x, y) in [(1.35, 0.68), (1.35, -0.68), (-1.35, 0.68), (-1.35, -0.68)] {
                s.fill(&Shape::Box([x, y], [0.33, 0.13], 0.0, 0.05), hex(0x18181b));
                s.fill(&Shape::Box([x, y], [0.15, 0.08], 0.0, 0.0), hex(0x6a6c72));
            }
            s.line([1.35, -0.6], [1.35, 0.6], 2.0, hex(0x2a2b30));
            s.line([-1.35, -0.6], [-1.35, 0.6], 2.0, hex(0x2a2b30));
            s.line([-2.1, 0.4], [0.8, 0.3], 1.5, hex(0x7a7c80));
            s.outline(OUTLINE);
        }
        Thing::Bin => {
            let green = hex(0x2f5a3a);
            s.shaded(&Shape::Box([0.0, 0.0], [0.35, 0.35], 0.0, 0.05), green, 0.4);
            s.fill(
                &Shape::Box([-0.28, 0.0], [0.05, 0.33], 0.0, 0.0),
                tone(green, -0.3),
            );
            s.fill(
                &Shape::Box([0.27, 0.0], [0.035, 0.2], 0.0, 0.0),
                tone(green, 0.3),
            );
            s.outline(OUTLINE);
        }
        Thing::Bench => {
            let frame = hex(0x34363b);
            for x in [-0.75, 0.75] {
                s.fill(&Shape::Box([x, 0.0], [0.06, 0.3], 0.0, 0.0), frame);
            }
            for k in 0..3 {
                let y = -0.2 + k as f32 * 0.2;
                let c = [hex(0x9a6e42), hex(0x8a623a), hex(0xa57849)][k];
                s.fill(&Shape::Box([0.0, y], [0.9, 0.08], 0.0, 0.0), c);
                s.line([-0.88, y - 0.05], [0.88, y - 0.05], 1.0, tone(c, 0.2));
            }
            s.outline(OUTLINE);
        }
        Thing::Barrier => {
            let steel = hex(0xb9bec4);
            for x in [-1.1, 1.1] {
                s.fill(&Shape::Box([x, 0.0], [0.08, 0.12], 0.0, 0.0), hex(0x55585e));
            }
            s.fill(&Shape::Box([0.0, 0.0], [1.2, 0.05], 0.0, 0.0), steel);
            for k in 0..9 {
                let x = -1.0 + k as f32 * 0.25;
                s.fill(
                    &Shape::Box([x, 0.0], [0.03, 0.05], 0.0, 0.0),
                    if k % 2 == 0 {
                        hex(0xe0b52a)
                    } else {
                        tone(steel, 0.2)
                    },
                );
            }
            s.outline(OUTLINE);
        }
        Thing::PoliceBarrier => {
            // Feet, a steel frame in hazard stripes and a POLICE panel.
            for x in [-1.18, 1.18] {
                s.fill(&Shape::Box([x, 0.0], [0.1, 0.28], 0.0, 0.03), hex(0x45484e));
            }
            let steel = hex(0xc9ced4);
            s.shaded(&Shape::Box([0.0, 0.0], [1.28, 0.12], 0.0, 0.02), steel, 0.3);
            for k in 0..11 {
                let x = -1.15 + k as f32 * 0.23;
                let c = if k % 2 == 0 {
                    hex(0xf0c020)
                } else {
                    hex(0x1d1d20)
                };
                s.fill(&Shape::Box([x, 0.0], [0.08, 0.1], 0.0, 0.0), c);
            }
            s.fill(
                &Shape::Box([0.0, 0.0], [0.34, 0.11], 0.0, 0.02),
                hex(0x1f4fb0),
            );
            s.line([-0.24, 0.0], [0.24, 0.0], 1.0, hex(0xf2f4f8));
            s.outline(OUTLINE);
        }
        Thing::Barricade => {
            // A pile of planks, a pallet, tyres and a bin.
            let woods = [hex(0x8f6a42), hex(0x7a5634), hex(0xa27a4c), hex(0x6b4a2e)];
            s.fill(
                &Shape::Box([0.05, 0.0], [0.42, 2.3], 0.0, 0.12),
                hex(0x4a3a2c),
            );
            for k in 0..12 {
                let y = -2.1 + k as f32 * 0.38 + (rnd(k, 0, 321) - 0.5) * 0.2;
                let a = (rnd(k, 1, 321) - 0.5) * 1.2 + if k % 3 == 0 { 1.4 } else { 0.0 };
                let len = 0.4 + rnd(k, 2, 321) * 0.45;
                s.shaded(
                    &Shape::Box([(rnd(k, 3, 321) - 0.5) * 0.4, y], [len, 0.07], a, 0.0),
                    woods[(k % 4) as usize],
                    0.2,
                );
            }
            // Pallet.
            s.fill(
                &Shape::Box([0.0, 0.9], [0.45, 0.55], 0.15, 0.0),
                hex(0xb08a5a),
            );
            for k in 0..4 {
                let y = 0.55 + k as f32 * 0.23;
                s.line([-0.42, y], [0.42, y + 0.12], 1.0, hex(0x6b4a2e));
            }
            // Tyres.
            for p in [[0.1, -1.4], [-0.15, -0.9]] {
                s.fill(&Shape::Circle(p, 0.3), hex(0x1d1d20));
                s.fill(&Shape::Circle(p, 0.14), hex(0x4a4b50));
            }
            s.shaded(
                &Shape::Box([0.05, 1.85], [0.3, 0.3], 0.4, 0.04),
                hex(0x2f5a3a),
                0.4,
            );
            s.outline(OUTLINE);
        }
        Thing::Van(phase) | Thing::Ambulance(phase) => {
            let amb = matches!(t, Thing::Ambulance(_));
            let white = hex(0xeeeff0);
            let (hx, hy) = (2.6, 1.0);
            s.shaded(&Shape::Box([0.0, 0.0], [hx, hy], 0.0, 0.18), white, 0.6);
            // Cab roof and windscreen.
            s.fill(
                &Shape::Box([hx - 0.42, 0.0], [0.2, hy * 0.8], 0.0, 0.05),
                GLASS,
            );
            s.fill(
                &Shape::Box([hx - 0.1, 0.0], [0.08, hy * 0.86], 0.0, 0.0),
                tone(white, -0.12),
            );
            // Battenburg checks along both sides: blue/yellow police,
            // green/yellow ambulance.
            let (c1, c2) = if amb {
                (hex(0x2f8a3a), hex(0xf0d32a))
            } else {
                (hex(0x2742a8), hex(0xf0d32a))
            };
            for side in [-1.0f32, 1.0] {
                for k in 0..10 {
                    let x = -hx + 0.15 + k as f32 * 0.42;
                    if x > hx - 0.7 {
                        break;
                    }
                    let c = if k % 2 == 0 { c1 } else { c2 };
                    s.fill(
                        &Shape::Box([x + 0.21, side * (hy - 0.1)], [0.21, 0.1], 0.0, 0.0),
                        c,
                    );
                }
            }
            // Roof: number or cross, vents, rear doors.
            if amb {
                s.fill(
                    &Shape::Box([-0.5, 0.0], [0.45, 0.13], 0.0, 0.0),
                    hex(0xc8282a),
                );
                s.fill(
                    &Shape::Box([-0.5, 0.0], [0.13, 0.45], 0.0, 0.0),
                    hex(0xc8282a),
                );
            } else {
                s.fill(
                    &Shape::Box([-0.6, 0.0], [0.7, 0.32], 0.0, 0.0),
                    tone(white, -0.06),
                );
                for k in 0..3 {
                    s.fill(
                        &Shape::Box([-1.05 + k as f32 * 0.45, 0.0], [0.13, 0.2], 0.0, 0.0),
                        hex(0x26272c),
                    );
                }
            }
            s.fill(
                &Shape::Box([-1.6, 0.55], [0.25, 0.18], 0.0, 0.0),
                tone(white, -0.15),
            );
            s.line([-hx + 0.05, 0.0], [-hx + 0.3, 0.0], 1.0, tone(white, -0.35));
            lightbar(s, [hx - 0.75, 0.0], hy * 0.75, phase, amb);
            let _ = px;
            s.outline(OUTLINE);
        }
        Thing::Supply(kind) => supply(s, kind),
        Thing::WaterCannon(phase) => {
            let body = hex(0x3f4a5a);
            let (hx, hy) = (4.0, 1.25);
            s.shaded(&Shape::Box([0.0, 0.0], [hx, hy], 0.0, 0.2), body, 0.5);
            // Tank ribs.
            for k in 0..6 {
                let x = -3.4 + k as f32 * 0.85;
                s.fill(
                    &Shape::Box([x, 0.0], [0.06, hy - 0.12], 0.0, 0.0),
                    tone(body, -0.25),
                );
            }
            // Cab.
            s.shaded(
                &Shape::Box([hx - 0.85, 0.0], [0.8, hy - 0.05], 0.0, 0.12),
                tone(body, 0.12),
                0.5,
            );
            s.fill(
                &Shape::Box([hx - 0.25, 0.0], [0.15, hy * 0.8], 0.0, 0.03),
                GLASS,
            );
            // Turret and nozzle.
            s.shaded(&Shape::Circle([hx - 1.1, 0.0], 0.55), hex(0x2c3440), 0.9);
            s.line([hx - 1.1, 0.0], [hx + 0.05, 0.0], 3.0, hex(0x1d2229));
            s.fill(&Shape::Circle([hx - 1.1, 0.0], 0.18), hex(0x6a7480));
            for side in [-1.0, 1.0] {
                s.fill(
                    &Shape::Box([0.0, side * (hy - 0.08)], [hx - 1.0, 0.06], 0.0, 0.0),
                    hex(0xd8c22c),
                );
            }
            lightbar(s, [hx - 1.85, 0.0], hy * 0.8, phase, false);
            s.outline(OUTLINE);
        }
    }
}

/// A pallet of supplies.
fn supply(s: &mut Sheet, kind: u8) {
    let wood = hex(0xa98455);
    s.fill(
        &Shape::Box([0.0, 0.0], [0.62, 0.62], 0.0, 0.0),
        tone(wood, -0.2),
    );
    for k in 0..5 {
        let y = -0.5 + k as f32 * 0.25;
        s.fill(&Shape::Box([0.0, y], [0.6, 0.08], 0.0, 0.0), wood);
    }
    match kind {
        0 => {
            // Shrink-wrapped packs of bottles.
            for (i, j) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let c = [i * 0.27, j * 0.27];
                s.shaded(&Shape::Box(c, [0.24, 0.24], 0.0, 0.03), hex(0x9fc4e4), 0.4);
                for (a, b) in [(-0.1, -0.1), (0.1, -0.1), (-0.1, 0.1), (0.1, 0.1)] {
                    s.fill(&Shape::Circle([c[0] + a, c[1] + b], 0.05), hex(0x2f6fc0));
                }
            }
        }
        1 => {
            for (i, j) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let c = [i * 0.27, j * 0.27];
                s.shaded(&Shape::Box(c, [0.25, 0.25], 0.0, 0.02), hex(0xeeeee8), 0.3);
                s.fill(&Shape::Box(c, [0.13, 0.04], 0.0, 0.0), hex(0xc8282a));
                s.fill(&Shape::Box(c, [0.04, 0.13], 0.0, 0.0), hex(0xc8282a));
            }
        }
        2 => {
            for k in 0..4 {
                let x = -0.3 + k as f32 * 0.2;
                s.shaded(
                    &Shape::Box([x, 0.0], [0.09, 0.55], 0.0, 0.03),
                    hex(0xb0c4d6),
                    0.4,
                );
                s.fill(&Shape::Box([x, 0.0], [0.03, 0.28], 0.0, 0.0), hex(0x3a4250));
            }
        }
        _ => {
            for k in 0..5 {
                let y = -0.44 + k as f32 * 0.22;
                s.shaded(
                    &Shape::Box([0.0, y], [0.58, 0.09], 0.05 * (k as f32 - 2.0), 0.0),
                    [hex(0x8f6a42), hex(0xa27a4c), hex(0x7a5634)][k % 3],
                    0.2,
                );
            }
        }
    }
    s.outline(OUTLINE);
}

/// A light bar across a roof, flashing in two phases.
fn lightbar(s: &mut Sheet, at: P, half_w: f32, phase: u8, amb: bool) {
    s.fill(&Shape::Box(at, [0.12, half_w], 0.0, 0.04), hex(0x26272c));
    let (lit, dim, warm) = if amb {
        (hex(0x7fb4ff), hex(0x24407a), hex(0xff5a4a))
    } else {
        (hex(0x7fb4ff), hex(0x24407a), hex(0x8fc0ff))
    };
    for (k, side) in [-1.0f32, 1.0].into_iter().enumerate() {
        let on = phase != 0 && (phase as usize - 1) == k;
        let c = if on {
            if k == 1 { warm } else { lit }
        } else {
            dim
        };
        let p = [at[0], at[1] + side * half_w * 0.55];
        s.fill(&Shape::Box(p, [0.09, half_w * 0.38], 0.0, 0.02), c);
        if on {
            s.fill(
                &Shape::Box(p, [0.04, half_w * 0.2], 0.0, 0.0),
                mix(c, [255, 255, 255], 0.6),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlases_pack_and_paint_at_every_level() {
        let d = DecalAtlas::layout();
        let t = ThingAtlas::layout();
        for level in 0..art::LEVELS {
            let a = d.paint(level);
            assert_eq!(a.img.w, d.units[0] * UNIT / art::step(level));
            let b = t.paint(level);
            assert_eq!(b.img.h, t.units[1] * UNIT / art::step(level));
            for s in &t.sprites {
                // Every sprite paints something inside its cell.
                let px = UNIT / art::step(level);
                let [x, y, w, h] = s.rect;
                let mut n = 0;
                for yy in y * px..(y + h) * px {
                    for xx in x * px..(x + w) * px {
                        n += (b.img.get(xx, yy)[3] > 0) as u32;
                    }
                }
                assert!(n > 0, "{s:?} empty at {level}");
            }
        }
        // Cells are at least as big as what they show.
        for s in &t.sprites {
            let c = s.cell_m();
            assert!(c[0] >= s.body[0] && c[1] >= s.body[1]);
        }
        let car = t.get(Thing::Car(0));
        let h = car.quad_half([2.2, 0.9]);
        assert!((h[0] * 2.0 - car.cell_m()[0]).abs() < 1e-5);
    }

    /// `RIOT_SHEETS_PNG=/dir cargo test -p riot_sheets dump_sheets -- --ignored`
    #[test]
    #[ignore]
    fn dump_sheets() {
        let dir = std::env::var("RIOT_SHEETS_PNG").unwrap_or_else(|_| ".".into());
        for level in 0..art::LEVELS {
            for (name, img) in [
                ("decals", DecalAtlas::layout().paint(level).img),
                ("things", ThingAtlas::layout().paint(level).img),
            ] {
                let mut bytes = img.bytes();
                for p in bytes.as_chunks_mut::<4>().0 {
                    if p[3] == 0 {
                        p.copy_from_slice(&[96, 104, 96, 255]);
                    }
                }
                art::png::dump(&format!("{dir}/{name}-{level}.png"), img.w, img.h, &bytes);
            }
        }
    }
}
