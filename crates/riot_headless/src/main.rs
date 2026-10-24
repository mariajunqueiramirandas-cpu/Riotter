//! Headless runner (DESIGN §4.1, §19): batch runs, benchmarks, determinism
//! and allocation checks, map-size independence and auto-resolve.

mod alloc;
mod args;
mod bench;
mod image;
mod scenario;

use std::process::ExitCode;
use std::time::Instant;

use riot_sim::World;

use crate::args::Args;

const USAGE: &str = "\
riot_headless <command> [options]

commands:
  run          run a scenario and print progress
  bench        per-phase ms/tick (p50, p99) against the DESIGN §3.1 budget
  determinism  identical state hash on every tick at several thread counts
  alloc        zero heap allocations per tick after warm-up
  hashes       print the state hash after every tick (compare builds with diff)
  mapsize      the same crowd on a 512 m and an 8 km map costs the same
  resolve      auto-resolve: run to the end and print the RiotOutcome
  gen-map      write a generated city as Tiled JSON (--out PATH --size M --seed X)
  image        write a top-down PNG (--out PATH --px N --scale PX_PER_M --centre X,Y
               --layer dens|fear|anger|threat|gas|camera --every N)

scenario options:
  --scenario PATH   RON scenario (default: a generated district)
  --people N        crowd size for the generated scenario   [2000]
  --squads N        police squads                           [8]
  --size M          generated map size in metres            [1024]
  --seed X          seed                                    [1]
  --tunables PATH   tunables.ron overriding the defaults
  --central         crowd arrives around the square, police on the scene
  --cap N           escalation cap 0..=4

