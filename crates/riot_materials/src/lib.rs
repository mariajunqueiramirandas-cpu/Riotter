//! Tileable pixel-art materials for the ground and the roofs.
//!
//! Each material is a square texture that repeats every [`TILE_M`] metres,
//! generated once per art level at that level's density (512 px at level 0
//! down to 64 px at level 3). Layouts (slabs, setts, roof tiles, cracks,
//! leaf clusters) are laid out in level-0 pixels and sampled at each
//! level's pixel centres, so every level shows the same pattern drawn with
//! its own pixels; noise and grain are per pixel at every level, which keeps
//! the texture crisp instead of blurred. Joints and edge highlights appear
//! only where a level has the pixels for them.

use riot_art::{self as art, Img, Rgb, bayer, fbm, hex, mix, rnd, tone, vnoise};

/// Level-0 pixels per tile side.
pub const TILE_PX: usize = 512;
/// Metres covered by one repeat of a material.
pub const TILE_M: f32 = TILE_PX as f32 / art::DENSITY;

/// A ground or roof material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mat {
    Asphalt,
    Pavement,
    /// Granite kerb stones; `u` runs along the kerb.
    Kerb,
    /// Sandstone setts.
    Plaza,
    Grass,
    Water,
    /// Road paint (white, tinted for other colours).
    Paint,
    Concrete,
    /// Clay roof tiles; `u` along the eaves, `v` up the slope.
    Tiles,
    /// Slate; `u` along the eaves.
    Slate,
    /// A flat roof covered in gravel.
    Gravel,
    /// A flat roof with a bitumen membrane; `u` across the rolls.
    Tar,
    /// Corrugated metal; `u` across the ridges.
    Metal,
    /// Tree canopy.
    Foliage,
    /// Planks; `u` along the boards.
    Wood,
    /// Plain white, for tinted flat colours and shadows.
    Solid,
}

impl Mat {
    pub const ALL: [Mat; 16] = [
        Mat::Asphalt,
        Mat::Pavement,
        Mat::Kerb,
        Mat::Plaza,
        Mat::Grass,
        Mat::Water,
        Mat::Paint,
        Mat::Concrete,
        Mat::Tiles,
        Mat::Slate,
        Mat::Gravel,
        Mat::Tar,
        Mat::Metal,
        Mat::Foliage,
        Mat::Wood,
        Mat::Solid,
    ];

    pub fn index(self) -> usize {
        Mat::ALL.iter().position(|&m| m == self).unwrap_or(0)
    }

    /// The colour this material averages to, for flat far-away meshes.
    pub fn average(self) -> Rgb {
        match self {
            Mat::Asphalt => hex(0x4b4d53),
            Mat::Pavement => hex(0xb6b2a8),
            Mat::Kerb => hex(0xc8c6be),
            Mat::Plaza => hex(0xd2c4a3),
            Mat::Grass => hex(0x689a45),
            Mat::Water => hex(0x41729f),
            Mat::Paint => hex(0xe4e2d6),
            Mat::Concrete => hex(0xb2b0a9),
            Mat::Tiles => hex(0xac553a),
            Mat::Slate => hex(0x5d6672),
            Mat::Gravel => hex(0xa8a7a1),
            Mat::Tar => hex(0x56595f),
            Mat::Metal => hex(0x8c98a2),
            Mat::Foliage => hex(0x4f7f38),
            Mat::Wood => hex(0x8a6440),
            Mat::Solid => [255, 255, 255],
        }
    }
}

/// Texture side in pixels at an art level.
pub fn size(level: usize) -> usize {
    TILE_PX >> level
}

/// Sampling helper: a level's pixel `(x, y)` in level-0 pixel coordinates.
struct Grid {
    level: usize,
    k: usize,
    s: usize,
}

impl Grid {
    fn new(level: usize) -> Grid {
        Grid {
            level,
            k: art::step(level),
            s: size(level),
        }
    }

    /// Level-0 pixel at the centre of level pixel `x`.
    #[inline]
    fn l0(&self, x: usize) -> usize {
        x * self.k + self.k / 2
    }

    /// Tile units (0..1).
    #[inline]
    fn uv(&self, x: usize, y: usize) -> (f32, f32) {
        (
            (x as f32 + 0.5) / self.s as f32,
            (y as f32 + 0.5) / self.s as f32,
        )
    }

