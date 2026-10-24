//! `image`: run a scenario and write a top-down PNG of the world — walls,
//! people coloured by what they are doing, objectives and field heat — for
//! eyeballing behaviour without a GPU. A minimal PNG encoder (stored deflate
//! blocks) keeps the tool dependency-free.

use riot_core::Vec2;
use riot_sim::fields::Layer;
use riot_sim::state::{Action, Gait, flag, pose, stat};
use riot_sim::{Archetype, World};

use crate::args::Args;

pub fn image(args: &Args) -> Result<(), String> {
    let sc = crate::scenario::load(args)?;
    let threads = args.usize("threads").unwrap_or_else(crate::default_threads);
    let pool = crate::pool(threads);
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    w.snap_cfg.enabled = false;
    let ticks = args.u32("ticks").unwrap_or(1200);
    let every = args.u32("every").unwrap_or(0);
    let out = args.str("out").unwrap_or("riot.png".into());
    let layer = args.str("layer");
    for t in 0..ticks {
        pool.install(|| w.tick(&[]));
        if every > 0 && (t + 1) % every == 0 {
            let path = out.replace(".png", &format!("_{:06}.png", w.tick));
            write(&w, args, &path, layer.as_deref())?;
        }
    }
    write(&w, args, &out, layer.as_deref())
}

fn write(w: &World, args: &Args, path: &str, layer: Option<&str>) -> Result<(), String> {
    let px_per_m = args.f32("scale").unwrap_or(1.0);
    let (cx, cy) = match args.str("centre") {
        Some(s) => {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (
                v.first().copied().unwrap_or(0.0),
                v.get(1).copied().unwrap_or(0.0),
            )
        }
        None => (0.0, 0.0),
    };
    let size = args.u32("px").unwrap_or(1024) as usize;
    let mut img = vec![[24u8, 24, 28]; size * size];
    let to_world = |x: usize, y: usize| {
        Vec2::new(
            cx + (x as f32 - size as f32 * 0.5) / px_per_m,
            cy + (y as f32 - size as f32 * 0.5) / px_per_m,
        )
    };
    // Ground: walkable cells light, walls dark; optional field heat on top.
    let heat = layer.map(|l| match l {
        "dens" => Layer::Dens,
        "fear" => Layer::Fear,
        "anger" => Layer::Anger,
        "threat" => Layer::Threat,
        "gas" => Layer::Gas,
        "camera" => Layer::Camera,
        _ => Layer::Smoke,
    });
    for y in 0..size {
        for x in 0..size {
            let p = to_world(x, y);
            let mut c = if w.map.walkable_at(p) {
                [70u8, 72, 78]
            } else {
                [30, 30, 34]
            };
            if let Some(l) = heat {
                let v = w.fields.sample(l, p.x, p.y);
                let v = match l {
                    Layer::Dens => v / 8.0,
                    Layer::Threat => v / 10.0,
                    Layer::Camera => v / 6.0,
                    _ => v,
                }
                .clamp(0.0, 1.0);
                c = [
                    (c[0] as f32 * (1.0 - v) + 255.0 * v) as u8,
                    (c[1] as f32 * (1.0 - v) + 80.0 * v) as u8,
                    (c[2] as f32 * (1.0 - v)) as u8,
                ];
            }
            img[y * size + x] = c;
        }
    }
    let to_px = |p: Vec2| {
        let x = (p.x - cx) * px_per_m + size as f32 * 0.5;
        let y = (p.y - cy) * px_per_m + size as f32 * 0.5;
        (x, y)
    };
    let mut disc = |img: &mut [[u8; 3]], p: Vec2, r: f32, col: [u8; 3]| {
        let (x, y) = to_px(p);
        let rp = (r * px_per_m).max(0.6);
        for dy in -(rp.ceil() as i32)..=rp.ceil() as i32 {
            for dx in -(rp.ceil() as i32)..=rp.ceil() as i32 {
                if (dx * dx + dy * dy) as f32 > rp * rp + 0.5 {
                    continue;
                }
                let (ix, iy) = (x as i32 + dx, y as i32 + dy);
                if ix >= 0 && iy >= 0 && (ix as usize) < size && (iy as usize) < size {
                    img[iy as usize * size + ix as usize] = col;
                }
            }
        }
    };
    for o in w.objectives.iter().filter(|o| o.active) {
        let (x, y) = to_px(o.pos);
        let r = o.radius * px_per_m;
        for k in 0..720 {
            let a = Vec2::from_heading((k * 256 / 720) as u8);
            let (ix, iy) = ((x + a.x * r) as i32, (y + a.y * r) as i32);
            if ix >= 0 && iy >= 0 && (ix as usize) < size && (iy as usize) < size {
                img[iy as usize * size + ix as usize] = [240, 220, 60];
            }
        }
    }
    for pr in &w.props {
        disc(&mut img, pr.pos, pr.half.x.min(pr.half.y), [140, 110, 90]);
    }
    for i in 0..w.people.len {
        if w.people.is_dead(i) {
            continue;
        }
        let st = w.people.stat[i];
        let a = Archetype::from_u8(st[stat::ARCH]);
        let act = Action::from_u8(st[stat::ACTION]);
        let gait = Gait::from_u8(w.people.pose[i][pose::GAIT]);
        let col = if a.is_officer() {
            [60, 110, 255]
        } else if gait.is_down() {
            [255, 40, 40]
        } else if st[stat::FLAGS] & flag::ASLEEP != 0 {
            [120, 120, 120]
        } else {
            match act {
                Action::March => [80, 220, 120],
                Action::Hold => [230, 230, 230],
                Action::Flee => [255, 140, 0],
                Action::Throw | Action::Barricade => [255, 0, 200],
                Action::GoHome => [150, 90, 200],
                Action::Film => [0, 230, 230],
                Action::Rest | Action::SeekSupply | Action::Help => [200, 200, 80],
                _ => [160, 170, 160],
            }
        };
        disc(&mut img, w.people.pos(i), 0.25, col);
    }
    for v in &w.police.vehicles {
        disc(&mut img, v.pos, 1.2, [40, 60, 200]);
    }
    std::fs::write(path, encode_png(&img, size, size)).map_err(|e| format!("{path}: {e}"))?;
    println!("wrote {path} at tick {}", w.tick);
    Ok(())
}

fn crc32(data: &[u8], mut c: u32) -> u32 {
    c = !c;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn encode_png(img: &[[u8; 3]], w: usize, h: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for p in &img[y * w..(y + 1) * w] {
            raw.extend_from_slice(p);
        }
    }
    // zlib with stored (uncompressed) deflate blocks.
    let mut z = vec![0x78, 0x01];
    for (k, block) in raw.chunks(65_535).enumerate() {
        let last = (k + 1) * 65_535 >= raw.len();
        z.push(last as u8);
        let n = block.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let c = crc32(data, crc32(kind, 0));
        out.extend_from_slice(&c.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}
