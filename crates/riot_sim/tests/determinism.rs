//! The same inputs give the same state on every tick whatever the thread
//! count (DESIGN §3.1): work is split into fixed chunks and reduced in a
//! fixed order, never by how rayon happens to schedule it.

mod common;

use riot_sim::testkit;
use riot_sim::{MapDef, RiotSetup, Tunables, World};

fn hashes(map: &MapDef, setup: &RiotSetup, threads: usize, ticks: u32, script_at: u32) -> Vec<u64> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let script = common::script(map, script_at);
    pool.install(|| {
        let mut w = World::new(setup, map, Tunables::default()).unwrap();
        let h = (0..ticks)
            .map(|_| {
                common::step(&mut w, &script);
                w.state_hash()
            })
            .collect();
        if threads == 1 {
            let s = &w.snapshot.stats;
            eprintln!(
                "after {ticks} ticks: {} present, escalation {}, {} arrests, {} injuries, \
                 {} projectiles, {} messages",
                s.present,
                s.escalation,
                s.arrests,
                s.injuries,
                w.snapshot.projectiles.len(),
                s.messages
            );
        }
        h
    })
}

fn same_on_every_thread_count(map: &MapDef, setup: &RiotSetup, ticks: u32, script_at: u32) {
    let reference = hashes(map, setup, 1, ticks, script_at);
    for threads in [2, 3, 4] {
        let h = hashes(map, setup, threads, ticks, script_at);
        if let Some(t) = reference.iter().zip(&h).position(|(a, b)| a != b) {
            panic!("{threads} threads diverge from 1 thread at tick {t}");
        }
    }
}

#[test]
fn a_violent_square_is_the_same_on_any_thread_count() {
    let (map, setup) = common::violent(1200);
    same_on_every_thread_count(&map, &setup, 1000, 500);
}

#[test]
fn a_march_is_the_same_on_any_thread_count() {
    let map = testkit::district(1024.0, 64.0);
    let setup = testkit::march(&map, 3000, 8, 4);
    same_on_every_thread_count(&map, &setup, 600, u32::MAX);
}
