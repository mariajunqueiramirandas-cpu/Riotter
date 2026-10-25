//! Procedural sprite sheets and palettes (DESIGN §18.2), as pixel art.
//!
//! Every sheet is drawn once at startup per art level ([`riot_art`]), seen
//! from straight above with the person facing +x (forward) and +y to their
//! right. The vertex shader rotates each sprite by `heading`, so a gait needs
//! one strip of frames, not 8 or 16 directions.
//!
//! The atlas holds no colours, only **palette weights**: R, G and B are the
//! weights of three palette colours (which three depends on the layer, e.g.
//! clothes, skin and trousers for the body), premultiplied by coverage in A.
//! The fragment shader mixes the colours picked by a person's appearance bits,
//! so a handful of sheets yields thousands of distinct-looking people. Any
//! weight short of the coverage goes to a dark outline colour, so shading
//! bands also shift towards it.
//!
//! The shapes are signed-distance shapes rasterized with hard edges: a pixel
//! is painted when its centre is inside. Shading comes in bands, parts get a
//! darker contour where they cross other parts (at the two densest levels),
//! and every sprite gets a one-pixel outline.

use std::f32::consts::TAU;

use riot_art::{self as art, Shape};

/// Metres covered by a cell side; the person stands at the cell centre.
pub const CELL_M: f32 = 2.0;
/// Atlas cells per row.
pub const COLS: usize = 16;
/// Atlas rows of cells.
pub const ROWS: usize = 5;

/// Pixels per atlas cell side at an art level (96 at level 0).
pub fn cell(level: usize) -> usize {
    (CELL_M * art::density(level)) as usize
}

/// Atlas width in pixels at an art level.
pub fn atlas_w(level: usize) -> usize {
    cell(level) * COLS
}

/// Atlas height in pixels at an art level.
pub fn atlas_h(level: usize) -> usize {
    cell(level) * ROWS
}

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
    /// A soft shadow under a standing person.
    pub const SHADOW: u32 = 28;
    /// Body accessories: backpack, scarf, hi-vis vest, press vest.
    pub const ACCESSORY: u32 = 32;
    /// 16 hair styles.
    pub const HEAD: u32 = 36;
    /// Cap, helmet, balaclava.
    pub const HEADWEAR: u32 = 52;
    /// Held items by `riot_defs::state::Item` code (0 is unused).
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

/// The generated sheets of one art level: premultiplied RGBA8,
/// `atlas_w(level) × atlas_h(level)`.
pub struct Atlas {
    pub level: usize,
    pub rgba: Vec<u8>,
}

/// The generated palettes: RGBA8, `PAL_W × PAL_H`.
pub struct Palette {
    pub rgba: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Painting

type P = [f32; 2];

/// Which palette weight a shape writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ink {
    /// Palette colour A (R channel).
    A,
    /// Palette colour B (G channel).
    B,
    /// Palette colour C (B channel).
    C,
    /// The outline colour.
    Black,
}

#[derive(Clone, Copy, Debug)]
struct Style {
    /// Brightness at the shape's centre.
    shade: f32,
    /// How much darker the rim is (0 = flat).
    bulge: f32,
    opacity: f32,
    /// Draw a darker contour where the shape crosses earlier parts.
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
    fn bulge_less(self) -> Style {
        Style {
            bulge: self.bulge * 0.5,
            ..self
        }
    }
}

/// Weight of a palette colour at its plain brightness; the shader scales
/// weights by `1 / BASE`, so a weight of 1 is a highlight.
pub const BASE: f32 = 0.8;

/// One level of the atlas being painted: weights and coverage per pixel.
struct Canvas {
    level: usize,
    ppm: f32,
    cell: usize,
    w: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(level: usize) -> Canvas {
        let (w, h) = (atlas_w(level), atlas_h(level));
        Canvas {
            level,
            ppm: art::density(level),
            cell: cell(level),
            w,
            px: vec![[0.0; 4]; w * h],
        }
    }

    /// Pixels of frame `f`'s cell: origin.
    fn origin(&self, f: u32) -> (usize, usize) {
        (
            (f as usize % COLS) * self.cell,
            (f as usize / COLS) * self.cell,
        )
    }

