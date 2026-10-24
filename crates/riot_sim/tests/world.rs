use riot_sim::testkit;
use riot_sim::{Tunables, World};

#[test]
fn a_march_runs() {
    let map = testkit::district(1024.0, 64.0);
    let setup = testkit::march(&map, 2000, 8, 7);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    let t0 = std::time::Instant::now();
    for _ in 0..2400 {
        w.tick(&[]);
    }
    let el = t0.elapsed();
    let s = &w.snapshot.stats;
    eprintln!(
        "{:?} per tick; present {} protesters {} officers {} asleep {} fear {:.2} anger {:.2} morale {:.2} esc {} arrests {} inj {} msgs {}",
        el / 2400,
        s.present,
        s.protesters,
        s.officers,
        s.asleep,
        s.mean_fear,
        s.mean_anger,
        s.mean_morale,
        s.escalation,
        s.arrests,
        s.injuries,
        s.messages
    );
    let o = &w.objectives[0];
    let near = (0..w.people.len)
        .filter(|&i| !w.people.is_dead(i) && (w.people.pos(i) - o.pos).len() < o.radius * 1.5)
        .count();
    eprintln!(
        "near square: {near}; flow {:?}; fields active {}",
        w.flow.stats, w.fields.active
    );
    assert!(s.present > 1500);
    assert!(near > 500, "only {near} reached the square");
}
