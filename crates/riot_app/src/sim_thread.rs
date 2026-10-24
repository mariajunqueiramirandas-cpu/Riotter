//! The simulation thread (DESIGN §2.8, §4.2, §12).
//!
//! The `World` lives on its own thread and ticks on a dedicated rayon pool
//! of (cores − 1) threads, leaving the main thread free for input and
//! rendering. A fixed-tick accumulator runs 20 ticks per real second times
//! the game speed; speed never changes the tick length. Every input reaches
//! the world as a command stamped with the tick it is applied on.
//!
//! Snapshots leave through a triple buffer: the sim copies `world.snapshot`
//! into its own back buffer (outside any lock), then swaps it with the
//! shared middle buffer; the renderer swaps the middle buffer out when it is
//! fresh. Only pointer swaps happen under the lock, and every buffer is
//! reused, so steady state allocates nothing.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use riot_sim::world::PHASES;
use riot_sim::{Command, CommandKind, Overlay, Phase, Profiler, Snapshot, TICK_HZ, World};

/// Messages to the simulation thread.
#[derive(Clone, Debug)]
pub enum ToSim {
    /// Apply on the next tick.
    Cmd(CommandKind),
    /// Apply on a given tick (or the next one, if that has passed).
    At(u32, CommandKind),
    /// Game speed: 1, 2, 4 or 8 times 20 ticks per second.
    Speed(u32),
    Pause(bool),
    /// Run one tick while paused.
    Step,
    /// Run this many ticks as fast as possible, then resume pacing.
    FastForward(u32),
    /// Field layer to capture for the debug overlay (render only).
    Overlay(Overlay),
    Quit,
}

/// What the sim thread reports to the HUD.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimStatus {
    /// The world's next tick.
    pub tick: u32,
    /// Tick of the last published snapshot.
    pub published: Option<u32>,
    pub paused: bool,
    pub speed: u32,
    /// Ticks still to run headlessly.
    pub fast_forward: u32,
    /// Mean wall time per tick (ms) over the last half second.
    pub tick_ms: f32,
    /// Ticks run per real second, measured.
    pub ticks_per_sec: f32,
    /// The sim could not keep up with the game speed.
    pub behind: bool,
    /// Mean ms per phase over the last half second.
    pub phase_ms: [f32; PHASES],
    /// Commands waiting for the next tick.
    pub queued: usize,
    pub threads: usize,
    pub finished: bool,
}

struct Mailbox {
    snap: Snapshot,
    produced: Instant,
    fresh: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    status: Mutex<SimStatus>,
}

/// The main thread's handle on the simulation thread.
pub struct SimHandle {
    tx: Sender<ToSim>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl SimHandle {
    /// Move `world` onto a new thread that ticks it on a pool of `threads`.
    pub fn spawn(world: World, threads: usize, speed: u32, paused: bool) -> SimHandle {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                snap: Snapshot::default(),
                produced: Instant::now(),
                fresh: false,
            }),
            status: Mutex::new(SimStatus {
                paused,
                speed,
                threads,
                ..Default::default()
            }),
        });
        let sh = shared.clone();
        let join = std::thread::Builder::new()
            .name("sim".into())
            .spawn(move || {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .thread_name(|i| format!("sim-pool-{i}"))
                    .build()
                    .expect("simulation thread pool");
                SimLoop::new(world, rx, sh, speed, paused).run(&pool);
            })
            .expect("simulation thread");
        SimHandle {
            tx,
            shared,
            join: Some(join),
        }
    }

    /// Send a message to the sim thread.
    pub fn send(&self, m: ToSim) {
        // The sim thread only exits on Quit; a send error means it panicked,
        // which `status` and the missing snapshots already show.
        let _ = self.tx.send(m);
    }

    /// Queue a command for the next tick.
    pub fn command(&self, k: CommandKind) {
        self.send(ToSim::Cmd(k));
    }

    /// Swap the newest snapshot into `into` if there is one; returns when it
    /// was produced.
    pub fn take_snapshot(&self, into: &mut Snapshot) -> Option<Instant> {
        let mut m = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !m.fresh {
            return None;
        }
        m.fresh = false;
        std::mem::swap(into, &mut m.snap);
        Some(m.produced)
    }

    /// The latest status report.
    pub fn status(&self) -> SimStatus {
        *self.shared.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the sim thread is still alive.
    pub fn alive(&self) -> bool {
        self.join.as_ref().is_some_and(|j| !j.is_finished())
    }
}