    /// Fine details are only drawn where they span at least a pixel.
    fn detailed(&self) -> bool {
        self.level <= 1
    }

    fn draw(&mut self, f: u32, shape: &Shape, ink: Ink, style: Style) {
        if style.outline && self.detailed() && ink != Ink::Black {
            // Contour: one pixel around the part, a dark band of its own
            // colour, only over parts already painted (the silhouette gets
            // the outline instead).
            self.fill(
                f,
                shape,
                1.0,
                ink,
                style.shade * 0.45,
                0.0,
                style.opacity,
                true,
            );
        }
        let bulge = if self.level <= 2 { style.bulge } else { 0.0 };
        self.fill(f, shape, 0.0, ink, style.shade, bulge, style.opacity, false);
    }

    #[allow(clippy::too_many_arguments)]
    fn fill(
        &mut self,
        f: u32,
        shape: &Shape,
        grow_px: f32,
        ink: Ink,
        shade: f32,
        bulge: f32,
        opacity: f32,
        over_only: bool,
    ) {
        let (ox, oy) = self.origin(f);
        let (lo, hi) = shape.bounds();
        let half = CELL_M * 0.5;
        let to_px = |m: f32| (m + half) * self.ppm;
        let x0 = (to_px(lo[0]) - grow_px - 2.0).floor().max(0.0) as usize;
        let y0 = (to_px(lo[1]) - grow_px - 2.0).floor().max(0.0) as usize;
        let x1 = ((to_px(hi[0]) + grow_px + 2.0).ceil() as usize).min(self.cell);
        let y1 = ((to_px(hi[1]) + grow_px + 2.0).ceil() as usize).min(self.cell);
        // Highlight only on shapes big enough to show one.
        let (smin, _) = {
            let e = [hi[0] - lo[0], hi[1] - lo[1]];
            (e[0].min(e[1]) * self.ppm, ())
        };
        for py in y0..y1 {
            for px in x0..x1 {
                let p = [
                    (px as f32 + 0.5) / self.ppm - half,
                    (py as f32 + 0.5) / self.ppm - half,
                ];
                let (d, r) = shape.eval(p);
                if d * self.ppm >= grow_px + 0.02 {
                    continue;
                }
                let dst = &mut self.px[(oy + py) * self.w + ox + px];
                if over_only && dst[3] < 0.5 {
                    continue;
                }
                // Banded shading: base, two rim bands, a centre highlight.
                let mut s = shade * BASE * (1.0 - bulge * r * r);
                if bulge > 0.1 && r < 0.38 && smin >= 5.0 && !over_only {
                    s = shade;
                }
                let s = ((s * 12.5).round() / 12.5).clamp(0.0, 1.0);
                let w = match ink {
                    Ink::A => [s, 0.0, 0.0],
                    Ink::B => [0.0, s, 0.0],
                    Ink::C => [0.0, 0.0, s],
                    Ink::Black => [0.0, 0.0, 0.0],
                };
                let cov = opacity;
                let keep = 1.0 - cov;
                for c in 0..3 {
                    dst[c] = w[c] * cov + dst[c] * keep;
                }
                dst[3] = cov + dst[3] * keep;
            }
        }
    }

