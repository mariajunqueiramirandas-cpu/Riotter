//! A fork-join team for the tick (DESIGN §12): a fixed set of threads that
//! run "for every chunk index, do this" and then join.
//!
//! Rayon wakes idle workers through the OS scheduler, which on some machines
//! (cloud VMs in particular) costs around 100 µs every time a parallel loop
//! follows a stretch of serial work, and a tick has a few dozen of those.
//! Team workers spin for a while after each job before they park, so a job
//! that follows soon after the previous one starts in a few microseconds.
//!
//! Work is handed out by index and every index runs exactly once, so the
//! result of a job never depends on how many threads ran it or which thread
//! ran what: the simulation stays deterministic.
//!
//! This crate, like rayon's internals, is where the simulation's atomics and
//! unsafe code live; simulation code only calls [`Team::run`] and
//! [`Team::for_chunks`].

use core::any::Any;
use core::marker::PhantomData;
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

/// How long an idle worker spins for the next job before it parks.
const SPIN: Duration = Duration::from_millis(2);

type Job = dyn Fn(usize) + Sync;
type Panic = Box<dyn Any + Send>;

/// Bits of the claim word: job epoch, task count, next task index.
const NEXT_BITS: u32 = 18;
const TASK_BITS: u32 = 18;
const TAG_SHIFT: u32 = NEXT_BITS + TASK_BITS;
const FIELD: u64 = (1 << NEXT_BITS) - 1;
const EPOCH_MASK: u64 = (1 << (64 - TAG_SHIFT)) - 1;
/// Most tasks one claim word hands out; [`Team::run`] groups bigger jobs.
const MAX_TASKS: usize = (1 << TASK_BITS) - 1;

#[inline]
fn claim_word(epoch: u64, tasks: usize) -> u64 {
    ((epoch & EPOCH_MASK) << TAG_SHIFT) | ((tasks as u64) << NEXT_BITS)
}

struct Shared {
    /// Bumped for every job; idle workers watch it to wake up.
    epoch: AtomicU64,
    /// `epoch << 36 | tasks << 18 | next`. A task is taken by a CAS on the
    /// whole word, so a thread that read it during an earlier job can only
    /// take an index that is free in the current one.
    claim: AtomicU64,
    /// The current job. Read only by a thread that has just claimed one of
    /// its tasks.
    job: UnsafeCell<Option<*const Job>>,
    /// Tasks of the current job that have finished.
    done: AtomicUsize,
    parked: Vec<AtomicBool>,
    /// A task of the current job panicked; its payload is in `panic`.
    panicked: AtomicBool,
    panic: Mutex<Option<Panic>>,
    stop: AtomicBool,
}

// SAFETY: `job` is written only by the thread running `Team::run`, and only
// while no task is claimed: before it publishes the job's claim word
// (Release), when every task of the previous job has finished, and after
// every task of this job has finished. In between it is read only by
// threads that claimed a task, with an Acquire CAS on a value the
// publishing store heads, and `run` waits for each of those tasks.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// A team of `threads` threads: the caller of [`Team::run`] plus
/// `threads - 1` workers.
pub struct Team {
    shared: Arc<Shared>,
    workers: Vec<(JoinHandle<()>, Thread)>,
    /// Guards against a job starting another job on the same team.
    running: AtomicBool,
}

impl Team {
    /// `start` runs first on each new worker thread (for example to mark
    /// it for a counting allocator).
    pub fn with_start(threads: usize, start: impl Fn() + Send + Sync + 'static) -> Team {
        let n = threads.max(1) - 1;
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(0),
            claim: AtomicU64::new(0),
            job: UnsafeCell::new(None),
            done: AtomicUsize::new(0),
            parked: (0..n).map(|_| AtomicBool::new(false)).collect(),
            panicked: AtomicBool::new(false),
            panic: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let start = Arc::new(start);
        let workers = (0..n)
            .map(|k| {
                let s = shared.clone();
                let start = start.clone();
                let h = thread::Builder::new()
                    .name(format!("riot-team-{k}"))
                    .spawn(move || {
                        start();
                        worker(&s, k);
                    })
                    .expect("spawn team thread");
                let t = h.thread().clone();
                (h, t)
            })
            .collect();
        Team {
            shared,
            workers,
            running: AtomicBool::new(false),
        }
    }