impl Drop for SimHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(ToSim::Quit);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The renderer's side: the two most recent snapshots received and when
/// the sim produced them, for interpolation.
#[derive(Default)]
pub struct Snapshots {
    pub prev: Snapshot,
    pub cur: Snapshot,
    pub has_prev: bool,
    pub has_cur: bool,
    incoming: Snapshot,
    t_prev: Option<Instant>,
    t_cur: Option<Instant>,
}

impl Snapshots {
    /// Take the newest snapshot from the sim, if any. Buffers rotate, so
    /// nothing is allocated once they have grown.
    pub fn receive(&mut self, sim: &SimHandle) -> bool {
        let Some(t) = sim.take_snapshot(&mut self.incoming) else {
            return false;
        };
        std::mem::swap(&mut self.prev, &mut self.cur);
        std::mem::swap(&mut self.cur, &mut self.incoming);
        self.has_prev = self.has_cur;
        self.has_cur = true;
        self.t_prev = self.t_cur;
        self.t_cur = Some(t);
        true
    }

    /// The previous snapshot, once there are two.
    pub fn prev(&self) -> Option<&Snapshot> {
        self.has_prev.then_some(&self.prev)
    }

    /// Interpolation from `prev` (0) to `cur` (1). Rendering runs one
    /// snapshot interval behind the sim, so motion is smooth at any speed.
    pub fn alpha(&self, now: Instant) -> f32 {
        match (self.t_prev, self.t_cur) {
            (Some(p), Some(c)) => {
                // Clamp the interval so a long pause does not slow the
                // first frames after it.
                let span = (c - p).as_secs_f32().clamp(0.001, 0.1);
                ((now - c).as_secs_f32() / span).clamp(0.0, 1.0)
            }
            _ => 1.0,
        }
    }
}

/// Wall-clock phase timers. They live here, outside the simulation logic,
/// so they cannot affect results (DESIGN §19).
struct PhaseTimer {
    t0: Instant,
    acc: [f64; PHASES],
}

impl Profiler for PhaseTimer {
    fn begin(&mut self, _phase: Phase) {
        self.t0 = Instant::now();
    }
    fn end(&mut self, phase: Phase) {
        self.acc[phase as usize] += self.t0.elapsed().as_secs_f64();
    }
}

struct SimLoop {
    world: World,
    rx: Receiver<ToSim>,
    shared: Arc<Shared>,
    back: Snapshot,
    speed: u32,
    paused: bool,
    quit: bool,
    steps: u32,
    fast_forward: u32,
    /// Commands for the next tick, and commands for later ticks.
    pending: Vec<CommandKind>,
    scheduled: Vec<(u32, CommandKind)>,
    cmds: Vec<Command>,
    timer: PhaseTimer,
    /// Accumulated ticks owed to the game clock.
    acc: f64,
    behind: bool,
    // Measurement window.
    window_start: Instant,
    window_ticks: u32,
    window_time: f64,
}

impl SimLoop {
    fn new(
        world: World,
        rx: Receiver<ToSim>,
        shared: Arc<Shared>,
        speed: u32,
        paused: bool,
    ) -> SimLoop {
        SimLoop {
            world,
            rx,
            shared,
            back: Snapshot::default(),
            speed,
            paused,
            quit: false,
            steps: 0,
            fast_forward: 0,
            pending: Vec::new(),
            scheduled: Vec::new(),
            cmds: Vec::new(),
            timer: PhaseTimer {
                t0: Instant::now(),
                acc: [0.0; PHASES],
            },
            acc: 0.0,
            behind: false,
            window_start: Instant::now(),
            window_ticks: 0,
            window_time: 0.0,
        }
    }

