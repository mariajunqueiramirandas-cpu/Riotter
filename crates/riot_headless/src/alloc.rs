//! Zero heap allocations per tick (DESIGN §16): a counting global allocator
//! runs ticks after warm-up and requires exactly zero allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use riot_sim::World;

use crate::args::Args;

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
/// Print a backtrace for allocations while set (`--trace`).
static TRACE: AtomicBool = AtomicBool::new(false);
static TRACED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static IN_TRACE: Cell<bool> = const { Cell::new(false) };
}

fn trace(size: usize) {
    if !TRACE.load(Ordering::Relaxed) || TRACED.load(Ordering::Relaxed) >= 3 {
        return;
    }
    IN_TRACE.with(|t| {
        if t.replace(true) {
            return;
        }
        TRACED.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "allocation of {size} bytes:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        t.set(false);
    });
}

// SAFETY: forwards to the system allocator, only counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        trace(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        trace(new_size);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

pub fn allocations() -> (u64, u64) {
    (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

pub fn check(args: &Args) -> Result<(), String> {
    let sc = crate::scenario::load(args)?;
    let warmup = args.u32("warmup").unwrap_or(1200);
    let ticks = args.u32("ticks").unwrap_or(1000);
    let threads = args.usize("threads").unwrap_or_else(crate::default_threads);
    let pool = crate::pool(threads);
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    for _ in 0..warmup {
        pool.install(|| w.tick(&[]));
    }
    // Tick inside one `install`, so rayon's injector (which grows a block
    // every few dozen injected jobs) is not counted against the simulation.
    let trace_on = args.flag("trace");
    let (a0, b0) = allocations();
    let worst = pool.install(|| {
        let mut worst: Option<(u32, u64)> = None;
        for _ in 0..ticks {
            let (before, _) = allocations();
            TRACE.store(trace_on, Ordering::Relaxed);
            w.tick(&[]);
            TRACE.store(false, Ordering::Relaxed);
            let (after, _) = allocations();
            if after > before && worst.is_none_or(|x| after - before > x.1) {
                worst = Some((w.tick - 1, after - before));
            }
        }
        worst
    });
    let (a1, b1) = allocations();
    println!(
        "{} allocations ({} bytes) in {ticks} ticks after {warmup} warm-up ticks",
        a1 - a0,
        b1 - b0
    );
    if let Some((t, n)) = worst {
        return Err(format!(
            "not allocation-free: worst tick {t} made {n} allocations"
        ));
    }
    println!("zero allocations per tick");
    Ok(())
}
