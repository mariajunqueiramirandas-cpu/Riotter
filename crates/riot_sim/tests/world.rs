use riot_sim::testkit;
use riot_sim::{Tunables, World};

#[test]
fn a_crowd_reaches_the_square() {
    let map = testkit::district(512.0, 64.0);
    let setup = testkit::central(&map, 1500, 7);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    for _ in 0..3600 {
        w.tick(&[]);
    }
    let o = w.objectives[0];
    let near = (0..w.people.len)
        .filter(|&i| !w.people.is_dead(i) && (w.people.pos(i) - o.pos).len() < o.radius * 1.2)
        .count();
    assert!(w.snapshot.stats.present > 1400);
    assert!(near > 900, "only {near} reached the square");
}

#[test]
fn snapshot_covers_everyone_and_debug_spawns_get_fresh_ids() {
    use riot_sim::{Archetype, Command, CommandKind, DebugCommand};
    let map = testkit::district(512.0, 64.0);
    let setup = testkit::central(&map, 300, 3);
    let mut w = World::new(&setup, &map, Tunables::default()).unwrap();
    for _ in 0..1300 {
        w.tick(&[]);
    }
    let present = w
        .snapshot
        .people
        .iter()
        .filter(|p| p.flags & riot_sim::snapshot::snap_flag::PRESENT != 0)
        .count();
    assert_eq!(present as u32, w.snapshot.stats.present);
    let before = w.issued;
    let spawn = Command::at(
        w.tick,
        CommandKind::Debug(DebugCommand::Spawn {
            x: 0.0,
            y: 0.0,
            count: 20,
            archetype: Archetype::Student,
            radius: 5.0,
        }),
    );
    w.tick(&[spawn]);
    assert_eq!(w.issued, w.planned + 20);
    assert!(w.issued > before);
    let present = w
        .snapshot
        .people
        .iter()
        .filter(|p| p.flags & riot_sim::snapshot::snap_flag::PRESENT != 0)
        .count();
    assert_eq!(present as u32, w.snapshot.stats.present);
}