    /// Size in this level's pixels of `n` level-0 pixels.
    #[inline]
    fn px(&self, n: usize) -> usize {
        n / self.k
    }

    fn each(&self, mut f: impl FnMut(usize, usize) -> Rgb) -> Img {
        let mut img = Img::new(self.s, self.s);
        for y in 0..self.s {
            for x in 0..self.s {
                let c = f(x, y);
                img.set(x, y, c);
            }
        }
        img
    }
}

/// Generate `m` at an art level.
pub fn generate(m: Mat, level: usize) -> Img {
    let g = Grid::new(level);
    match m {
        Mat::Asphalt => asphalt(&g),
        Mat::Pavement => pavement(&g),
        Mat::Kerb => kerb(&g),
        Mat::Plaza => plaza(&g),
        Mat::Grass => grass(&g),
        Mat::Water => water(&g),
        Mat::Paint => paint(&g),
        Mat::Concrete => concrete(&g),
        Mat::Tiles => tiles(&g),
        Mat::Slate => slate(&g),
        Mat::Gravel => gravel(&g),
        Mat::Tar => tar(&g),
        Mat::Metal => metal(&g),
        Mat::Foliage => foliage(&g),
        Mat::Wood => wood(&g),
        Mat::Solid => Img::filled(g.s, g.s, [255, 255, 255]),
    }
}

/// Pick one of `tones` from a grain value in `[0, 1)` with these
/// cumulative weights.
fn grain(r: f32, tones: &[(f32, Rgb)]) -> Rgb {
    for &(w, c) in tones {
        if r < w {
            return c;
        }
    }
    tones.last().map_or([0, 0, 0], |t| t.1)
}

/// A random walk in level-0 pixels drawn at a level, wrapping.
fn crack(img: &mut Img, g: &Grid, seed: u32, start: (f32, f32), len: usize, c: Rgb) {
    let (mut x, mut y) = start;
    let mut a = rnd(seed as i32, 1, 77) * std::f32::consts::TAU;
    for i in 0..len {
        a += (rnd(i as i32, seed as i32, 78) - 0.5) * 0.9;
        x += a.cos();
        y += a.sin();
        img.set_wrap(
            (x / g.k as f32).floor() as i32,
            (y / g.k as f32).floor() as i32,
            c,
        );
        // A branch now and then.
        if i % 37 == 19 && rnd(i as i32, seed as i32, 79) < 0.5 && len > 40 {
            crack(img, g, seed.wrapping_mul(31) ^ i as u32, (x, y), len / 3, c);
        }
    }
}

fn asphalt(g: &Grid) -> Img {
    let base = hex(0x4a4c52);
    let tones = [
        (0.2, hex(0x404248)),
        (0.84, base),
        (0.975, hex(0x56595f)),
        (1.0, hex(0x6a6c71)),
    ];
    let mut img = g.each(|x, y| {
        let (u, v) = g.uv(x, y);
        let big = fbm(u, v, 3, 3, 11);
        let c = grain(rnd(x as i32, y as i32, 12 + g.level as u32), &tones);
        // Worn and patched areas, dithered so they stay pixel art.
        let t = (big - 0.5) * 3.0;
        if t > bayer(x as i32, y as i32) * 1.4 {
            tone(c, 0.08)
        } else if -t > bayer(x as i32, y as i32) * 1.4 + 0.25 {
            tone(c, -0.1)
        } else {
            c
        }
    });
    for k in 0..4u32 {
        let start = (
            rnd(k as i32, 0, 13) * TILE_PX as f32,
            rnd(k as i32, 1, 13) * TILE_PX as f32,
        );
        crack(&mut img, g, 100 + k, start, 90, hex(0x34363b));
    }
    img
}