    pub fn new(threads: usize) -> Team {
        Team::with_start(threads, || {})
    }

    pub fn threads(&self) -> usize {
        self.workers.len() + 1
    }

    /// Run `f(k)` for every `k` in `0..tasks`, spread over the team, and
    /// return when all have finished. Must not be called from inside a job.
    ///
    /// Every task runs exactly once, also when some of them panic; the
    /// first panic is then resumed here.
    pub fn run(&self, tasks: usize, f: &(dyn Fn(usize) + Sync)) {
        if tasks == 0 {
            return;
        }
        if self.workers.is_empty() || tasks == 1 {
            for k in 0..tasks {
                f(k);
            }
            return;
        }
        if tasks > MAX_TASKS {
            let per = tasks.div_ceil(MAX_TASKS);
            return self.run(tasks.div_ceil(per), &|g| {
                let mut first = None;
                for k in g * per..((g + 1) * per).min(tasks) {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(k)));
                    first = first.or(r.err());
                }
                if let Some(p) = first {
                    std::panic::resume_unwind(p);
                }
            });
        }
        assert!(
            !self.running.swap(true, Ordering::Acquire),
            "Team::run inside a job, or from two threads at once"
        );
        let s = &*self.shared;
        // SAFETY: every task of the previous job has finished and none of
        // this one can be claimed before the claim word below is published,
        // so nobody reads `job` now. The pointer is used only by threads
        // holding a claimed task, and this function waits for all of them.
        unsafe {
            let p: *const (dyn Fn(usize) + Sync + '_) = f;
            *s.job.get() = Some(core::mem::transmute::<
                *const (dyn Fn(usize) + Sync + '_),
                *const Job,
            >(p));
        }
        s.done.store(0, Ordering::Relaxed);
        let epoch = s.epoch.load(Ordering::Relaxed) + 1;
        s.claim.store(claim_word(epoch, tasks), Ordering::Release);
        // SeqCst, as the worker's `parked` store and epoch re-check are:
        // either it sees this epoch and does not park, or we see it parked.
        s.epoch.store(epoch, Ordering::SeqCst);
        for (k, (_, t)) in self.workers.iter().enumerate() {
            if s.parked[k].load(Ordering::SeqCst) {
                t.unpark();
            }
        }
        work(s, epoch);
        // Wait for the tasks, not for the workers: one that is asleep or
        // descheduled and has claimed nothing does not hold the job up.
        let mut spins = 0u32;
        while s.done.load(Ordering::Acquire) < tasks {
            spins += 1;
            if spins < 1 << 16 {
                core::hint::spin_loop();
            } else {
                thread::yield_now();
            }
        }
        // SAFETY: every task has finished, and no claim of this job can
        // succeed any more: its claim word says all are taken.
        unsafe { *s.job.get() = None };
        let panic = if s.panicked.load(Ordering::Relaxed) {
            s.panicked.store(false, Ordering::Relaxed);
            s.panic.lock().unwrap_or_else(|e| e.into_inner()).take()
        } else {
            None
        };
        self.running.store(false, Ordering::Release);
        if let Some(p) = panic {
            std::panic::resume_unwind(p);
        }
    }

    /// Run `f(c, parts)` for every chunk index `c` in `0..chunks`, where
    /// `parts` are the `c`-th chunks of the slices in `views` (made with
    /// [`chunks_mut`]): each chunk is handed to exactly one call.
    pub fn for_chunks<V: Views>(
        &self,
        chunks: usize,
        views: V,
        f: impl Fn(usize, V::Parts) + Sync,
    ) {
        let n = chunks.min(views.chunks());
        self.run(n, &|c| {
            // SAFETY: `run` calls this closure exactly once per index, so
            // no two calls get the same chunk.
            let parts = unsafe { views.part(c) };
            f(c, parts);
        });
    }
}

