//! Procedural sprite sheets and palettes (DESIGN §18.2).
//!
//! Every sheet is drawn once at startup into one atlas, seen from straight
//! above with the person facing +x (forward) and +y to their right. The
//! vertex shader rotates each sprite by `heading`, so a gait needs one strip
//! of frames, not 8 or 16 directions.
//!
//! The atlas holds no colours, only **palette weights**: R, G and B are the
//! weights of three palette colours (which three depends on the layer, e.g.
//! clothes, skin and trousers for the body), premultiplied by coverage in A.
//! The fragment shader mixes the colours picked by a person's appearance bits,
//! so a handful of sheets yields thousands of distinct-looking people, and
//! linear filtering stays correct because the mix is linear in the texel.

use std::f32::consts::TAU;

/// Pixels per atlas cell side.
pub const CELL: usize = 96;
/// Metres covered by a cell side; the person stands at the cell centre.
pub const CELL_M: f32 = 2.0;
/// Atlas cells per row.
pub const COLS: usize = 16;
/// Atlas rows of cells.
pub const ROWS: usize = 5;
/// Atlas width in pixels.
pub const ATLAS_W: usize = CELL * COLS;
/// Atlas height in pixels.
pub const ATLAS_H: usize = CELL * ROWS;

/// Atlas frame indices. The shader mirrors these numbers.
pub mod frame {
    /// Body layer: one strip per gait.
    pub const STAND: u32 = 0;
    /// 8 frames, indexed by `stride / 32`.
    pub const WALK: u32 = 1;
    /// 8 frames, indexed by `stride / 32`.
    pub const RUN: u32 = 9;
    /// Crouching (barricading, helping).
    pub const CROUCH: u32 = 17;
    /// Hands behind the back.
    pub const RESTRAINED: u32 = 18;
    /// 4 frames on the render clock (cosmetic).
    pub const THROW: u32 = 19;
    /// 4 frames on the render clock (cosmetic).
    pub const STRIKE: u32 = 23;
    /// Both hands up, holding a camera.
    pub const FILM: u32 = 27;
    /// Body accessories: backpack, scarf, hi-vis vest, press vest.
    pub const ACCESSORY: u32 = 32;
    /// 16 hair styles.
    pub const HEAD: u32 = 36;
    /// Cap, helmet, balaclava.
    pub const HEADWEAR: u32 = 52;
    /// Held items by [`riot_sim::state::Item`] code (0 is unused).
    pub const ITEM: u32 = 56;
    /// Lying on the ground: fallen or carried. Body and head in one sheet.
    pub const LYING: u32 = 67;
    /// Middle level of detail: one sheet for the whole person.
    pub const MID: u32 = 68;
    /// Middle level of detail with a shield in front.
    pub const MID_SHIELD: u32 = 69;
    /// Frames in use (some slots are empty).
    pub const COUNT: u32 = 70;
}

/// Palette texture size.
pub const PAL_W: usize = 64;
/// Palette texture height (rows).
pub const PAL_H: usize = 16;

/// Palette rows. The shader mirrors these numbers.
pub mod pal_row {
    /// Shirts and jackets.
    pub const CLOTHES_TOP: usize = 0;
    /// Trousers and shoes.
    pub const CLOTHES_BOTTOM: usize = 1;
    /// Skin tones.
    pub const SKIN: usize = 2;
    /// Hair colours.
    pub const HAIR: usize = 3;
    /// Accessory main colours.
    pub const ACC_A: usize = 4;
    /// Accessory trim (stripes, bands, visors).
    pub const ACC_B: usize = 5;
    /// Item main colours.
    pub const ITEM_A: usize = 6;
    /// Item second colours.
    pub const ITEM_B: usize = 7;
    /// Item detail colours.
    pub const ITEM_C: usize = 8;
    /// Bright varied colours for caps, backpacks, scarves and flags.
    pub const VARIED: usize = 9;
}

/// The generated sheets: premultiplied RGBA8, `ATLAS_W × ATLAS_H`.
pub struct Atlas {
    pub rgba: Vec<u8>,
}