    /// One-pixel outline around everything painted in frame `f`.
    fn outline(&mut self, f: u32) {
        let (ox, oy) = self.origin(f);
        let n = self.cell;
        let a = match self.level {
            0 | 1 => 1.0,
            2 => 0.8,
            _ => 0.5,
        };
        let solid = |c: &Canvas, x: usize, y: usize| c.px[(oy + y) * c.w + ox + x][3] > 0.45;
        let mut add = Vec::new();
        // Keep the cell's border clear so filtering never bleeds.
        for y in 1..n - 1 {
            for x in 1..n - 1 {
                if self.px[(oy + y) * self.w + ox + x][3] > 0.0 {
                    continue;
                }
                if solid(self, x - 1, y)
                    || solid(self, x + 1, y)
                    || solid(self, x, y - 1)
                    || solid(self, x, y + 1)
                {
                    add.push((x, y));
                }
            }
        }
        for (x, y) in add {
            self.px[(oy + y) * self.w + ox + x] = [0.0, 0.0, 0.0, a];
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
pub fn build_atlas(level: usize) -> Atlas {
    let mut cv = Canvas::new(level);
    body_frames(&mut cv);
    accessory_frames(&mut cv);
    head_frames(&mut cv);
    item_frames(&mut cv);
    lying_frame(&mut cv);
    mid_frames(&mut cv);
    for f in 0..frame::COUNT {
        if f != frame::SHADOW {
            cv.outline(f);
        }
    }
    shadow_frame(&mut cv);
    Atlas {
        level,
        rgba: cv.into_rgba8(),
    }
}

/// A soft shadow: a flat ellipse of plain coverage (the shader draws it
/// dark and translucent).
fn shadow_frame(cv: &mut Canvas) {
    cv.draw(
        frame::SHADOW,
        &Shape::Ellipse([0.0, 0.0], [0.2, 0.3]),
        Ink::Black,
        SOLID.flat().no_outline(),
    );
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
/// `riot_snapshot::appearance_for`: 0–43 crowd, 44–51 radicals, 52–55
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
        let (n, w) = (cell(atlas.level), atlas_w(atlas.level));
        let (ox, oy) = ((f as usize % COLS) * n, (f as usize / COLS) * n);
        let mut c = 0;
        for y in 0..n {
            for x in 0..n {
                c += (atlas.rgba[((oy + y) * w + ox + x) * 4 + 3] > 128) as u32;
            }
        }
        c
    }

    #[test]
    fn every_frame_is_painted_and_stays_inside_its_cell() {
        assert!(frame::COUNT as usize <= COLS * ROWS);
        for level in 0..art::LEVELS {
            let atlas = build_atlas(level);
            let (n, w) = (cell(level), atlas_w(level));
            assert_eq!(atlas.rgba.len(), w * atlas_h(level) * 4);
            for f in 0..frame::COUNT {
                if f == frame::ITEM || f == frame::HEADWEAR + 3 || (29..32).contains(&f) {
                    continue; // unused slots
                }
                // Small parts may vanish at the coarsest levels.
                let min = [30, 10, 2, 0][level];
                assert!(coverage(&atlas, f) >= min, "frame {f} is empty at {level}");
                // The border stays clear, so sampling never bleeds.
                let (ox, oy) = ((f as usize % COLS) * n, (f as usize / COLS) * n);
                for k in 0..n {
                    for (x, y) in [(k, 0), (k, n - 1), (0, k), (n - 1, k)] {
                        let a = atlas.rgba[((oy + y) * w + ox + x) * 4 + 3];
                        assert_eq!(a, 0, "frame {f} touches its border at {level}");
                    }
                }
            }
        }
    }

    #[test]
    fn weights_are_premultiplied() {
        for level in 0..art::LEVELS {
            let atlas = build_atlas(level);
            for px in atlas.rgba.as_chunks::<4>().0 {
                let sum = px[0] as u32 + px[1] as u32 + px[2] as u32;
                assert!(sum <= px[3] as u32 + 1);
            }
        }
    }

    /// `RIOT_ATLAS_PNG=/path/atlas.png cargo test -p riot_sprites dump_atlas -- --ignored`
    /// writes the level-0 atlas (weights shown as R/G/B over grey) for
    /// inspection, and `atlas-1.png` … for the other levels.
    #[test]
    #[ignore]
    fn dump_atlas() {
        let path = std::env::var("RIOT_ATLAS_PNG").unwrap_or_else(|_| "atlas.png".into());
        for level in 0..art::LEVELS {
            let atlas = build_atlas(level);
            let mut bytes = atlas.rgba.clone();
            for px in bytes.as_chunks_mut::<4>().0 {
                let k = 255 - px[3] as u32;
                for c in 0..3 {
                    px[c] = (px[c] as u32 + 90 * k / 255) as u8;
                }
                px[3] = 255;
            }
            let p = if level == 0 {
                path.clone()
            } else {
                path.replace(".png", &format!("-{level}.png"))
            };
            art::png::dump(&p, atlas_w(level), atlas_h(level), &bytes);
        }
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