/// Claim and run tasks of job `epoch` until none are left.
fn work(s: &Shared, epoch: u64) {
    let tag = epoch & EPOCH_MASK;
    let mut c = s.claim.load(Ordering::Acquire);
    loop {
        let (n, k) = ((c >> NEXT_BITS) & FIELD, c & FIELD);
        if c >> TAG_SHIFT != tag || k >= n {
            return;
        }
        if let Err(now) =
            s.claim
                .compare_exchange_weak(c, c + 1, Ordering::Acquire, Ordering::Acquire)
        {
            c = now;
            continue;
        }
        run_task(s, k as usize);
        c = s.claim.load(Ordering::Acquire);
    }
}

/// Run task `k` of the current job, which this thread has claimed, and
/// count it done.
fn run_task(s: &Shared, k: usize) {
    {
        // SAFETY: task `k` is ours, so `run` is still waiting for it and
        // the job it stored before publishing the claim word (which our
        // claim read with Acquire) is alive and visible. The reference ends
        // with this block, before the task is counted.
        let f = unsafe { &*(*s.job.get()).expect("team job") };
        if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(k))) {
            let mut slot = s.panic.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                *slot = Some(p);
            }
            s.panicked.store(true, Ordering::Relaxed);
        }
    }
    // Release: `run` reads the task's effects, and `panicked`, once it has
    // seen every task counted.
    s.done.fetch_add(1, Ordering::Release);
}

fn worker(s: &Shared, me: usize) {
    let mut seen = 0u64;
    loop {
        // Wait for a new job: spin, then park.
        let since = Instant::now();
        let mut spins = 0u32;
        loop {
            let e = s.epoch.load(Ordering::Acquire);
            if e != seen {
                seen = e;
                break;
            }
            if s.stop.load(Ordering::Acquire) {
                return;
            }
            spins = spins.wrapping_add(1);
            if !spins.is_multiple_of(1024) || since.elapsed() < SPIN {
                core::hint::spin_loop();
                continue;
            }
            s.parked[me].store(true, Ordering::SeqCst);
            // Re-check after announcing, so a job posted in between is not
            // missed; `unpark` before `park` makes `park` return at once.
            if s.epoch.load(Ordering::SeqCst) == seen && !s.stop.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_millis(50));
            }
            s.parked[me].store(false, Ordering::Relaxed);
        }
        // A late wake-up finds the job's tasks all taken, or a later job's
        // claim word, and goes back to waiting.
        work(s, seen);
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        for (_, t) in &self.workers {
            t.unpark();
        }
        for (h, _) in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

thread_local! {
    static CURRENT: Cell<*const Team> = const { Cell::new(core::ptr::null()) };
}

/// Make `team` the one [`current`] returns on this thread while `f` runs.
pub fn install<R>(team: &Team, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(team as *const Team));
    struct Restore(*const Team);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(prev);
    f()
}

/// Run `f` with the team installed on this thread, or a one-thread team
/// when none is: everything still works, sequentially.
pub fn with_current<R>(f: impl FnOnce(&Team) -> R) -> R {
    let p = CURRENT.with(|c| c.get());
    if p.is_null() {
        SOLO.with(|t| f(t))
    } else {
        // SAFETY: `install` keeps the team alive and registered only for
        // the duration of its closure.
        f(unsafe { &*p })
    }
}

thread_local! {
    static SOLO: Team = Team::new(1);
}

/// Chunked mutable view of a slice for [`Team::for_chunks`].
pub struct ChunksMut<'a, T> {
    ptr: *mut T,
    len: usize,
    chunk: usize,
    _borrow: PhantomData<&'a mut [T]>,
}

// SAFETY: a `ChunksMut` hands out disjoint chunks, each to one task.
unsafe impl<T: Send> Sync for ChunksMut<'_, T> {}
unsafe impl<T: Send> Send for ChunksMut<'_, T> {}

/// Split `v` into chunks of `chunk` elements (the last may be shorter).
pub fn chunks_mut<T>(v: &mut [T], chunk: usize) -> ChunksMut<'_, T> {
    assert!(chunk > 0);
    ChunksMut {
        ptr: v.as_mut_ptr(),
        len: v.len(),
        chunk,
        _borrow: PhantomData,
    }
}