/// The generated palettes: RGBA8, `PAL_W × PAL_H`.
pub struct Palette {
    pub rgba: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Shapes

type P = [f32; 2];

/// A signed-distance shape in sprite metres (negative inside).
#[derive(Clone, Debug)]
enum Shape {
    Circle(P, f32),
    /// Centre, radii (along x, along y).
    Ellipse(P, P),
    /// Segment end points and radius.
    Capsule(P, P, f32),
    /// Centre, half extents, rotation, corner radius.
    Box(P, P, f32, f32),
    /// Convex polygon, counter-clockwise or clockwise.
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
    fn eval(&self, p: P) -> (f32, f32) {
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
                // Convex: the largest half-plane distance (exact inside,
                // a slight overestimate near outer corners, fine for sprites).
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
    fn bounds(&self) -> (P, P) {
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
}

/// Which palette weight a shape writes.
#[derive(Clone, Copy, Debug)]
enum Ink {
    /// Palette colour A (R channel).
    A,
    /// Palette colour B (G channel).
    B,
    /// Palette colour C (B channel).
    C,
    /// Black.
    Black,
}

#[derive(Clone, Copy, Debug)]
struct Style {
    /// Brightness at the shape's centre.
    shade: f32,
    /// How much darker the rim is (0 = flat).
    bulge: f32,
    opacity: f32,
    outline: bool,
}

const SOLID: Style = Style {
    shade: 1.0,
    bulge: 0.22,
    opacity: 1.0,
    outline: true,
};

impl Style {
    fn shade(self, s: f32) -> Style {
        Style { shade: s, ..self }
    }
    fn flat(self) -> Style {
        Style { bulge: 0.0, ..self }
    }
    fn opacity(self, o: f32) -> Style {
        Style { opacity: o, ..self }
    }
    fn no_outline(self) -> Style {
        Style {
            outline: false,
            ..self
        }
    }
}

/// Outline width in metres (about one atlas pixel).
const OUTLINE: f32 = 0.022;
const PX_PER_M: f32 = CELL as f32 / CELL_M;

/// The atlas being painted, premultiplied f32 RGBA.
struct Canvas {
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new() -> Canvas {
        Canvas {
            px: vec![[0.0; 4]; ATLAS_W * ATLAS_H],
        }
    }

    fn draw(&mut self, f: u32, shape: &Shape, ink: Ink, style: Style) {
        if style.outline {
            self.fill(f, shape, OUTLINE, Ink::Black, 0.0, 0.0, style.opacity);
        }
        self.fill(f, shape, 0.0, ink, style.shade, style.bulge, style.opacity);
    }

    fn fill(
        &mut self,
        f: u32,
        shape: &Shape,
        grow: f32,
        ink: Ink,
        shade: f32,
        bulge: f32,
        opacity: f32,
    ) {
        let (ox, oy) = ((f as usize % COLS) * CELL, (f as usize / COLS) * CELL);
        let (lo, hi) = shape.bounds();
        let to_px = |m: f32| (m + CELL_M * 0.5) * PX_PER_M;
        let x0 = (to_px(lo[0] - grow) - 2.0).floor().max(0.0) as usize;
        let y0 = (to_px(lo[1] - grow) - 2.0).floor().max(0.0) as usize;
        let x1 = ((to_px(hi[0] + grow) + 2.0).ceil() as usize).min(CELL);
        let y1 = ((to_px(hi[1] + grow) + 2.0).ceil() as usize).min(CELL);
        for py in y0..y1 {
            for px in x0..x1 {
                let p = [
                    (px as f32 + 0.5) / PX_PER_M - CELL_M * 0.5,
                    (py as f32 + 0.5) / PX_PER_M - CELL_M * 0.5,
                ];
                let (d, r) = shape.eval(p);
                let cov = ((0.5 - (d - grow) * PX_PER_M).clamp(0.0, 1.0)) * opacity;
                if cov <= 0.0 {
                    continue;
                }
                let s = (shade * (1.0 - bulge * r * r)).clamp(0.0, 1.0);
                let w = match ink {
                    Ink::A => [s, 0.0, 0.0],
                    Ink::B => [0.0, s, 0.0],
                    Ink::C => [0.0, 0.0, s],
                    Ink::Black => [0.0, 0.0, 0.0],
                };
                let dst = &mut self.px[(oy + py) * ATLAS_W + ox + px];
                let keep = 1.0 - cov;
                for c in 0..3 {
                    dst[c] = w[c] * cov + dst[c] * keep;
                }
                dst[3] = cov + dst[3] * keep;
            }
        }
    }

    fn into_rgba8(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in self.px {
            for c in p {
                out.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Bodies

/// Where the limbs are in one body frame (sprite metres).
#[derive(Clone, Copy, Debug)]
struct Pose {
    feet: [P; 2],
    hands: [P; 2],
    torso: P,
    /// Torso radii: front-back, half the shoulder width.
    torso_r: P,
    /// Crouching: knees forward.
    knees: bool,
}

const SHOULDER_Y: f32 = 0.19;

impl Pose {
    fn standing() -> Pose {
        Pose {
            feet: [[0.06, -0.1], [0.06, 0.1]],
            hands: [[0.01, -0.285], [0.01, 0.285]],
            torso: [-0.01, 0.0],
            torso_r: [0.12, 0.24],
            knees: false,
        }
    }
}

fn draw_body(cv: &mut Canvas, f: u32, pose: &Pose) {
    // Feet first, so the torso covers them unless they step out.
    for foot in pose.feet {
        cv.draw(
            f,
            &Shape::Ellipse(foot, [0.08, 0.045]),
            Ink::C,
            SOLID.shade(0.55),
        );
    }
    if pose.knees {
        for s in [-1.0, 1.0] {
            cv.draw(
                f,
                &Shape::Capsule([0.0, 0.09 * s], [0.24, 0.11 * s], 0.06),
                Ink::C,
                SOLID.shade(0.85),
            );
        }
    }
    let t = pose.torso;
    cv.draw(f, &Shape::Ellipse(t, pose.torso_r), Ink::A, SOLID);
    // Sleeves from the shoulders, then hands.
    for (k, s) in [-1.0f32, 1.0].into_iter().enumerate() {
        let shoulder = [t[0] + 0.0, SHOULDER_Y * s * pose.torso_r[1] / 0.24];
        let hand = pose.hands[k];
        let elbow = [
            shoulder[0] + (hand[0] - shoulder[0]) * 0.75,
            shoulder[1] + (hand[1] - shoulder[1]) * 0.75,
        ];
        cv.draw(
            f,
            &Shape::Capsule(shoulder, elbow, 0.052),
            Ink::A,
            SOLID.shade(0.82),
        );
        cv.draw(f, &Shape::Circle(hand, 0.043), Ink::B, SOLID.shade(0.95));
    }
    // A collar shadow where the head sits.
    cv.draw(
        f,
        &Shape::Circle([t[0] + 0.03, 0.0], 0.08),
        Ink::A,
        SOLID.shade(0.62).no_outline().flat().opacity(0.6),
    );
}

fn body_frames(cv: &mut Canvas) {
    draw_body(cv, frame::STAND, &Pose::standing());
    for k in 0..8 {
        let ph = (k as f32 / 8.0) * TAU;
        let (s, _) = ph.sin_cos();
        let mut p = Pose::standing();
        p.feet = [[0.02 + 0.2 * s, -0.1], [0.02 - 0.2 * s, 0.1]];
        p.hands = [[0.01 - 0.12 * s, -0.28], [0.01 + 0.12 * s, 0.28]];
        draw_body(cv, frame::WALK + k, &p);

        let mut r = Pose::standing();
        r.torso = [0.03, 0.0];
        r.feet = [[0.04 + 0.32 * s, -0.09], [0.04 - 0.32 * s, 0.09]];
        r.hands = [[0.08 - 0.17 * s, -0.24], [0.08 + 0.17 * s, 0.24]];
        draw_body(cv, frame::RUN + k, &r);
    }
    let mut c = Pose::standing();
    c.torso = [0.05, 0.0];
    c.torso_r = [0.13, 0.22];
    c.knees = true;
    c.feet = [[0.12, -0.12], [0.12, 0.12]];
    c.hands = [[0.24, -0.16], [0.24, 0.16]];
    draw_body(cv, frame::CROUCH, &c);

    let mut r = Pose::standing();
    r.hands = [[-0.17, -0.05], [-0.17, 0.05]];
    draw_body(cv, frame::RESTRAINED, &r);

    // Throw: the right hand comes over from behind.
    let throw = [[-0.32, 0.22], [-0.18, 0.3], [0.12, 0.3], [0.38, 0.18]];
    for (k, h) in throw.into_iter().enumerate() {
        let mut p = Pose::standing();
        p.feet = [[0.16, -0.1], [-0.12, 0.1]];
        p.hands = [[0.18, -0.24], h];
        draw_body(cv, frame::THROW + k as u32, &p);
    }
    // Strike: the right arm swings from the side to the front.
    let strike = [[0.05, 0.42], [0.25, 0.38], [0.4, 0.18], [0.36, 0.02]];
    for (k, h) in strike.into_iter().enumerate() {
        let mut p = Pose::standing();
        p.feet = [[0.12, -0.1], [-0.06, 0.1]];
        p.hands = [[0.08, -0.27], h];
        draw_body(cv, frame::STRIKE + k as u32, &p);
    }
    let mut film = Pose::standing();
    film.hands = [[0.2, -0.08], [0.2, 0.08]];
    draw_body(cv, frame::FILM, &film);
}

// ---------------------------------------------------------------------------
// Accessories, heads and items

fn accessory_frames(cv: &mut Canvas) {
    // Backpack.
    let f = frame::ACCESSORY;
    for s in [-1.0, 1.0] {
        cv.draw(
            f,
            &Shape::Capsule([-0.09, 0.13 * s], [0.07, 0.13 * s], 0.018),
            Ink::A,
            SOLID.shade(0.65),
        );
    }
    cv.draw(
        f,
        &Shape::Box([-0.16, 0.0], [0.075, 0.15], 0.0, 0.035),
        Ink::A,
        SOLID,
    );
    cv.draw(
        f,
        &Shape::Box([-0.2, 0.0], [0.02, 0.1], 0.0, 0.01),
        Ink::A,
        SOLID.shade(0.7).flat(),
    );
    // Scarf.
    let f = frame::ACCESSORY + 1;
    cv.draw(
        f,
        &Shape::Capsule([0.0, 0.08], [-0.2, 0.15], 0.032),
        Ink::A,
        SOLID.shade(0.8),
    );
    cv.draw(
        f,
        &Shape::Capsule([0.05, -0.11], [0.05, 0.11], 0.04),
        Ink::A,
        SOLID,
    );
    // Hi-vis vest with reflective stripes.
    let f = frame::ACCESSORY + 2;
    cv.draw(
        f,
        &Shape::Ellipse([-0.01, 0.0], [0.115, 0.215]),
        Ink::A,
        SOLID,
    );
    for s in [-1.0, 1.0] {
        cv.draw(
            f,
            &Shape::Capsule([-0.1, 0.125 * s], [0.09, 0.125 * s], 0.017),
            Ink::B,
            SOLID.no_outline().flat(),
        );
    }
    // Press vest with a white band across the back.
    let f = frame::ACCESSORY + 3;
    cv.draw(
        f,
        &Shape::Ellipse([-0.01, 0.0], [0.115, 0.215]),
        Ink::A,
        SOLID,
    );
    cv.draw(
        f,
        &Shape::Box([-0.065, 0.0], [0.035, 0.17], 0.0, 0.01),
        Ink::B,
        SOLID.no_outline().flat(),
    );
}

const HEAD_C: P = [0.03, 0.0];
const HEAD_R: f32 = 0.105;

fn head_base(cv: &mut Canvas, f: u32) {
    for s in [-1.0, 1.0] {
        cv.draw(
            f,
            &Shape::Circle([0.035, 0.1 * s], 0.026),
            Ink::B,
            SOLID.shade(0.85),
        );
    }
    cv.draw(f, &Shape::Circle(HEAD_C, HEAD_R), Ink::B, SOLID);
    cv.draw(
        f,
        &Shape::Ellipse([0.135, 0.0], [0.028, 0.022]),
        Ink::B,
        SOLID.shade(0.9),
    );
}

/// The hair "cap" covering the top of the head, leaving the forehead.
fn hair_cap(cv: &mut Canvas, f: u32, shift: f32, r: f32, style: Style) {
    cv.draw(
        f,
        &Shape::Circle([HEAD_C[0] - shift, 0.0], r),
        Ink::A,
        style,
    );
}

fn head_frames(cv: &mut Canvas) {
    for style in 0..16u32 {
        let f = frame::HEAD + style;
        // Hair that falls behind and around the head is drawn first.
        match style {
            3 => cv.draw(
                f,
                &Shape::Ellipse([-0.06, 0.0], [0.15, 0.13]),
                Ink::A,
                SOLID.shade(0.85),
            ),
            12 => cv.draw(
                f,
                &Shape::Ellipse([-0.01, 0.0], [0.125, 0.13]),
                Ink::A,
                SOLID.shade(0.9),
            ),
            _ => {}
        }
        head_base(cv, f);
        match style {
            0 => hair_cap(cv, f, 0.018, 0.1, SOLID),
            1 => hair_cap(cv, f, 0.012, 0.1, SOLID.opacity(0.55).no_outline()),
            2 => cv.draw(
                f,
                &Shape::Circle([0.0, -0.03], 0.035),
                Ink::B,
                SOLID.shade(1.0).no_outline().flat().opacity(0.4),
            ),
            3 | 12 => hair_cap(cv, f, 0.012, 0.102, SOLID),
            4 => {
                hair_cap(cv, f, 0.018, 0.1, SOLID);
                cv.draw(
                    f,
                    &Shape::Capsule([-0.06, 0.0], [-0.21, 0.02], 0.035),
                    Ink::A,
                    SOLID.shade(0.85),
                );
            }
            5 => {
                hair_cap(cv, f, 0.018, 0.1, SOLID);
                cv.draw(
                    f,
                    &Shape::Circle([-0.075, 0.0], 0.052),
                    Ink::A,
                    SOLID.shade(0.9),
                );
            }
            6 => {
                for k in 0..10 {
                    let a = k as f32 / 10.0 * TAU;
                    cv.draw(
                        f,
                        &Shape::Circle([0.005 + 0.1 * a.cos(), 0.1 * a.sin()], 0.06),
                        Ink::A,
                        SOLID.shade(0.85),
                    );
                }
                hair_cap(cv, f, 0.025, 0.115, SOLID.no_outline());
            }
            7 => cv.draw(
                f,
                &Shape::Capsule([0.11, 0.0], [-0.09, 0.0], 0.032),
                Ink::A,
                SOLID,
            ),
            8 => {
                hair_cap(cv, f, 0.016, 0.101, SOLID);
                cv.draw(
                    f,
                    &Shape::Capsule([0.09, -0.035], [-0.05, -0.035], 0.008),
                    Ink::B,
                    SOLID.no_outline().flat().opacity(0.8),
                );
            }
            9 => {
                hair_cap(cv, f, 0.02, 0.098, SOLID);
                for k in 0..8 {
                    let a = k as f32 / 8.0 * TAU + 0.3;
                    cv.draw(
                        f,
                        &Shape::Circle([0.01 + 0.07 * a.cos(), 0.07 * a.sin()], 0.038),
                        Ink::A,
                        SOLID.shade(0.95).bulge_less(),
                    );
                }
            }
            10 => {
                hair_cap(cv, f, 0.016, 0.1, SOLID);
                for s in [-1.0, 1.0] {
                    cv.draw(
                        f,
                        &Shape::Capsule([-0.06, 0.05 * s], [-0.24, 0.08 * s], 0.022),
                        Ink::A,
                        SOLID.shade(0.85),
                    );
                }
            }
            11 => hair_cap(cv, f, 0.005, 0.104, SOLID),
            13 => {
                hair_cap(cv, f, -0.005, 0.12, SOLID);
                cv.draw(
                    f,
                    &Shape::Ellipse([0.11, 0.0], [0.04, 0.06]),
                    Ink::B,
                    SOLID.shade(0.95).no_outline(),
                );
            }
            14 => {
                for k in 0..9 {
                    let a = (k as f32 / 8.0 - 0.5) * 2.6 + std::f32::consts::PI;
                    let d = [a.cos(), a.sin()];
                    cv.draw(
                        f,
                        &Shape::Capsule(
                            [0.0 + 0.05 * d[0], 0.05 * d[1]],
                            [0.0 + 0.2 * d[0], 0.2 * d[1]],
                            0.022,
                        ),
                        Ink::A,
                        SOLID.shade(0.85),
                    );
                }
                hair_cap(cv, f, 0.018, 0.1, SOLID);
            }
            _ => {
                // 15: receding.
                hair_cap(cv, f, 0.03, 0.095, SOLID);
                cv.draw(
                    f,
                    &Shape::Circle([0.06, 0.0], 0.07),
                    Ink::B,
                    SOLID.no_outline(),
                );
            }
        }
    }
    // Cap with a brim.
    let f = frame::HEADWEAR;
    head_base(cv, f);
    hair_cap(cv, f, 0.03, 0.1, SOLID.shade(0.9));
    cv.draw(
        f,
        &Shape::Ellipse([0.13, 0.0], [0.065, 0.085]),
        Ink::C,
        SOLID.shade(0.7),
    );
    cv.draw(f, &Shape::Circle([0.035, 0.0], 0.101), Ink::C, SOLID);
    // Riot helmet with a visor.
    let f = frame::HEADWEAR + 1;
    cv.draw(f, &Shape::Circle([0.02, 0.0], 0.135), Ink::C, SOLID);
    cv.draw(
        f,
        &Shape::Capsule([-0.1, 0.0], [0.08, 0.0], 0.014),
        Ink::C,
        SOLID.shade(0.6).no_outline(),
    );
    cv.draw(
        f,
        &Shape::Ellipse([0.125, 0.0], [0.05, 0.115]),
        Ink::Black,
        SOLID.opacity(0.8).no_outline(),
    );
    // Balaclava with an eye slit.
    let f = frame::HEADWEAR + 2;
    head_base(cv, f);
    cv.draw(f, &Shape::Circle(HEAD_C, 0.11), Ink::C, SOLID);
    cv.draw(
        f,
        &Shape::Box([0.115, 0.0], [0.014, 0.05], 0.0, 0.01),
        Ink::B,
        SOLID.no_outline().flat(),
    );
}

impl Style {
    fn bulge_less(self) -> Style {
        Style {
            bulge: self.bulge * 0.5,
            ..self
        }
    }
}

/// Where a held item sits: in the right hand at rest.
pub const REST_HAND: [f32; 2] = [0.06, 0.28];

fn item_frames(cv: &mut Canvas) {
    let f = |item: u32| frame::ITEM + item;
    // 1 Sign: a placard held up in front.
    cv.draw(
        f(1),
        &Shape::Box([0.13, 0.0], [0.035, 0.3], 0.0, 0.01),
        Ink::A,
        SOLID.bulge_less(),
    );
    for k in 0..3 {
        let y = -0.18 + k as f32 * 0.18;
        cv.draw(
            f(1),
            &Shape::Box([0.13, y], [0.014, 0.065], 0.0, 0.005),
            Ink::C,
            SOLID.no_outline().flat(),
        );
    }
    // 2 Shield: a riot shield in front of the body.
    cv.draw(
        f(2),
        &Shape::Box([0.31, 0.0], [0.04, 0.32], 0.0, 0.025),
        Ink::A,
        SOLID.opacity(0.85).bulge_less(),
    );
    cv.draw(
        f(2),
        &Shape::Box([0.31, 0.0], [0.018, 0.16], 0.0, 0.006),
        Ink::C,
        SOLID.no_outline().flat().opacity(0.9),
    );
    // 3 Bottle.
    bottle(cv, f(3), 0.028);
    // 4 Camera held in front of the face.
    cv.draw(
        f(4),
        &Shape::Box([0.23, 0.0], [0.06, 0.085], 0.0, 0.015),
        Ink::A,
        SOLID,
    );
    cv.draw(f(4), &Shape::Circle([0.3, 0.0], 0.036), Ink::B, SOLID);
    cv.draw(
        f(4),
        &Shape::Box([0.21, 0.0], [0.04, 0.016], 0.0, 0.005),
        Ink::C,
        SOLID.no_outline().flat(),
    );
    // 5 Flag: pole in the hand, cloth trailing behind.
    cv.draw(
        f(5),
        &Shape::Poly(vec![
            [0.06, 0.25],
            [-0.5, 0.21],
            [-0.52, 0.36],
            [0.06, 0.33],
        ]),
        Ink::A,
        SOLID.bulge_less(),
    );
    cv.draw(
        f(5),
        &Shape::Poly(vec![
            [0.05, 0.28],
            [-0.5, 0.265],
            [-0.505, 0.305],
            [0.05, 0.3],
        ]),
        Ink::C,
        SOLID.no_outline().flat(),
    );
    cv.draw(
        f(5),
        &Shape::Circle(REST_HAND, 0.022),
        Ink::B,
        SOLID.shade(0.8),
    );
    // 6 Baton.
    cv.draw(
        f(6),
        &Shape::Capsule(REST_HAND, [0.43, 0.21], 0.019),
        Ink::A,
        SOLID,
    );
    cv.draw(
        f(6),
        &Shape::Capsule([0.03, 0.285], [0.1, 0.27], 0.023),
        Ink::B,
        SOLID,
    );
    // 7 Megaphone: a cone in front of the face.
    cv.draw(
        f(7),
        &Shape::Poly(vec![
            [0.14, -0.03],
            [0.34, -0.075],
            [0.34, 0.075],
            [0.14, 0.03],
        ]),
        Ink::A,
        SOLID,
    );
    cv.draw(
        f(7),
        &Shape::Box([0.34, 0.0], [0.014, 0.078], 0.0, 0.006),
        Ink::B,
        SOLID.no_outline(),
    );
    // 8 Molotov: a bottle with a burning rag.
    bottle(cv, f(8), 0.028);
    cv.draw(
        f(8),
        &Shape::Circle([0.25, 0.29], 0.034),
        Ink::B,
        SOLID.no_outline(),
    );
    cv.draw(
        f(8),
        &Shape::Circle([0.255, 0.29], 0.017),
        Ink::C,
        SOLID.no_outline().flat(),
    );
    // 9 Water bottle.
    bottle(cv, f(9), 0.031);
    // 10 Plank carried on the shoulder.
    cv.draw(
        f(10),
        &Shape::Box([-0.02, 0.24], [0.56, 0.045], 0.12, 0.01),
        Ink::A,
        SOLID.bulge_less(),
    );
    cv.draw(
        f(10),
        &Shape::Capsule([-0.4, 0.195], [0.3, 0.28], 0.006),
        Ink::B,
        SOLID.no_outline().flat(),
    );
}

fn bottle(cv: &mut Canvas, f: u32, r: f32) {
    cv.draw(
        f,
        &Shape::Capsule([0.08, 0.29], [0.21, 0.29], r),
        Ink::A,
        SOLID.opacity(0.92),
    );
    cv.draw(
        f,
        &Shape::Circle([0.225, 0.29], 0.015),
        Ink::B,
        SOLID.no_outline(),
    );
}

fn lying_frame(cv: &mut Canvas) {
    let f = frame::LYING;
    // On their back, head towards +x, arms and legs spread a little.
    for s in [-1.0, 1.0] {
        cv.draw(
            f,
            &Shape::Capsule([0.12, 0.09 * s], [-0.7, 0.14 * s], 0.075),
            Ink::A,
            SOLID.shade(0.55),
        );
        cv.draw(
            f,
            &Shape::Ellipse([-0.76, 0.15 * s], [0.05, 0.06]),
            Ink::A,
            SOLID.shade(0.3),
        );
        cv.draw(
            f,
            &Shape::Capsule([0.5, 0.2 * s], [0.22, 0.42 * s], 0.05),
            Ink::A,
            SOLID.shade(0.85),
        );
        cv.draw(f, &Shape::Circle([0.2, 0.44 * s], 0.044), Ink::B, SOLID);
    }
    cv.draw(f, &Shape::Ellipse([0.33, 0.0], [0.25, 0.22]), Ink::A, SOLID);
    cv.draw(f, &Shape::Circle([0.82, 0.0], 0.095), Ink::C, SOLID);
    cv.draw(f, &Shape::Circle([0.73, 0.0], 0.1), Ink::B, SOLID);
}

fn mid_frames(cv: &mut Canvas) {
    // Seen from afar: no outlines (they would darken the whole sprite once
    // minified), clothes dominate, a small head.
    for f in [frame::MID, frame::MID_SHIELD] {
        cv.draw(
            f,
            &Shape::Ellipse([-0.01, 0.0], [0.15, 0.27]),
            Ink::A,
            SOLID.no_outline().bulge_less(),
        );
        cv.draw(
            f,
            &Shape::Circle([0.03, 0.0], 0.095),
            Ink::C,
            SOLID.no_outline().bulge_less(),
        );
    }
    cv.draw(
        frame::MID_SHIELD,
        &Shape::Box([0.33, 0.0], [0.045, 0.33], 0.0, 0.02),
        Ink::Black,
        SOLID.opacity(0.9),
    );
}

/// Paint every sheet.
pub fn build_atlas() -> Atlas {
    let mut cv = Canvas::new();
    body_frames(&mut cv);
    accessory_frames(&mut cv);
    head_frames(&mut cv);
    item_frames(&mut cv);
    lying_frame(&mut cv);
    mid_frames(&mut cv);
    Atlas {
        rgba: cv.into_rgba8(),
    }
}

// ---------------------------------------------------------------------------
// Palettes

type Rgb = [u8; 3];

fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = (h.rem_euclid(360.0)) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |f: f32| ((f + m) * 255.0 + 0.5) as u8;
    [q(r), q(g), q(b)]
}

const SKIN: [Rgb; 8] = [
    [250, 222, 196],
    [238, 196, 160],
    [222, 172, 130],
    [196, 140, 100],
    [164, 110, 74],
    [128, 84, 56],
    [98, 62, 42],
    [70, 44, 32],
];

const HAIR: [Rgb; 8] = [
    [28, 24, 24],
    [62, 42, 30],
    [104, 70, 44],
    [146, 66, 34],
    [214, 178, 108],
    [150, 150, 150],
    [222, 222, 214],
    [204, 84, 150],
];

const TROUSERS: [Rgb; 8] = [
    [62, 82, 122],
    [40, 48, 72],
    [34, 34, 38],
    [150, 134, 100],
    [100, 100, 106],
    [92, 72, 56],
    [104, 128, 162],
    [84, 90, 60],
];

/// Clothes top and bottom for each of the 64 clothes palettes. Ranges follow
/// `riot_sim::world::appearance_for`: 0–43 crowd, 44–51 radicals, 52–55
/// organizers, 56–59 journalists, 60–61 officers.
fn clothes(i: usize) -> (Rgb, Rgb) {
    match i {
        0..=43 => {
            let top = if i % 9 == 4 {
                [[236, 236, 230], [52, 52, 56], [128, 128, 132]][i / 9 % 3]
            } else {
                let h = i as f32 * 137.5 + 10.0;
                let s = [0.35, 0.55, 0.7, 0.8][i % 4];
                let v = [0.92, 0.72, 0.84, 0.6][(i / 4) % 4];
                hsv(h, s, v)
            };
            (top, TROUSERS[(i * 5 + 3) % 8])
        }
        44..=51 => {
            let tops = [
                [26, 26, 28],
                [36, 36, 40],
                [20, 20, 22],
                [48, 50, 44],
                [30, 32, 44],
                [24, 24, 24],
                [56, 30, 30],
                [40, 40, 40],
            ];
            (tops[i - 44], [24, 24, 26])
        }
        52..=55 => {
            let tops = [
                [200, 44, 44],
                [228, 120, 32],
                [124, 64, 164],
                [30, 140, 140],
            ];
            (tops[i - 52], TROUSERS[0])
        }
        56..=59 => {
            let tops = [
                [172, 156, 120],
                [150, 180, 210],
                [204, 194, 172],
                [110, 116, 80],
            ];
            (tops[i - 56], TROUSERS[2])
        }
        60 => ([34, 44, 72], [26, 30, 46]),
        61 => ([30, 32, 40], [24, 26, 34]),
        _ => ([120, 120, 120], [60, 60, 60]),
    }
}

/// Accessory colours A and B by accessory code (cap, backpack and scarf take
/// varied colours in the shader instead).
const ACC: [(Rgb, Rgb); 8] = [
    ([128, 128, 128], [200, 200, 200]),
    ([180, 40, 40], [120, 30, 30]),
    ([40, 48, 72], [70, 80, 100]),
    ([24, 24, 26], [60, 60, 60]),
    ([60, 90, 140], [40, 40, 40]),
    ([170, 40, 50], [220, 220, 220]),
    ([212, 232, 40], [222, 222, 214]),
    ([36, 92, 182], [244, 244, 244]),
];

/// Item colours A, B and C by item code.
const ITEMS: [(Rgb, Rgb, Rgb); 11] = [
    ([0, 0, 0], [0, 0, 0], [0, 0, 0]),
    ([240, 236, 222], [124, 88, 52], [190, 30, 34]),
    ([176, 196, 214], [34, 34, 40], [242, 242, 242]),
    ([64, 136, 70], [208, 208, 208], [180, 150, 60]),
    ([34, 34, 36], [92, 116, 150], [128, 128, 128]),
    ([200, 40, 40], [160, 160, 160], [244, 244, 244]),
    ([26, 26, 28], [70, 70, 70], [0, 0, 0]),
    ([238, 238, 238], [204, 40, 40], [120, 120, 120]),
    ([96, 146, 74], [252, 140, 30], [255, 226, 90]),
    ([120, 180, 236], [40, 92, 204], [0, 0, 0]),
    ([176, 128, 72], [118, 84, 46], [0, 0, 0]),
];

/// Build the palette texture.
pub fn build_palette() -> Palette {
    let mut rgba = vec![0u8; PAL_W * PAL_H * 4];
    let mut put = |row: usize, col: usize, c: Rgb| {
        let o = (row * PAL_W + col) * 4;
        rgba[o..o + 3].copy_from_slice(&c);
        rgba[o + 3] = 255;
    };
    for i in 0..64 {
        let (top, bottom) = clothes(i);
        put(pal_row::CLOTHES_TOP, i, top);
        put(pal_row::CLOTHES_BOTTOM, i, bottom);
        put(
            pal_row::VARIED,
            i,
            hsv(i as f32 * 97.0 + 20.0, 0.55, [0.8, 0.6][i % 2]),
        );
    }
    for i in 0..8 {
        put(pal_row::SKIN, i, SKIN[i]);
        put(pal_row::HAIR, i, HAIR[i]);
        put(pal_row::ACC_A, i, ACC[i].0);
        put(pal_row::ACC_B, i, ACC[i].1);
    }
    for (i, &(a, b, c)) in ITEMS.iter().enumerate() {
        put(pal_row::ITEM_A, i, a);
        put(pal_row::ITEM_B, i, b);
        put(pal_row::ITEM_C, i, c);
    }
    Palette { rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(atlas: &Atlas, f: u32) -> u32 {
        let (ox, oy) = ((f as usize % COLS) * CELL, (f as usize / COLS) * CELL);
        let mut n = 0;
        for y in 0..CELL {
            for x in 0..CELL {
                n += (atlas.rgba[((oy + y) * ATLAS_W + ox + x) * 4 + 3] > 128) as u32;
            }
        }
        n
    }

    #[test]
    fn every_frame_is_painted_and_stays_inside_its_cell() {
        assert!(frame::COUNT as usize <= COLS * ROWS);
        let atlas = build_atlas();
        assert_eq!(atlas.rgba.len(), ATLAS_W * ATLAS_H * 4);
        for f in 0..frame::COUNT {
            if f == frame::ITEM || f == frame::HEADWEAR + 3 || (28..32).contains(&f) {
                continue; // unused slots
            }
            assert!(coverage(&atlas, f) > 30, "frame {f} is empty");
            // A 4-pixel border stays clear, so filtering never bleeds.
            let (ox, oy) = ((f as usize % COLS) * CELL, (f as usize / COLS) * CELL);
            for k in 0..CELL {
                for (x, y) in [(k, 0), (k, CELL - 1), (0, k), (CELL - 1, k)] {
                    let a = atlas.rgba[((oy + y) * ATLAS_W + ox + x) * 4 + 3];
                    assert_eq!(a, 0, "frame {f} touches its border");
                }
            }
        }
    }

    #[test]
    fn weights_are_premultiplied() {
        let atlas = build_atlas();
        for px in atlas.rgba.chunks_exact(4) {
            assert!(px[0].max(px[1]).max(px[2]) as u32 <= px[3] as u32 + 1);
        }
    }

    /// `RIOT_ATLAS_PNG=/path/atlas.png cargo test -p riot_render dump_atlas -- --ignored`
    /// writes the atlas (weights shown as R/G/B over grey) for inspection.
    #[test]
    #[ignore]
    fn dump_atlas() {
        let path = std::env::var("RIOT_ATLAS_PNG").unwrap_or_else(|_| "atlas.png".into());
        let atlas = build_atlas();
        let mut bytes = atlas.rgba.clone();
        for px in bytes.chunks_exact_mut(4) {
            let k = 255 - px[3] as u32;
            for c in 0..3 {
                px[c] = (px[c] as u32 + 90 * k / 255) as u8;
            }
            px[3] = 255;
        }
        macroquad::texture::Image {
            bytes,
            width: ATLAS_W as u16,
            height: ATLAS_H as u16,
        }
        .export_png(&path);
    }

    #[test]
    fn palette_is_opaque_where_used() {
        let p = build_palette();
        assert_eq!(p.rgba.len(), PAL_W * PAL_H * 4);
        for row in [0, 1, 9] {
            for col in 0..64 {
                assert_eq!(p.rgba[(row * PAL_W + col) * 4 + 3], 255);
            }
        }
    }
}