fn pavement(g: &Grid) -> Img {
    const SLAB: usize = 32;
    let tones = [hex(0xb9b5ab), hex(0xc2beb3), hex(0xafaba1), hex(0xbcb7aa)];
    let grout = hex(0x8f8b83);
    let joints = g.px(SLAB) >= 4;
    let bevel = g.px(SLAB) >= 8;
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        let row = ly / SLAB;
        let off = (row % 2) * SLAB / 2;
        let col = ((lx + off) / SLAB) % (TILE_PX / SLAB);
        let (sx, sy) = (((lx + off) % SLAB) / g.k, (ly % SLAB) / g.k);
        let n = g.px(SLAB);
        if joints && (sx == 0 || sy == 0) {
            return grout;
        }
        let mut c = tones[(art::hash(col as i32, row as i32, 21) % 4) as usize];
        if bevel && (sx == 1 || sy == 1) {
            c = tone(c, 0.12);
        } else if bevel && (sx == n - 1 || sy == n - 1) {
            c = tone(c, -0.1);
        }
        // A cracked slab now and then.
        if bevel && art::hash(col as i32, row as i32, 22).is_multiple_of(43) {
            let d = (sx as i32 - sy as i32 * 2 / 3 - (n as i32) / 5).abs();
            if d == 0 {
                return tone(c, -0.35);
            }
        }
        let r = rnd(x as i32, y as i32, 23 + g.level as u32);
        let (u, v) = g.uv(x, y);
        let dirt = fbm(u, v, 4, 2, 24);
        if r < 0.06 || dirt > 0.68 + bayer(x as i32, y as i32) * 0.2 {
            tone(c, -0.12)
        } else if r > 0.975 {
            tone(c, 0.1)
        } else {
            c
        }
    })
}

fn kerb(g: &Grid) -> Img {
    const BLOCK: usize = 48;
    let base = hex(0xc9c7bf);
    g.each(|x, y| {
        let lx = g.l0(x);
        if g.px(BLOCK) >= 4 && (lx % BLOCK) / g.k == 0 {
            return hex(0x8e8c86);
        }
        let r = rnd(x as i32, y as i32, 31 + g.level as u32);
        grain(
            r,
            &[
                (0.03, hex(0x6f6e6a)),
                (0.15, hex(0xadaba4)),
                (0.9, base),
                (1.0, hex(0xdddbd3)),
            ],
        )
    })
}

fn plaza(g: &Grid) -> Img {
    const ROW: usize = 16;
    let rows = TILE_PX / ROW;
    // Stone boundaries per row, in level-0 pixels.
    let bounds: Vec<Vec<usize>> = (0..rows)
        .map(|r| {
            let mut b = vec![0];
            let mut at = (art::hash(r as i32, 0, 41) % 16) as usize;
            b[0] = at;
            loop {
                let w = [20, 24, 28, 32][(art::hash(r as i32, at as i32, 42) % 4) as usize];
                if at + w + 16 > TILE_PX + b[0] {
                    break;
                }
                at += w;
                b.push(at);
            }
            b
        })
        .collect();
    let tones = [
        hex(0xd8c9a7),
        hex(0xcdbd99),
        hex(0xe2d5b6),
        hex(0xc4b28f),
        hex(0xd3c2a0),
    ];
    let mortar = hex(0x9c8e74);
    let joints = g.px(ROW) >= 4;
    let bevel = g.px(ROW) >= 8;
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        let row = ly / ROW;
        let b = &bounds[row];
        // Wrap so the row's first stone continues past the tile edge.
        let lxw = if lx < b[0] { lx + TILE_PX } else { lx };
        let k = b.partition_point(|&s| s <= lxw).saturating_sub(1);
        let x0 = b[k];
        let x1 = b.get(k + 1).copied().unwrap_or(b[0] + TILE_PX);
        let (sx, sy) = ((lxw - x0) / g.k, (ly % ROW) / g.k);
        let (w, h) = ((x1 - x0) / g.k, g.px(ROW));
        if joints && (sx == 0 || sy == 0) {
            return mortar;
        }
        let mut c = tones[(art::hash(k as i32, row as i32, 43) % 5) as usize];
        if bevel && sy == 1 {
            c = tone(c, 0.12);
        } else if bevel && (sy == h - 1 || sx == w - 1) {
            c = tone(c, -0.12);
        }
        let r = rnd(x as i32, y as i32, 44 + g.level as u32);
        if r < 0.05 {
            tone(c, -0.1)
        } else if r > 0.98 {
            tone(c, 0.1)
        } else {
            c
        }
    })
}