impl<T> ChunksMut<'_, T> {
    fn count(&self) -> usize {
        self.len.div_ceil(self.chunk)
    }

    /// # Safety
    /// Each `c` may be taken at most once while the returned slice lives.
    unsafe fn take<'b>(&self, c: usize) -> &'b mut [T] {
        let s = c * self.chunk;
        let e = (s + self.chunk).min(self.len);
        assert!(s < e);
        // SAFETY: in bounds, and the caller takes each chunk once.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.add(s), e - s) }
    }
}

/// One or more [`ChunksMut`] views split together, chunk by chunk.
pub trait Views: Sync {
    type Parts;
    fn chunks(&self) -> usize;
    /// # Safety
    /// Each `c` may be taken at most once.
    unsafe fn part(&self, c: usize) -> Self::Parts;
}

impl<'a, T: Send> Views for ChunksMut<'a, T> {
    type Parts = &'a mut [T];
    fn chunks(&self) -> usize {
        self.count()
    }
    unsafe fn part(&self, c: usize) -> &'a mut [T] {
        // SAFETY: forwarded contract.
        unsafe { self.take(c) }
    }
}

macro_rules! views_tuple {
    ($($v:ident $i:tt),+) => {
        impl<$($v: Views),+> Views for ($($v,)+) {
            type Parts = ($($v::Parts,)+);
            fn chunks(&self) -> usize {
                let mut n = usize::MAX;
                $(n = n.min(self.$i.chunks());)+
                n
            }
            unsafe fn part(&self, c: usize) -> Self::Parts {
                // SAFETY: forwarded contract.
                unsafe { ($(self.$i.part(c),)+) }
            }
        }
    };
}
views_tuple!(A 0);
views_tuple!(A 0, B 1);
views_tuple!(A 0, B 1, C 2);
views_tuple!(A 0, B 1, C 2, D 3);
views_tuple!(A 0, B 1, C 2, D 3, E 4);
views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, AtomicU32};

    #[test]
    fn every_chunk_once_at_any_size() {
        for threads in [1, 2, 4, 7] {
            let team = Team::new(threads);
            for round in 0..50u32 {
                let mut a = vec![0u32; 1000 + round as usize];
                let mut b = vec![0u64; 1000 + round as usize];
                team.for_chunks(
                    usize::MAX,
                    (chunks_mut(&mut a, 64), chunks_mut(&mut b, 64)),
                    |c, (a, b)| {
                        for (k, (x, y)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
                            *x += 1 + c as u32;
                            *y = (c * 64 + k) as u64;
                        }
                    },
                );
                for (i, (&x, &y)) in a.iter().zip(&b).enumerate() {
                    assert_eq!(x, 1 + (i / 64) as u32);
                    assert_eq!(y, i as u64);
                }
            }
        }
    }

    #[test]
    fn installed_team_is_current() {
        let team = Team::new(3);
        install(&team, || with_current(|t| assert_eq!(t.threads(), 3)));
        with_current(|t| assert_eq!(t.threads(), 1));
    }

    #[test]
    fn parks_and_wakes() {
        let team = Team::new(4);
        let hits = AtomicUsize::new(0);
        for _ in 0..3 {
            team.run(100, &|_| {
                hits.fetch_add(1, Ordering::Relaxed);
            });
            thread::sleep(SPIN * 2);
        }
        assert_eq!(hits.load(Ordering::Relaxed), 300);
    }

    /// Runs `jobs` jobs of 2..=40 tasks and checks every index ran once.
    fn each_index_once(team: &Team, jobs: usize) {
        let hits: Vec<AtomicU32> = (0..40).map(|_| AtomicU32::new(0)).collect();
        for j in 0..jobs {
            let n = 2 + (j * 7919) % 39;
            team.run(n, &|k| {
                hits[k].fetch_add(1, Ordering::Relaxed);
            });
            for (k, h) in hits.iter().enumerate() {
                assert_eq!(h.swap(0, Ordering::Relaxed), (k < n) as u32, "job {j}");
            }
        }
    }

    #[test]
    fn many_short_jobs_run_each_index_once() {
        for threads in [2, 4, 8] {
            each_index_once(&Team::new(threads), 20_000);
        }
    }

    #[test]
    fn claims_survive_the_epoch_tag_wrapping() {
        let team = Team::new(4);
        team.shared.epoch.store(EPOCH_MASK - 3, Ordering::Relaxed);
        each_index_once(&team, 100);
        assert!(team.shared.epoch.load(Ordering::Relaxed) > EPOCH_MASK);
    }

    #[test]
    fn jobs_do_not_wait_for_late_workers() {
        // Every worker starts late, as if descheduled: the caller runs the
        // jobs alone meanwhile, and the latecomers then wake to finished
        // jobs and must neither run their tasks again nor hold anything up.
        let late = Duration::from_millis(400);
        let team = Team::with_start(4, move || thread::sleep(late));
        let t0 = Instant::now();
        each_index_once(&team, 200);
        assert!(t0.elapsed() < late, "a job waited for a worker");
        thread::sleep(late + Duration::from_millis(50));
        each_index_once(&team, 2_000);
        // The workers take part again once they are up.
        let names = Mutex::new(std::collections::BTreeSet::new());
        for _ in 0..100 {
            team.run(16, &|_| {
                thread::sleep(Duration::from_micros(200));
                let me = thread::current().name().map(str::to_owned);
                names.lock().unwrap().insert(me);
            });
            if names.lock().unwrap().len() > 1 {
                return;
            }
        }
        panic!("no worker took part");
    }

    fn message(p: &Panic) -> &str {
        p.downcast_ref::<&str>()
            .copied()
            .or_else(|| p.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("?")
    }

    #[test]
    fn panics_reach_the_caller_after_every_task_ran() {
        let team = Team::new(4);
        for bad in [0, 13, 63] {
            let hits: Vec<AtomicU32> = (0..64).map(|_| AtomicU32::new(0)).collect();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                team.run(64, &|k| {
                    hits[k].fetch_add(1, Ordering::Relaxed);
                    thread::sleep(Duration::from_micros(50));
                    if k == bad || (bad == 0 && k % 2 == 1) {
                        panic!("task {k}");
                    }
                })
            }));
            let p = r.expect_err("the panic is resumed");
            assert!(message(&p).starts_with("task "));
            assert!(hits.iter().all(|h| h.load(Ordering::Relaxed) == 1));
            // The team is usable again, with no stale panic left behind.
            each_index_once(&team, 100);
        }
    }

    #[test]
    fn a_job_inside_a_job_panics_and_leaves_the_team_usable() {
        let team = Team::new(3);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            install(&team, || {
                team.run(8, &|_| with_current(|t| t.run(4, &|_| {})));
            })
        }));
        // Workers have no team installed, so only the caller's tasks nest.
        if let Err(p) = r {
            assert!(message(&p).starts_with("Team::run inside a job"));
        }
        each_index_once(&team, 100);
    }

    #[test]
    fn jobs_bigger_than_a_claim_word_are_grouped() {
        let team = Team::new(4);
        let n = 2 * MAX_TASKS + 5;
        let hits: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
        team.run(n, &|k| {
            hits[k].fetch_add(1, Ordering::Relaxed);
        });
        assert!(hits.iter().all(|h| h.swap(0, Ordering::Relaxed) == 1));
        // A panic still lets the rest of its group run.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            team.run(n, &|k| {
                hits[k].fetch_add(1, Ordering::Relaxed);
                assert!(k != 1000, "task 1000");
            })
        }));
        assert_eq!(message(&r.expect_err("the panic is resumed")), "task 1000");
        assert!(hits.iter().all(|h| h.load(Ordering::Relaxed) == 1));
    }
}

