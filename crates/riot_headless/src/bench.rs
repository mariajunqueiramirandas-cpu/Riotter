//! Per-phase benchmarks (DESIGN §19): p50 and p99 ms/tick per phase, checked
//! against the budget table (DESIGN §3.1). Timers wrap phases from outside,
//! so they cannot affect results.

use std::fmt::Write as _;
use std::time::Instant;

use riot_sim::world::{PHASES, Phase, Profiler};
use riot_sim::{World, testkit};

use crate::args::Args;

#[derive(Default)]
pub struct Timer {
    start: Option<Instant>,
    pub current: [f64; PHASES],
}

impl Profiler for Timer {
    fn begin(&mut self, _phase: Phase) {
        self.start = Some(Instant::now());
    }
    fn end(&mut self, phase: Phase) {
        if let Some(s) = self.start.take() {
            self.current[phase as usize] = s.elapsed().as_secs_f64() * 1000.0;
        }
    }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Run `ticks` ticks after `warmup`, returning per-tick per-phase ms.
pub fn measure(
    w: &mut World,
    pool: &rayon::ThreadPool,
    warmup: u32,
    ticks: u32,
) -> Vec<[f64; PHASES]> {
    // One `install` for the whole run: the simulation thread stays inside the
    // pool, as it does in the app (DESIGN §12 thread pool).
    pool.install(|| {
        for _ in 0..warmup {
            w.tick(&[]);
        }
        let mut out = Vec::with_capacity(ticks as usize);
        let mut timer = Timer::default();
        for _ in 0..ticks {
            measured_tick(w, &mut timer);
            out.push(timer.current);
        }
        out
    })
}

/// One measured tick. Not inlined, so a profiler can collect only these:
/// `valgrind --tool=callgrind --collect-atstart=no --toggle-collect='*measured_tick*'`.
#[inline(never)]
fn measured_tick(w: &mut World, timer: &mut Timer) {
    w.tick_profiled(&[], timer);
}

pub fn bench(args: &Args) -> Result<(), String> {
    let sc = crate::scenario::load(args)?;
    let threads = args.usize("threads").unwrap_or_else(crate::default_threads);
    let warmup = args.u32("warmup").unwrap_or(200);
    let ticks = args.u32("ticks").unwrap_or(400);
    let pool = crate::pool(threads);
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    w.snap_cfg.enabled = !args.flag("no-snapshot");
    let rows = measure(&mut w, &pool, warmup, ticks);
    let agents = w.present();
    // The budget is for 10,000 agents; scale it for other crowd sizes.
    let scale = (agents as f64 / 10_000.0).max(0.2);
    let mut csv = String::from("phase,p50_ms,p99_ms,mean_ms,budget_ms\n");
    println!(
        "{agents} agents ({} asleep), {threads} threads, {ticks} ticks after {warmup} warm-up",
        w.snapshot.stats.asleep
    );
    println!(
        "{:<10} {:>8} {:>8} {:>8} {:>8}",
        "phase", "p50", "p99", "mean", "budget"
    );
    let mut over = Vec::new();
    let mut totals: Vec<f64> = rows.iter().map(|r| r.iter().sum()).collect();
    for ph in Phase::ALL {
        let mut v: Vec<f64> = rows.iter().map(|r| r[ph as usize]).collect();
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        let p50 = percentile(&mut v, 0.5);
        let p99 = percentile(&mut v, 0.99);
        let budget = ph.budget_ms() as f64 * scale;
        let mark = if p50 > budget * 1.1 && budget > 0.0 {
            " over"
        } else {
            ""
        };
        println!(
            "{:<10} {:>8.3} {:>8.3} {:>8.3} {:>8.3}{mark}",
            ph.name(),
            p50,
            p99,
            mean,
            budget
        );
        let _ = writeln!(csv, "{},{p50:.4},{p99:.4},{mean:.4},{budget:.4}", ph.name());
        if !mark.is_empty() {
            over.push(ph.name());
        }
    }
    let mean = totals.iter().sum::<f64>() / totals.len().max(1) as f64;
    let p50 = percentile(&mut totals, 0.5);
    let p99 = percentile(&mut totals, 0.99);
    println!(
        "{:<10} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
        "total",
        p50,
        p99,
        mean,
        3.0 * scale
    );
    let _ = writeln!(csv, "total,{p50:.4},{p99:.4},{mean:.4},{:.4}", 3.0 * scale);
    if let Some(path) = args.str("csv") {
        std::fs::write(&path, csv).map_err(|e| format!("{path}: {e}"))?;
        println!("wrote {path}");
    }
    if args.flag("budget") && !over.is_empty() {
        return Err(format!("over budget: {}", over.join(", ")));
    }
    Ok(())
}

/// Map size is free (DESIGN §3, §19): the same crowd on a 512 m and an
/// 8192 m map must cost the same per tick, within 5%.
pub fn mapsize(args: &Args) -> Result<(), String> {
    let people = args.u32("people").unwrap_or(10_000);
    let threads = args.usize("threads").unwrap_or_else(crate::default_threads);
    let warmup = args.u32("warmup").unwrap_or(600);
    let ticks = args.u32("ticks").unwrap_or(400);
    let pool = crate::pool(threads);
    let mut results = Vec::new();
    for size in [512.0f32, 8192.0] {
        let t0 = Instant::now();
        let map = testkit::district(size, 64.0);
        let setup = testkit::central(&map, people, 7);
        let mut w = pool
            .install(|| World::new(&setup, &map, Default::default()))
            .map_err(|e| e.to_string())?;
        let built = t0.elapsed();
        w.snap_cfg.enabled = false;
        let rows = measure(&mut w, &pool, warmup, ticks);
        let mut totals: Vec<f64> = rows.iter().map(|r| r.iter().sum()).collect();
        let p50 = percentile(&mut totals, 0.5);
        println!(
            "{size:>6} m map: built in {built:.2?}, {} agents, p50 {p50:.3} ms/tick",
            w.present()
        );
        results.push(p50);
    }
    let ratio = results[1] / results[0];
    println!("8 km / 0.5 km cost ratio: {ratio:.3}");
    if (ratio - 1.0).abs() > 0.05 && args.flag("strict") {
        return Err(format!(
            "map size changed per-tick cost by {:.1}%",
            (ratio - 1.0) * 100.0
        ));
    }
    Ok(())
}