fn grass(g: &Grid) -> Img {
    let greens = [hex(0x557f39), hex(0x638f40), hex(0x6e9d48), hex(0x7eae52)];
    let mut img = g.each(|x, y| {
        let (u, v) = g.uv(x, y);
        let n = fbm(u, v, 4, 3, 51) * 3.0 + rnd(x as i32, y as i32, 52) * 0.9 - 0.45;
        let b = bayer(x as i32, y as i32);
        let i = (n + b * 0.6 - 0.3).clamp(0.0, 3.0) as usize;
        greens[i]
    });
    // Tufts: short strokes, light on top, dark at the root.
    let tufts = 2600;
    for t in 0..tufts {
        let lx = rnd(t, 0, 53) * TILE_PX as f32;
        let ly = rnd(t, 1, 53) * TILE_PX as f32;
        let (px, py) = ((lx / g.k as f32) as i32, (ly / g.k as f32) as i32);
        let len = (4 / g.k).max(1) as i32;
        let light = rnd(t, 2, 53) < 0.6;
        for k in 0..len {
            let c = if k == 0 && light {
                hex(0x8fbd5f)
            } else if light {
                hex(0x7eae52)
            } else {
                hex(0x4a7232)
            };
            img.set_wrap(px, py + k, c);
        }
    }
    // A few flowers.
    for t in 0..90 {
        let lx = rnd(t, 0, 54) * TILE_PX as f32;
        let ly = rnd(t, 1, 54) * TILE_PX as f32;
        let c = [hex(0xf2efe0), hex(0xf2d34a), hex(0xd88ad0)][(t % 3) as usize];
        img.set_wrap((lx / g.k as f32) as i32, (ly / g.k as f32) as i32, c);
    }
    img
}

fn water(g: &Grid) -> Img {
    let deep = hex(0x355f8a);
    let base = hex(0x3d6e9c);
    let mut img = g.each(|x, y| {
        let (u, v) = g.uv(x, y);
        let n = fbm(u, v, 3, 3, 61);
        if n > 0.55 + bayer(x as i32, y as i32) * 0.15 {
            base
        } else {
            deep
        }
    });
    // Ripples: short light dashes, a glint here and there.
    for t in 0..260 {
        let lx = rnd(t, 0, 62) * TILE_PX as f32;
        let ly = rnd(t, 1, 62) * TILE_PX as f32;
        let len = (6.0 + rnd(t, 2, 62) * 14.0) / g.k as f32;
        let c = if t % 9 == 0 {
            hex(0x9cc6e6)
        } else {
            hex(0x5b8fbf)
        };
        let (px, py) = ((lx / g.k as f32) as i32, (ly / g.k as f32) as i32);
        for k in 0..len.max(1.0) as i32 {
            img.set_wrap(px + k, py, c);
        }
    }
    img
}

fn paint(g: &Grid) -> Img {
    let mut img = Img::new(g.s, g.s);
    for y in 0..g.s {
        for x in 0..g.s {
            let (u, v) = g.uv(x, y);
            let wear = vnoise(u, v, 16, 71);
            let r = rnd(x as i32, y as i32, 72 + g.level as u32);
            if r < 0.02 + (wear - 0.65).max(0.0) * 0.6 {
                continue; // worn through
            }
            let c = if r < 0.24 {
                hex(0xc9c7bc)
            } else {
                hex(0xe8e6da)
            };
            img.set(x, y, c);
        }
    }
    img
}

fn concrete(g: &Grid) -> Img {
    const PANEL: usize = 64;
    let base = hex(0xb4b2ab);
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        if g.px(PANEL) >= 6 && ((lx % PANEL) / g.k == 0 || (ly % PANEL) / g.k == 0) {
            return hex(0x96948d);
        }
        let (u, v) = g.uv(x, y);
        let stain = fbm(u, v, 4, 3, 81);
        let r = rnd(x as i32, y as i32, 82 + g.level as u32);
        let c = grain(
            r,
            &[(0.08, hex(0xa3a19a)), (0.92, base), (1.0, hex(0xc3c1ba))],
        );
        if stain > 0.6 + bayer(x as i32, y as i32) * 0.12 {
            tone(c, -0.1)
        } else {
            c
        }
    })
}