/// Chunked shared view of a slice.
pub struct Chunks<'a, T> {
    v: &'a [T],
    chunk: usize,
}

impl<'a, T: Sync> Views for Chunks<'a, T> {
    type Parts = &'a [T];
    fn chunks(&self) -> usize {
        self.v.len().div_ceil(self.chunk)
    }
    unsafe fn part(&self, c: usize) -> &'a [T] {
        let s = c * self.chunk;
        &self.v[s..(s + self.chunk).min(self.v.len())]
    }
}

/// Element-wise mutable view: part `c` is `&mut v[c]`.
pub struct ItemsMut<'a, T>(ChunksMut<'a, T>);

impl<'a, T: Send> Views for ItemsMut<'a, T> {
    type Parts = &'a mut T;
    fn chunks(&self) -> usize {
        self.0.count()
    }
    unsafe fn part(&self, c: usize) -> &'a mut T {
        // SAFETY: forwarded contract; chunks of one element.
        unsafe { &mut self.0.take(c)[0] }
    }
}

/// Element-wise shared view: part `c` is `&v[c]`.
pub struct Items<'a, T>(&'a [T]);

impl<'a, T: Sync> Views for Items<'a, T> {
    type Parts = &'a T;
    fn chunks(&self) -> usize {
        self.0.len()
    }
    unsafe fn part(&self, c: usize) -> &'a T {
        &self.0[c]
    }
}

