//! Riotter's window and input (DESIGN §4.1, §18.3).
//!
//! macroquad provides the window, input and debug drawing; `riot_render`
//! draws snapshots; the simulation runs on its own thread and rayon pool
//! ([`sim_thread`]). Every player action becomes a command stamped with a
//! tick ([`input`]); tunables reload from a file ([`hotreload`]).
//!
//! `--ticks N --screenshot PATH` runs N ticks headlessly, renders, saves a
//! PNG and exits, so rendering can be verified without a display (Xvfb).

mod args;
mod hotreload;
mod hud;
mod input;
mod scenario;
mod sim_thread;

use std::time::Instant;

use macroquad::prelude::*;
use riot_render::{Camera, Frame, RenderStats, Renderer};
use riot_sim::setup::{ObjectiveKind, SupplyKind};
use riot_sim::{CommandKind, DebugCommand, MapDef, Stance, World};

use args::{Args, USAGE};
use hotreload::TunablesWatcher;
use hud::{FrameTimes, HudInput};
use input::Input;
use sim_thread::{SimHandle, Snapshots, ToSim};

fn parse_args() -> Args {
    match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}\n");
            }
            eprint!("{USAGE}");
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    }
}

fn window_conf() -> macroquad::conf::Conf {
    let args = parse_args();
    let mut c = macroquad::conf::Conf {
        miniquad_conf: macroquad::miniquad::conf::Conf {
            window_title: "Riotter".to_string(),
            window_width: args.window.0,
            window_height: args.window.1,
            window_resizable: true,
            high_dpi: false,
            sample_count: 4,
            ..Default::default()
        },
        // Keep drawing every frame: the simulation moves without input.
        update_on: None,
        ..Default::default()
    };
    if args.screenshot.is_some() {
        // Unthrottled, so measured frame times are the real cost.
        c.miniquad_conf.platform.swap_interval = Some(0);
    }
    c
}

/// Sample commands for `--demo` around `c`: escalation, gas, a fire at the
/// nearest parked car, supplies and a hold objective, scheduled `lead`
/// ticks before `at`.
fn demo_commands(map: &MapDef, c: Vec2, at: u32, lead: u32) -> Vec<(u32, CommandKind)> {
    let t = at.saturating_sub(lead);
    let car = map
        .props
        .iter()
        .min_by(|a, b| {
            let d = |p: [f32; 2]| vec2(p[0], p[1]).distance_squared(c + vec2(25.0, 20.0));
            d(a.pos).total_cmp(&d(b.pos))
        })
        .map_or(c + vec2(25.0, 20.0), |p| vec2(p.pos[0], p.pos[1]));
    // Gas is blocked by walls, so canisters land on the nearest junction.
    let street = |p: Vec2| {
        map.roads
            .nodes
            .iter()
            .map(|n| vec2(n[0], n[1]))
            .min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p)))
            .unwrap_or(p)
    };
    let (g1, g2) = (street(c + vec2(20.0, -14.0)), street(c + vec2(-40.0, 10.0)));
    let dbg = CommandKind::Debug;
    let drop = |kind, d: Vec2, amount| CommandKind::DropSupplies {
        kind,
        x: c.x + d.x,
        y: c.y + d.y,
        amount,
    };
    vec![
        (t, dbg(DebugCommand::Escalate { rung: 3 })),
        (t, dbg(DebugCommand::Gas { x: g1.x, y: g1.y })),
        (t + 60, dbg(DebugCommand::Gas { x: g2.x, y: g2.y })),
        (t, dbg(DebugCommand::Ignite { x: car.x, y: car.y })),
        (t, CommandKind::SetStance(Stance::Confrontational)),
        (t, drop(SupplyKind::Water, vec2(8.0, 6.0), 30)),
        (t, drop(SupplyKind::Medic, vec2(-12.0, -6.0), 20)),
        (t, drop(SupplyKind::Shields, vec2(-4.0, 12.0), 10)),
        (
            t,
            CommandKind::PlaceObjective {
                slot: 1,
                kind: ObjectiveKind::Hold,
                x: c.x + 30.0,
                y: c.y,
                radius: 15.0,
            },
        ),
    ]
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