/// Rows of overlapping elements (roof tiles and slates): `w × h` level-0
/// pixels, every other row shifted by half. `f(row, col, sx, sy, w, h)`
/// colours a pixel from its position in its element (level pixels).
fn rows(
    g: &Grid,
    w: usize,
    h: usize,
    f: impl Fn(usize, usize, usize, usize, usize, usize) -> Rgb,
) -> Img {
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        let row = ly / h;
        let off = (row % 2) * w / 2;
        let col = ((lx + off) / w) % (TILE_PX / w);
        f(
            row,
            col,
            ((lx + off) % w) / g.k,
            (ly % h) / g.k,
            g.px(w).max(1),
            g.px(h).max(1),
        )
    })
}

fn tiles(g: &Grid) -> Img {
    let base = hex(0xc0603f);
    rows(g, 16, 16, |row, col, sx, sy, w, h| {
        let var = art::hash(col as i32, row as i32, 91);
        let mut c = match var % 7 {
            0 => hex(0xab5236),
            1 => hex(0xc86a47),
            _ => base,
        };
        if w >= 4 {
            // Curved tiles: a light crest, dark valleys at the sides.
            let t = (sx as f32 + 0.5) / w as f32;
            let crest = 1.0 - (t - 0.4).abs() * 2.6;
            c = if crest > 0.6 {
                tone(c, 0.18)
            } else if crest < -0.1 {
                tone(c, -0.22)
            } else {
                c
            };
        }
        if h >= 4 && sy + 1 == h {
            c = hex(0x6b2c1d); // the lower edge's shadow on the next row
        } else if h >= 6 && sy == 0 {
            c = tone(c, -0.1);
        }
        if var.is_multiple_of(23) {
            c = mix(c, hex(0x6c7a3a), 0.35); // a little moss
        }
        c
    })
}

fn slate(g: &Grid) -> Img {
    rows(g, 32, 16, |row, col, sx, sy, w, h| {
        let var = art::hash(col as i32, row as i32, 101);
        let mut c = match var % 6 {
            0 => hex(0x525a65),
            1 => hex(0x667080),
            _ => hex(0x5c6572),
        };
        if h >= 4 && sy + 1 == h {
            c = hex(0x343a42);
        } else if h >= 6 && sy == 0 {
            c = tone(c, 0.15);
        }
        if w >= 6 && sx == 0 {
            c = tone(c, -0.25);
        }
        c
    })
}

fn gravel(g: &Grid) -> Img {
    g.each(|x, y| {
        let (u, v) = g.uv(x, y);
        let stain = fbm(u, v, 4, 3, 111);
        let r = rnd(x as i32, y as i32, 112 + g.level as u32);
        let c = grain(
            r,
            &[
                (0.08, hex(0x8a8984)),
                (0.3, hex(0x9c9b95)),
                (0.75, hex(0xa9a8a2)),
                (0.93, hex(0xb8b7b1)),
                (1.0, hex(0xcac9c3)),
            ],
        );
        if stain > 0.62 + bayer(x as i32, y as i32) * 0.1 {
            tone(c, -0.12)
        } else {
            c
        }
    })
}

fn tar(g: &Grid) -> Img {
    const ROLL: usize = 48;
    g.each(|x, y| {
        let lx = g.l0(x);
        let sx = (lx % ROLL) / g.k;
        if g.px(ROLL) >= 4 && sx == 0 {
            return hex(0x6b6e75);
        }
        if g.px(ROLL) >= 6 && sx == 1 {
            return hex(0x45484e);
        }
        let (u, v) = g.uv(x, y);
        let sheen = fbm(u, v, 3, 3, 121);
        let r = rnd(x as i32, y as i32, 122 + g.level as u32);
        let c = if r < 0.1 {
            hex(0x4d5056)
        } else {
            hex(0x56595f)
        };
        if sheen > 0.6 + bayer(x as i32, y as i32) * 0.12 {
            tone(c, 0.1)
        } else {
            c
        }
    })
}

fn metal(g: &Grid) -> Img {
    const RIB: usize = 8;
    const SHEET: usize = 96;
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        if g.px(SHEET) >= 6 && (ly % SHEET) / g.k == 0 {
            return hex(0x5f6973);
        }
        let base = hex(0x8d99a3);
        let c = if g.px(RIB) >= 2 {
            match (lx % RIB) * 4 / RIB {
                0 => tone(base, 0.18),
                1 => base,
                2 => tone(base, -0.12),
                _ => tone(base, -0.25),
            }
        } else {
            base
        };
        if rnd(x as i32, y as i32, 131) < 0.04 {
            tone(c, -0.1)
        } else {
            c
        }
    })
}