/// Two views split side by side; parts are pairs, as with rayon's `zip`.
pub struct Zip<A, B>(A, B);

impl<A: Views, B: Views> Views for Zip<A, B> {
    type Parts = (A::Parts, B::Parts);
    fn chunks(&self) -> usize {
        self.0.chunks().min(self.1.chunks())
    }
    unsafe fn part(&self, c: usize) -> Self::Parts {
        // SAFETY: forwarded contract.
        unsafe { (self.0.part(c), self.1.part(c)) }
    }
}

/// A parallel loop over the parts of a view, run on the [`current`] team:
/// a rayon-like surface (`zip`, `enumerate`, `for_each`) over fixed chunks.
pub struct Par<V>(V);

/// [`Par`] with chunk indices.
pub struct ParEnum<V>(V);

impl<V: Views> Par<V> {
    pub fn zip<W: Views>(self, other: Par<W>) -> Par<Zip<V, W>> {
        Par(Zip(self.0, other.0))
    }

    pub fn enumerate(self) -> ParEnum<V> {
        ParEnum(self.0)
    }

    pub fn for_each(self, f: impl Fn(V::Parts) + Sync) {
        let v = self.0;
        with_current(|t| {
            t.run(v.chunks(), &|c| {
                // SAFETY: `run` calls this exactly once per index.
                f(unsafe { v.part(c) })
            })
        });
    }
}

/// A [`Par`] or [`ParEnum`] mapped to a value per part, to be reduced.
pub struct ParMap<V, F>(V, F);

/// [`ParMap`] over enumerated parts.
pub struct ParEnumMap<V, F>(V, F);

/// Fold one value per task into `acc` under a lock. The order tasks finish
/// in varies, so `op` must be exactly associative and commutative (integer
/// sums, `max`, `min`), never a float sum.
fn reduce_tasks<R: Send>(
    n: usize,
    each: &(dyn Fn(usize) -> R + Sync),
    identity: &(dyn Fn() -> R + Sync),
    op: &(dyn Fn(R, R) -> R + Sync),
) -> R {
    let acc = std::sync::Mutex::new(Some(identity()));
    with_current(|t| {
        t.run(n, &|c| {
            let r = each(c);
            let mut g = acc.lock().unwrap_or_else(|e| e.into_inner());
            let a = g.take().expect("accumulator");
            *g = Some(op(a, r));
        })
    });
    acc.into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .expect("accumulator")
}

impl<V: Views> Par<V> {
    pub fn map<R, F: Fn(V::Parts) -> R + Sync>(self, f: F) -> ParMap<V, F> {
        ParMap(self.0, f)
    }
}

impl<V: Views> ParEnum<V> {
    pub fn map<R, F: Fn((usize, V::Parts)) -> R + Sync>(self, f: F) -> ParEnumMap<V, F> {
        ParEnumMap(self.0, f)
    }
}