#[macroquad::main(window_conf)]
async fn main() {
    let args = parse_args();
    let loaded = match scenario::load_world(&args) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot load the riot: {e}");
            std::process::exit(1);
        }
    };
    let (map, setup, tun) = (loaded.map, loaded.setup, loaded.tunables);
    let mut world = match World::new(&setup, &map, tun) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot start the riot: {e}");
            std::process::exit(1);
        }
    };
    world.snap_cfg.overlay = args.overlay;
    let people_total = world.max_people as usize;
    let threads = args.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
    });
    let threads = threads.max(1);
    let shot = args.screenshot.clone();
    let paused = args.paused || shot.is_some();
    let sim = SimHandle::spawn(world, threads, args.speed, paused);
    // A screenshot needs at least one snapshot.
    let ff = if shot.is_some() {
        args.ticks.max(1)
    } else {
        args.ticks
    };
    if args.demo {
        let c = args.camera.map_or(Vec2::ZERO, |(x, y, _)| vec2(x, y));
        for (t, k) in demo_commands(&map, c, ff, 240) {
            sim.send(ToSim::At(t, k));
        }
    }
    if ff > 0 {
        sim.send(ToSim::FastForward(ff));
    }

    let mut renderer = Renderer::new(&map);
    renderer.options.lod = args.lod;
    renderer.options.debug = args.markers;
    eprintln!(
        "riot_app: {} people, {} sim threads, GL {}",
        people_total,
        threads,
        Renderer::gl_info()
    );
    let screen = vec2(screen_width(), screen_height());
    let map_half = vec2(map.width * 0.5, map.height * 0.5);
    let mut cam = match args.camera {
        Some((x, y, z)) => Camera::new(vec2(x, y), z, screen),
        None => Camera::fit(map.width, map.height, screen),
    };
    let mut input = Input::new(args.speed, paused, args.overlay);
    let mut snaps = Snapshots::default();
    let mut watcher = loaded.watch.map(|p| TunablesWatcher::new(p, tun));
    let mut times = FrameTimes::default();
    let (mut cpu_samples, mut frame_samples) = (Vec::new(), Vec::new());
    let mut shot_frames = 0u32;
    let mut live_start: Option<Instant> = None;
    let start = Instant::now();
    let mut last = Instant::now();

    loop {
        let t0 = Instant::now();
        let dt = (t0 - last).as_secs_f32();
        last = t0;
        cam.screen = vec2(screen_width(), screen_height());
        snaps.receive(&sim);
        let status = sim.status();
        let now = start.elapsed().as_secs_f64();
        if shot.is_none() {
            input.update(
                &mut cam,
                &snaps.cur,
                &sim,
                &mut renderer.options,
                map_half,
                now,
                dt,
            );
        }
        if let Some(w) = &mut watcher {
            for k in w.poll(t0) {
                sim.command(k);
            }
        }

        let render = if snaps.has_cur {
            renderer.draw(&Frame {
                prev: snaps.prev(),
                cur: &snaps.cur,
                alpha: snaps.alpha(t0),
                camera: cam,
                dt,
                time: now as f32,
                selected: input.selected,
                chanting: input
                    .selected
                    .is_some_and(|id| input.chanting.contains(&id)),
                highlight_slot: Some(input.slot),
                tool: shot.is_none().then(|| input.tool_preview()),
            })
        } else {
            clear_background(Color::new(0.1, 0.11, 0.13, 1.0));
            draw_text("simulating…", 20.0, screen_height() * 0.5, 30.0, WHITE);
            RenderStats::default()
        };
        hud::draw(&HudInput {
            snap: snaps.has_cur.then_some(&snaps.cur),
            status,
            input: &input,
            render,
            frame: times,
            zoom: cam.zoom,
            reload: watcher.as_ref().and_then(|w| w.status.as_deref()),
            people_total,
        });
        let cpu_ms = t0.elapsed().as_secs_f32() * 1000.0;
        times.add(cpu_ms, dt * 1000.0);

        if let Some(path) = &shot {
            let ff_done = status.fast_forward == 0 && snaps.has_cur && status.published.is_some();
            let ready = if args.live > 0.0 {
                // Live: run at --speed while rendering, then capture.
                if ff_done && live_start.is_none() {
                    live_start = Some(t0);
                    sim.send(ToSim::Pause(false));
                }
                live_start.is_some_and(|s| (t0 - s).as_secs_f32() >= args.live)
            } else {
                ff_done && status.published == Some(snaps.cur.tick)
            };
            if live_start.is_some_and(|s| (t0 - s).as_secs_f32() > 0.5) {
                cpu_samples.push(cpu_ms);
                frame_samples.push(dt * 1000.0);
            }
            if ff_done && shot_frames == 0 && args.demo {
                input.selected = Input::nearest_organizer(&snaps.cur, cam.center);
            }
            if ready {
                shot_frames += 1;
                if args.live == 0.0 && shot_frames > 2 {
                    cpu_samples.push(cpu_ms);
                    frame_samples.push(dt * 1000.0);
                }
                if shot_frames
                    >= if args.live > 0.0 {
                        1
                    } else {
                        args.frames.max(3)
                    }
                {
                    get_screen_data().export_png(&path.to_string_lossy());
                    let n = frame_samples.len();
                    let worst = frame_samples.iter().copied().fold(0.0, f32::max);
                    println!(
                        "screenshot {} at tick {} ({} people drawn, lod {}): \
                         frame median {:.2} ms, worst {:.2} ms (cpu median {:.2} ms, \
                         people cull+upload {:.2} ms) over {} frames; \
                         sim {:.2} ms/tick, {:.0} ticks/s at {}x{}",
                        path.display(),
                        snaps.cur.tick,
                        render.people,
                        render.lod.map_or("-", |l| l.name()),
                        median(&mut frame_samples),
                        worst,
                        median(&mut cpu_samples),
                        render.gather_ms,
                        n,
                        status.tick_ms,
                        status.ticks_per_sec,
                        status.speed,
                        if status.behind { " (sim behind)" } else { "" },
                    );
                    let sections: Vec<String> = riot_render::SECTIONS
                        .iter()
                        .zip(render.sections_ms)
                        .map(|(n, ms)| format!("{n} {ms:.2}"))
                        .collect();
                    println!("render cpu ms (last frame): {}", sections.join(", "));
                    break;
                }
            }
        }
        if input.quit || !sim.alive() {
            break;
        }
        next_frame().await;
    }
    drop(sim);
}