    fn handle(&mut self, m: ToSim) {
        match m {
            ToSim::Cmd(k) => self.pending.push(k),
            ToSim::At(t, k) => self.scheduled.push((t, k)),
            ToSim::Speed(s) => self.speed = s.clamp(1, 8),
            ToSim::Pause(p) => {
                self.paused = p;
                self.acc = 0.0;
            }
            ToSim::Step => self.steps += 1,
            ToSim::FastForward(n) => self.fast_forward += n,
            ToSim::Overlay(o) => self.world.snap_cfg.overlay = o,
            ToSim::Quit => self.quit = true,
        }
    }

    /// Drain the channel; false once the main thread is gone.
    fn drain(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(m) => self.handle(m),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn tick(&mut self, pool: &rayon::ThreadPool) {
        let now = self.world.tick;
        self.cmds.clear();
        for k in self.pending.drain(..) {
            self.cmds.push(Command::at(now, k));
        }
        let mut i = 0;
        while i < self.scheduled.len() {
            if self.scheduled[i].0 <= now {
                let (_, k) = self.scheduled.remove(i);
                self.cmds.push(Command::at(now, k));
            } else {
                i += 1;
            }
        }
        let t0 = Instant::now();
        let (world, cmds, timer) = (&mut self.world, &self.cmds, &mut self.timer);
        pool.install(|| world.tick_profiled(cmds, timer));
        self.window_time += t0.elapsed().as_secs_f64();
        self.window_ticks += 1;
    }

    fn publish(&mut self) {
        self.back.copy_from(&self.world.snapshot);
        let mut m = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::swap(&mut m.snap, &mut self.back);
        m.produced = Instant::now();
        m.fresh = true;
    }

    fn report(&mut self, published: bool) {
        let mut s = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        s.tick = self.world.tick;
        if published {
            s.published = Some(self.world.snapshot.tick);
        }
        s.paused = self.paused;
        s.speed = self.speed;
        s.fast_forward = self.fast_forward;
        s.behind = self.behind;
        s.queued = self.pending.len() + self.scheduled.len();
        s.finished = self.world.finished();
        let el = self.window_start.elapsed().as_secs_f64();
        if el >= 0.5 {
            s.ticks_per_sec = (self.window_ticks as f64 / el) as f32;
            // While paused, keep showing the last measured costs.
            if self.window_ticks > 0 {
                let n = self.window_ticks as f64;
                s.tick_ms = (self.window_time / n * 1000.0) as f32;
                for (o, a) in s.phase_ms.iter_mut().zip(&mut self.timer.acc) {
                    *o = (*a / n * 1000.0) as f32;
                    *a = 0.0;
                }
            }
            self.window_start = Instant::now();
            self.window_ticks = 0;
            self.window_time = 0.0;
        }
    }

    fn run(mut self, pool: &rayon::ThreadPool) {
        let rate = TICK_HZ as f64;
        let mut last = Instant::now();
        loop {
            if !self.drain() || self.quit {
                return;
            }
            let now = Instant::now();
            let dt = (now - last).as_secs_f64();
            last = now;
            let mut ran = 0;
            if self.fast_forward > 0 {
                // Headless: as fast as possible, publishing every batch.
                let n = self.fast_forward.min(32);
                for _ in 0..n {
                    self.tick(pool);
                }
                self.fast_forward -= n;
                ran += n;
                last = Instant::now();
            } else {
                if !self.paused {
                    self.acc += dt * rate * self.speed as f64;
                }
                // Never owe more than a quarter second of ticks: when the sim
                // cannot keep up, the game slows down instead of spiralling.
                let cap = (rate * self.speed as f64 * 0.25).max(2.0);
                self.behind = self.acc > cap;
                self.acc = self.acc.min(cap);
                while self.acc >= 1.0 {
                    self.tick(pool);
                    self.acc -= 1.0;
                    ran += 1;
                }
                while self.steps > 0 {
                    self.tick(pool);
                    self.steps -= 1;
                    ran += 1;
                }
            }
            if ran > 0 {
                self.publish();
            }
            self.report(ran > 0);
            if self.fast_forward > 0 {
                continue;
            }
            // Sleep until the next tick is due or a message arrives.
            let wait = if self.paused {
                Duration::from_millis(50)
            } else {
                let secs = (1.0 - self.acc).max(0.0) / (rate * self.speed as f64);
                Duration::from_secs_f64(secs.clamp(0.0005, 0.05))
            };
            match self.rx.recv_timeout(wait) {
                Ok(m) => self.handle(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riot_sim::{Tunables, testkit};

    fn small_world() -> World {
        let map = testkit::district(512.0, 64.0);
        let setup = testkit::march(&map, 200, 2, 3);
        World::new(&setup, &map, Tunables::default()).unwrap()
    }

    #[test]
    fn fast_forward_publishes_and_commands_are_stamped() {
        let sim = SimHandle::spawn(small_world(), 2, 1, true);
        sim.command(CommandKind::SetStance(riot_sim::Stance::Confrontational));
        sim.send(ToSim::FastForward(40));
        let t0 = Instant::now();
        let mut snap = Snapshot::default();
        loop {
            let s = sim.status();
            if s.fast_forward == 0 && s.published == Some(39) {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "fast-forward stalled"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sim.take_snapshot(&mut snap).is_some());
        assert_eq!(snap.tick, 39);
        assert!(!snap.people.is_empty());
        // Paused: nothing more runs.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sim.status().tick, 40);
        sim.send(ToSim::Step);
        let t0 = Instant::now();
        while sim.status().tick != 41 {
            assert!(t0.elapsed() < Duration::from_secs(10), "step stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Run `n` ticks and wait for their snapshot.
    fn run_ticks(sim: &SimHandle, snaps: &mut Snapshots, n: u32) {
        let target = sim.status().tick + n;
        sim.send(ToSim::FastForward(n));
        let t0 = Instant::now();
        while sim.status().published != Some(target - 1) || sim.status().fast_forward > 0 {
            assert!(t0.elapsed() < Duration::from_secs(60), "ticks stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(snaps.receive(sim));
        assert_eq!(snaps.cur.tick, target - 1);
    }

    #[test]
    // Needs riot_sim to snapshot every spawned stable id (fixed on main:
    // `World::issued` now follows scheduled arrivals), so organizers show.
    fn player_verbs_reach_the_world() {
        use crate::input::{Input, Verb};
        use riot_render::RenderOptions;
        use riot_sim::setup::ObjectiveKind;

        let world = small_world();
        let road = world.map.roads.nodes[3];
        let sim = SimHandle::spawn(world, 2, 1, true);
        let mut snaps = Snapshots::default();
        run_ticks(&sim, &mut snaps, 60);
        let messages = snaps.cur.stats.messages;

        let mut input = Input::new(1, true, Overlay::None);
        let mut opts = RenderOptions::default();
        input.cursor_world = macroquad::prelude::vec2(road.x, road.y);
        let mut verbs = vec![
            Verb::NextSlot,
            Verb::Tool(ObjectiveKind::Hold),
            Verb::Place,
            Verb::DropSupply,
            Verb::CycleStance,
        ];
        verbs.extend([Verb::SelectOrganizer, Verb::Chant]);
        for v in verbs {
            for m in input.act(v, &snaps.cur, &mut opts, 0.0) {
                sim.send(m);
            }
        }
        assert!(input.selected.is_some(), "an organizer has arrived by now");
        run_ticks(&sim, &mut snaps, 2);
        let o = snaps.cur.objectives.iter().find(|o| o.slot == 1).unwrap();
        assert_eq!(o.kind, ObjectiveKind::Hold as u8);
        assert!(snaps.cur.drops.iter().any(|d| d.amount == 20));

        // The objective now exists, so a broadcast starts a new message.
        for m in input.act(Verb::Broadcast, &snaps.cur, &mut opts, 0.0) {
            sim.send(m);
        }
        run_ticks(&sim, &mut snaps, 2);
        assert!(snaps.cur.stats.messages > messages, "no new message");
    }

    #[test]
    fn game_speed_scales_ticks_per_second() {
        let sim = SimHandle::spawn(small_world(), 2, 4, false);
        std::thread::sleep(Duration::from_millis(1000));
        let ticks = sim.status().tick;
        // 4× is 80 ticks per second; allow slack for a busy machine.
        assert!((50..=90).contains(&ticks), "{ticks} ticks in 1 s at 4×");
    }
}