impl<V: Views, R: Send, F: Fn(V::Parts) -> R + Sync> ParMap<V, F> {
    /// See [`reduce_tasks`]: `op` must be exactly associative and commutative.
    pub fn reduce(self, identity: impl Fn() -> R + Sync, op: impl Fn(R, R) -> R + Sync) -> R {
        let (v, f) = (self.0, self.1);
        // SAFETY: each index is run exactly once.
        reduce_tasks(v.chunks(), &|c| f(unsafe { v.part(c) }), &identity, &op)
    }
}

impl<V: Views, R: Send, F: Fn((usize, V::Parts)) -> R + Sync> ParEnumMap<V, F> {
    /// See [`reduce_tasks`]: `op` must be exactly associative and commutative.
    pub fn reduce(self, identity: impl Fn() -> R + Sync, op: impl Fn(R, R) -> R + Sync) -> R {
        let (v, f) = (self.0, self.1);
        // SAFETY: each index is run exactly once.
        reduce_tasks(
            v.chunks(),
            &|c| f((c, unsafe { v.part(c) })),
            &identity,
            &op,
        )
    }

    /// Integer sum of the mapped values.
    pub fn sum<S>(self) -> S
    where
        R: Into<S>,
        S: core::ops::Add<Output = S> + Default + Send,
        F: Fn((usize, V::Parts)) -> R,
    {
        let (v, f) = (self.0, self.1);
        // SAFETY: each index is run exactly once.
        reduce_tasks(
            v.chunks(),
            &|c| f((c, unsafe { v.part(c) })).into(),
            &S::default,
            &|a, b| a + b,
        )
    }
}

impl<V: Views> ParEnum<V> {
    pub fn for_each(self, f: impl Fn((usize, V::Parts)) + Sync) {
        let v = self.0;
        with_current(|t| {
            t.run(v.chunks(), &|c| {
                // SAFETY: `run` calls this exactly once per index.
                f((c, unsafe { v.part(c) }))
            })
        });
    }
}

/// Parallel views of mutable slices, named as in rayon.
pub trait ParSliceMut<T: Send> {
    fn par_chunks_mut(&mut self, chunk: usize) -> Par<ChunksMut<'_, T>>;
    fn par_iter_mut(&mut self) -> Par<ItemsMut<'_, T>>;
}

/// Parallel views of shared slices, named as in rayon.
pub trait ParSlice<T: Sync> {
    fn par_chunks(&self, chunk: usize) -> Par<Chunks<'_, T>>;
    fn par_iter(&self) -> Par<Items<'_, T>>;
}

impl<T: Send> ParSliceMut<T> for [T] {
    fn par_chunks_mut(&mut self, chunk: usize) -> Par<ChunksMut<'_, T>> {
        Par(chunks_mut(self, chunk))
    }
    fn par_iter_mut(&mut self) -> Par<ItemsMut<'_, T>> {
        Par(ItemsMut(chunks_mut(self, 1)))
    }
}

impl<T: Sync> ParSlice<T> for [T] {
    fn par_chunks(&self, chunk: usize) -> Par<Chunks<'_, T>> {
        assert!(chunk > 0);
        Par(Chunks { v: self, chunk })
    }
    fn par_iter(&self) -> Par<Items<'_, T>> {
        Par(Items(self))
    }
}

/// `use riot_team::prelude::*` in place of `rayon::prelude::*`.
pub mod prelude {
    pub use crate::{ParSlice, ParSliceMut};
}

#[cfg(test)]
mod par_tests {
    use super::prelude::*;
    use super::*;

    #[test]
    fn rayon_style_loops() {
        let team = Team::new(4);
        install(&team, || {
            let mut a = vec![0u32; 2048];
            let mut n = vec![0u8; 4];
            let b: Vec<u32> = (0..2048).collect();
            a.par_chunks_mut(512)
                .zip(n.par_iter_mut())
                .zip(b.par_chunks(512))
                .enumerate()
                .for_each(|(c, ((a, n), b))| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x = y * 2;
                    }
                    *n = c as u8;
                });
            assert!(a.iter().enumerate().all(|(i, &x)| x == i as u32 * 2));
            assert_eq!(n, [0, 1, 2, 3]);
        });
    }
}