fn foliage(g: &Grid) -> Img {
    let gap = hex(0x2c4724);
    let mut img = Img::filled(g.s, g.s, gap);
    let greens = [
        hex(0x3f6b2f),
        hex(0x4a7a35),
        hex(0x55863b),
        hex(0x4f7c3e),
        hex(0x5a8a33),
    ];
    // Leaf clusters, each lit from the north-west.
    let blobs = 420;
    for b in 0..blobs {
        let cx = rnd(b, 0, 141) * TILE_PX as f32;
        let cy = rnd(b, 1, 141) * TILE_PX as f32;
        let r = 12.0 + rnd(b, 2, 141) * 16.0;
        let base = greens[(art::hash(b, 3, 141) % 5) as usize];
        let (x0, x1) = (
            ((cx - r) / g.k as f32).floor() as i32,
            ((cx + r) / g.k as f32).ceil() as i32,
        );
        let (y0, y1) = (
            ((cy - r) / g.k as f32).floor() as i32,
            ((cy + r) / g.k as f32).ceil() as i32,
        );
        for py in y0..=y1 {
            for px in x0..=x1 {
                let lx = (px as f32 + 0.5) * g.k as f32;
                let ly = (py as f32 + 0.5) * g.k as f32;
                let (dx, dy) = ((lx - cx) / r, (ly - cy) / r);
                let d2 = dx * dx + dy * dy;
                if d2 > 1.0 {
                    continue;
                }
                let lit = -(dx * 0.6 + dy * 0.8);
                let c = if d2 > 0.8 && lit < 0.0 {
                    tone(base, -0.35)
                } else if lit > 0.45 && d2 < 0.7 {
                    tone(base, 0.3)
                } else if lit > 0.1 {
                    tone(base, 0.12)
                } else if lit < -0.4 {
                    tone(base, -0.18)
                } else {
                    base
                };
                img.set_wrap(px, py, c);
            }
        }
    }
    // Specks of light through the leaves.
    for t in 0..300 {
        let lx = rnd(t, 0, 142) * TILE_PX as f32;
        let ly = rnd(t, 1, 142) * TILE_PX as f32;
        img.set_wrap(
            (lx / g.k as f32) as i32,
            (ly / g.k as f32) as i32,
            hex(0x86b35a),
        );
    }
    img
}

fn wood(g: &Grid) -> Img {
    const BOARD: usize = 16;
    g.each(|x, y| {
        let (lx, ly) = (g.l0(x), g.l0(y));
        let board = ly / BOARD;
        if g.px(BOARD) >= 4 && (ly % BOARD) / g.k == 0 {
            return hex(0x4e3722);
        }
        let base = [hex(0x8a6440), hex(0x94704a), hex(0x7f5c3a)][board % 3];
        let (u, _) = g.uv(x, y);
        let streak = vnoise(u * 4.0, board as f32 * 0.37, 16, 151);
        let _ = lx;
        if streak > 0.68 {
            tone(base, -0.15)
        } else if streak < 0.25 {
            tone(base, 0.1)
        } else {
            base
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_material_tiles_at_every_level() {
        for m in Mat::ALL {
            for level in 0..art::LEVELS {
                let img = generate(m, level);
                assert_eq!((img.w, img.h), (size(level), size(level)), "{m:?}");
                assert!(img.px.iter().any(|p| p[3] > 0), "{m:?} is empty");
            }
        }
        assert!((TILE_M - 10.666_667).abs() < 1e-4);
    }

    /// `RIOT_MATERIALS_PNG=/dir cargo test -p riot_materials dump_materials -- --ignored`
    /// writes every material at every level for inspection.
    #[test]
    #[ignore]
    fn dump_materials() {
        let dir = std::env::var("RIOT_MATERIALS_PNG").unwrap_or_else(|_| ".".into());
        for m in Mat::ALL {
            for level in 0..art::LEVELS {
                let img = generate(m, level);
                let path = format!("{dir}/{m:?}-{level}.png");
                art::png::dump(&path, img.w, img.h, &img.bytes());
            }
        }
    }
}
