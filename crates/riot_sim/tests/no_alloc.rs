//! A warmed-up world ticks without touching the heap (DESIGN §16): every
//! buffer is sized from the scenario's limits when the world is built.
//!
//! Only allocations made by the test thread and the simulation's own rayon
//! workers count, so the test harness cannot disturb the result.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use riot_sim::{Tunables, World};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTED: Cell<bool> = const { Cell::new(false) };
}

fn count() {
    if COUNTED.try_with(Cell::get).unwrap_or(false) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards to the system allocator, only counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn ticks_do_not_allocate() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .start_handler(|_| COUNTED.with(|c| c.set(true)))
        .build()
        .unwrap();
    let (map, setup) = common::violent(1500);
    let script = common::script(&map, 900);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    // Tick inside one `install`: rayon's injector grows a block every few
    // dozen injected jobs, which is rayon's allocation, not the world's.
    let worst = pool.install(|| {
        // Warm up past the arrivals, the escalation, gas and fire, so every
        // pool and lazily built flow tile has been touched once.
        for _ in 0..1500 {
            common::step(&mut w, &script);
        }
        COUNTED.with(|c| c.set(true));
        let mut worst = (0, 0);
        for _ in 0..800 {
            let before = ALLOCS.load(Ordering::Relaxed);
            common::step(&mut w, &script);
            let n = ALLOCS.load(Ordering::Relaxed) - before;
            if n > worst.1 {
                worst = (w.tick - 1, n);
            }
        }
        COUNTED.with(|c| c.set(false));
        worst
    });
    assert!(
        worst.1 == 0,
        "tick {} made {} heap allocations (run `riot_headless alloc --trace` to see where)",
        worst.0,
        worst.1
    );
    let s = &w.snapshot.stats;
    assert!(s.present > 1000, "the scenario should still be busy");
}
