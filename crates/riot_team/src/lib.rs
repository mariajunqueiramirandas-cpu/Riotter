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

use core::marker::PhantomData;
use std::cell::{Cell, UnsafeCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

/// How long an idle worker spins for the next job before it parks.
const SPIN: Duration = Duration::from_millis(2);

type Job = dyn Fn(usize) + Sync;

struct Shared {
    /// Bumped for every job; workers watch it.
    epoch: AtomicU64,
    /// The current job, valid while `pending > 0`.
    job: UnsafeCell<Option<*const Job>>,
    tasks: AtomicUsize,
    next: AtomicUsize,
    /// Workers that have not finished the current job.
    pending: AtomicUsize,
    parked: Vec<AtomicBool>,
    panicked: AtomicBool,
    stop: AtomicBool,
}

// SAFETY: `job` is written only by the thread running `Team::run` while no
// worker reads it (all workers are between jobs), and read by workers only
// after they observe the new epoch with Acquire ordering.
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
            job: UnsafeCell::new(None),
            tasks: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            parked: (0..n).map(|_| AtomicBool::new(false)).collect(),
            panicked: AtomicBool::new(false),
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
        assert!(
            !self.running.swap(true, Ordering::Acquire),
            "Team::run inside a job, or from two threads at once"
        );
        let s = &*self.shared;
        // SAFETY: no worker is inside a job (the last `run` waited for
        // `pending == 0`), so nobody reads `job` now. The pointer is only
        // used until this function returns, and `f` outlives that.
        unsafe {
            let p: *const (dyn Fn(usize) + Sync + '_) = f;
            *s.job.get() = Some(core::mem::transmute::<
                *const (dyn Fn(usize) + Sync + '_),
                *const Job,
            >(p));
        }
        s.tasks.store(tasks, Ordering::Relaxed);
        s.next.store(0, Ordering::Relaxed);
        s.pending.store(self.workers.len(), Ordering::Relaxed);
        s.epoch.fetch_add(1, Ordering::Release);
        for (k, (_, t)) in self.workers.iter().enumerate() {
            if s.parked[k].load(Ordering::Acquire) {
                t.unpark();
            }
        }
        // A panic here must still wait for the workers, which use `f`.
        let mine = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(s, f)));
        let mut spins = 0u32;
        while s.pending.load(Ordering::Acquire) != 0 {
            spins += 1;
            if spins < 1 << 16 {
                core::hint::spin_loop();
            } else {
                thread::yield_now();
            }
        }
        // SAFETY: every worker has finished with the job.
        unsafe { *s.job.get() = None };
        self.running.store(false, Ordering::Release);
        if let Err(p) = mine {
            s.panicked.store(false, Ordering::Release);
            std::panic::resume_unwind(p);
        }
        if s.panicked.swap(false, Ordering::AcqRel) {
            panic!("a team job panicked");
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

fn work(s: &Shared, f: &(dyn Fn(usize) + Sync + '_)) {
    let n = s.tasks.load(Ordering::Relaxed);
    loop {
        let k = s.next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            break;
        }
        f(k);
    }
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
            s.parked[me].store(true, Ordering::Release);
            // Re-check after announcing, so a job posted in between is not
            // missed; `unpark` before `park` makes `park` return at once.
            if s.epoch.load(Ordering::Acquire) == seen && !s.stop.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_millis(50));
            }
            s.parked[me].store(false, Ordering::Release);
        }
        // SAFETY: the epoch moved, so `run` stored the job before bumping
        // it (Release) and keeps it alive until `pending` reaches zero.
        let job = unsafe { (*s.job.get()).expect("team job") };
        let f = unsafe { &*job };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(s, f))).is_err() {
            s.panicked.store(true, Ordering::Release);
        }
        s.pending.fetch_sub(1, Ordering::AcqRel);
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