run options:
  --ticks N         ticks to run (default: the scenario's duration)
  --threads N       simulation threads (default: all cores)
  --warmup N        ticks before measuring (bench, alloc)   [200]
  --hash-every N    print the state hash every N ticks
  --csv PATH        write per-phase results (bench)
  --list T1,T2,..   thread counts (determinism)             [1,2,4,8]
  --budget          fail when a phase is more than 10% over budget (bench)
";

fn main() -> ExitCode {
    let mut args = Args::from_env();
    let Some(cmd) = args.command() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match cmd.as_str() {
        "run" => run(&args),
        "bench" => bench::bench(&args),
        "determinism" => determinism(&args),
        "alloc" => alloc::check(&args),
        "hashes" => hashes(&args),
        "mapsize" => bench::mapsize(&args),
        "resolve" => resolve(&args),
        "gen-map" => scenario::gen_map(&args),
        "image" => image::image(&args),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    if let Some(unused) = args.unused() {
        eprintln!("warning: unused arguments: {unused}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
}

pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn run(args: &Args) -> Result<(), String> {
    let sc = scenario::load(args)?;
    let threads = args.usize("threads").unwrap_or_else(default_threads);
    let pool = pool(threads);
    let t0 = Instant::now();
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    w.snap_cfg.enabled = false;
    println!(
        "built `{}` ({} m, {} nav tiles mixed of {}, {} region nodes) in {:.2?}",
        w.map.name,
        w.map.size,
        w.map.mixed.len(),
        w.map.tiles.len(),
        w.map.rnodes.len(),
        t0.elapsed()
    );
    let ticks = args.u32("ticks").unwrap_or(sc.setup.duration);
    let hash_every = args.u32("hash-every").unwrap_or(0);
    let report = (ticks / 20).max(1);
    let t1 = Instant::now();
    let mut last = Instant::now();
    for t in 0..ticks {
        pool.install(|| w.tick(&[]));
        if hash_every > 0 && (t + 1) % hash_every == 0 {
            println!("tick {:>6} hash {:016x}", w.tick, w.state_hash());
        }
        if (t + 1) % report == 0 {
            let ms = last.elapsed().as_secs_f64() * 1000.0 / report as f64;
            last = Instant::now();
            print_status(&mut w, ms);
        }
    }
    let el = t1.elapsed();
    println!(
        "{ticks} ticks in {el:.2?} ({:.3} ms/tick, {:.1}× real time)",
        el.as_secs_f64() * 1000.0 / ticks as f64,
        ticks as f64 / 20.0 / el.as_secs_f64()
    );
    println!("final hash {:016x}", w.state_hash());
    Ok(())
}

fn print_status(w: &mut World, ms: f64) {
    let present = w.present();
    let mut asleep = 0;
    let mut acts = [0u32; riot_sim::state::ACTIONS];
    for i in 0..w.people.len {
        if w.people.is_dead(i) {
            continue;
        }
        let st = w.people.stat[i];
        if st[riot_sim::state::stat::FLAGS] & riot_sim::state::flag::ASLEEP != 0 {
            asleep += 1;
        }
        acts[st[riot_sim::state::stat::ACTION] as usize] += 1;
    }
    let top: Vec<String> = {
        let mut v: Vec<(u32, usize)> = acts
            .iter()
            .enumerate()
            .map(|(k, &n)| (n, k))
            .filter(|x| x.0 > 0)
            .collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.iter()
            .take(5)
            .map(|&(n, k)| format!("{} {n}", riot_sim::state::Action::from_u8(k as u8).name()))
            .collect()
    };
    let mut near = 0;
    let mut dist = 0.0f64;
    let mut moving = 0;
    if let Some(ob) = w.objectives.iter().find(|o| o.active) {
        for i in 0..w.people.len {
            if w.people.is_dead(i) {
                continue;
            }
            let d = (w.people.pos(i) - ob.pos).len();
            dist += d as f64;
            if d < ob.radius {
                near += 1;
            }
            if w.people.vel(i).len() > 0.3 {
                moving += 1;
            }
        }
    }
    // Flow health for marching agents.
    let (mut dir, mut arrived, mut missing, mut speed, mut n) = (0, 0, 0, 0.0f64, 0);
    let mut rec = Vec::with_capacity(64);
    for i in 0..w.people.len {
        if w.people.is_dead(i)
            || w.people.stat[i][riot_sim::state::stat::ACTION]
                != riot_sim::state::Action::March as u8
        {
            continue;
        }
        rec.clear();
        match riot_sim::flow::sample_flow(&w.flow, &w.map, 0, w.people.pos(i), &mut rec) {
            riot_sim::flow::FlowSample::Dir(_) => dir += 1,
            riot_sim::flow::FlowSample::Arrived => arrived += 1,
            riot_sim::flow::FlowSample::Missing => missing += 1,
        }
        speed += w.people.vel(i).len() as f64;
        n += 1;
    }
    println!(
        "  march flow: dir {dir} arrived {arrived} missing {missing}; mean speed {:.2} m/s",
        speed / n.max(1) as f64
    );
    let mut doctrines = std::collections::BTreeMap::new();
    for sq in &w.police.squads {
        *doctrines.entry(format!("{:?}", sq.doctrine)).or_insert(0) += 1;
    }
    let (mut rad_anger, mut nr) = (0.0f32, 0);
    for i in 0..w.people.len {
        if !w.people.is_dead(i)
            && w.people.stat[i][riot_sim::state::stat::ARCH] == riot_sim::Archetype::Radical as u8
        {
            rad_anger += riot_core::widen_u8(w.people.emo[i][riot_sim::state::emo::ANGER]);
            nr += 1;
        }
    }
    println!(
        "  hazards: emitters {} gas {} smoke {} fire {} fires {} projectiles {} blocked {} hits {}",
        w.hazards.emitters.len(),
        w.hazards.gas_live,
        w.hazards.smoke_live,
        w.hazards.fire_live,
        w.outcome.fires,
        w.hazards.projectiles.len(),
        w.hazards.blocked,
        w.hazards.hits
    );
    let per = |v: &[u32; riot_sim::outcome::HARMS], scale: f32| {
        riot_sim::outcome::Harm::ALL
            .iter()
            .zip(v)
            .filter(|(_, n)| **n > 0)
            .map(|(h, n)| format!("{} {:.0}", h.name(), *n as f32 * scale))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        "  harm (people's worth of health): {}; deaths by: {}",
        per(&w.outcome.harm, 1.0 / 255.0),
        per(&w.outcome.deaths_by, 1.0)
    );
    println!(
        "  police: clusters {:?} violence {:.1} squads {:?}; radical anger {:.2}",
        w.police
            .clusters
            .iter()
            .map(|c| (
                c.count as u32,
                c.radius as u32,
                (c.anger * 100.0) as u32,
                c.threat as u32
            ))
            .collect::<Vec<_>>(),
        w.police.violence,
        doctrines,
        rad_anger / nr.max(1) as f32
    );
    let o = &w.outcome;
    println!(
        "  in objective {near}, mean dist {:.0} m, moving {moving}, flow built {} recomputes {}",
        dist / present.max(1) as f64,
        w.flow.stats.built,
        w.flow.stats.recomputes
    );
    println!(
        "t={:>6} {:>7.3} ms  present {:>6} asleep {:>5}  esc {:?}  arrests {} injuries {} deaths {} home {}  msgs {}  [{}]",
        w.tick,
        ms,
        present,
        asleep,
        w.police.escalation,
        o.arrests,
        o.injuries,
        o.deaths,
        o.went_home,
        w.msgs.count,
        top.join(", ")
    );
}

fn determinism(args: &Args) -> Result<(), String> {
    let sc = scenario::load(args)?;
    let ticks = args.u32("ticks").unwrap_or(600);
    let list: Vec<usize> = args
        .str("list")
        .unwrap_or("1,2,4,8".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if list.is_empty() {
        return Err("--list needs thread counts".into());
    }
    let mut reference: Option<Vec<u64>> = None;
    for &threads in &list {
        let pool = pool(threads);
        let mut w = pool
            .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
            .map_err(|e| e.to_string())?;
        let mut hashes = Vec::with_capacity(ticks as usize);
        let t0 = Instant::now();
        for _ in 0..ticks {
            pool.install(|| w.tick(&[]));
            hashes.push(w.state_hash());
        }
        println!(
            "{threads} threads: {ticks} ticks in {:.2?}, final hash {:016x}",
            t0.elapsed(),
            hashes.last().copied().unwrap_or(0)
        );
        match &reference {
            None => reference = Some(hashes),
            Some(r) => {
                if let Some(t) = r.iter().zip(&hashes).position(|(a, b)| a != b) {
                    return Err(format!("hash differs from {} threads at tick {t}", list[0]));
                }
            }
        }
    }
    println!("identical state hash on every tick at {list:?} threads");
    Ok(())
}

/// One line per tick: two builds (say x86-64-v3 and baseline x86-64) must
/// print the same file.
fn hashes(args: &Args) -> Result<(), String> {
    use std::io::Write;
    let sc = scenario::load(args)?;
    let ticks = args.u32("ticks").unwrap_or(1200);
    let threads = args.usize("threads").unwrap_or_else(default_threads);
    let pool = pool(threads);
    let hashes = pool.install(|| -> Result<Vec<u64>, String> {
        let mut w = World::new(&sc.setup, &sc.map, sc.tunables).map_err(|e| e.to_string())?;
        Ok((0..ticks)
            .map(|_| {
                w.tick(&[]);
                w.state_hash()
            })
            .collect())
    })?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for (t, h) in hashes.iter().enumerate() {
        writeln!(out, "{} {h:016x}", t + 1).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn resolve(args: &Args) -> Result<(), String> {
    let sc = scenario::load(args)?;
    let threads = args.usize("threads").unwrap_or_else(default_threads);
    let pool = pool(threads);
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    w.snap_cfg.enabled = false;
    if let Some(t) = args.u32("ticks") {
        w.setup.duration = t;
    }
    let t0 = Instant::now();
    let o = pool.install(|| w.resolve());
    let el = t0.elapsed();
    println!(
        "auto-resolved {} ticks ({:.1} simulated minutes) in {el:.2?}",
        o.ticks,
        o.ticks as f32 / 1200.0
    );
    println!("  turnout        {}", o.turnout);
    println!("  peak crowd     {}", o.peak_crowd);
    println!(
        "  ground held    {:.1}% {:?}",
        o.ground_held * 100.0,
        o.objective_held
    );
    println!("  arrests        {}", o.arrests);
    println!(
        "  injuries       {} (+{} officers)",
        o.injuries, o.officers_injured
    );
    println!("  deaths         {}", o.deaths);
    println!("  went home      {}", o.went_home);
    println!("  final morale   {:.2}", o.final_morale);
    println!("  escalation     {:?}", o.max_escalation);
    println!("  supplies used  {:?}", o.supplies_used);
    println!(
        "  footage        movement blame {:.1} reach {:.1} clips {} | police blame {:.1} reach {:.1} clips {}",
        o.footage.movement.blame,
        o.footage.movement.reach,
        o.footage.movement.clips,
        o.footage.police.blame,
        o.footage.police.reach,
        o.footage.police.clips
    );
    println!("  notable        {} events", o.notable.len());
    for n in o.notable.iter().take(20) {
        println!(
            "    t={:>6} {:?} at ({:.0}, {:.0})",
            n.tick, n.kind, n.pos[0], n.pos[1]
        );
    }
    println!("  state hash     {:016x}", o.state_hash);
    Ok(())
}
