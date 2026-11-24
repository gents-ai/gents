#![cfg(loom)]

//! loom models of the read-view publication protocol.
//!
//! # What these models check, and what they do not
//!
//! `kovan` carries no `cfg(loom)` instrumentation: its pointer word is
//! `core::sync::atomic::AtomicUsize` (swappable for shuttle's, never
//! for loom's), and its epoch/slot protocol is a 128-bit DCAS loom has
//! no type for. A loom model that called the real `ReadViewCell`
//! would therefore run kovan on hardware atomics, give loom's
//! scheduler no yield point inside `Atom::load` or `Atom::rcu`, and
//! explore a handful of spawn/join orders while proving nothing.
//!
//! So these models do not call kovan. They transcribe, in loom
//! atomics, the protocol regolith builds *on top of* `Atom`: the
//! compare-exchange retry loop, the four mutation closures the engine
//! passes it, and the load ordering the read path depends on. Each
//! model names the source lines it transcribes. The transcription is
//! the boundary of what is proved: a divergence between one of these
//! models and the line it names is not caught here.
//!
//! Three things are deliberately out of scope, because loom cannot see
//! them:
//!
//! * **Reclamation.** The models never free a retired view. When a view
//!   may be freed is kovan's guard contract, checked by kovan's own
//!   shuttle suite, not here.
//! * **The DCAS epoch protocol** behind `Atom::load`'s wait-free bound.
//! * **Torn reads of a view.** A `ReadView` is one heap allocation
//!   behind one pointer, so the model publishes one word. That a whole
//!   view is published in one step is structural in both, not a
//!   property either one tests.
//!
//! What is left is genuinely regolith's, and every model below would fail
//! if regolith got it wrong. Each ships with a negative control that
//! mutates one decision and asserts loom reports the violation, so a
//! model that stopped exploring cannot pass silently. Every model also
//! asserts a floor on the number of interleavings loom explored.

use std::panic::AssertUnwindSafe;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;

/// Memtable ids. Small enough to pack into a nibble, distinct enough
/// that a lost or duplicated one is visible in an assertion message.
const ACTIVE_AT_START: u8 = 1;
const FROZEN_AT_START: u8 = 2;
const FRESH: u8 = 3;

const MAX_FROZEN: usize = 4;

/// One published view, packed into a single word.
///
/// The packing is what makes the model faithful: a `ReadView` is one
/// immutable heap allocation reached through one pointer, so a
/// publication moves exactly one word here too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct View {
    active: u8,
    frozen: [u8; MAX_FROZEN],
    frozen_len: u8,
    /// Memtable ids folded into the version by a flush.
    flushed: u16,
    /// Lowest sequence the version still retains. Compaction raises it
    /// to the read horizon it sampled.
    gc_floor: u8,
    /// Publication counter. Not a field of the real `ReadView`; it is
    /// the model's witness that a reader never sees the view go
    /// backwards.
    generation: u8,
}

impl View {
    fn initial() -> Self {
        Self {
            active: ACTIVE_AT_START,
            frozen: [FROZEN_AT_START, 0, 0, 0],
            frozen_len: 1,
            flushed: 0,
            gc_floor: 1,
            generation: 0,
        }
    }

    fn pack(self) -> u64 {
        let mut word = u64::from(self.active) & 0xF;
        for (i, id) in self.frozen.iter().enumerate() {
            word |= (u64::from(*id) & 0xF) << (4 + 4 * i);
        }
        word |= (u64::from(self.frozen_len) & 0x7) << 20;
        word |= u64::from(self.flushed) << 24;
        word |= (u64::from(self.gc_floor) & 0xF) << 40;
        word |= (u64::from(self.generation) & 0xF) << 44;
        word
    }

    fn unpack(word: u64) -> Self {
        let mut frozen = [0u8; MAX_FROZEN];
        for (i, slot) in frozen.iter_mut().enumerate() {
            *slot = ((word >> (4 + 4 * i)) & 0xF) as u8;
        }
        Self {
            active: (word & 0xF) as u8,
            frozen,
            frozen_len: ((word >> 20) & 0x7) as u8,
            flushed: ((word >> 24) & 0xFFFF) as u16,
            gc_floor: ((word >> 40) & 0xF) as u8,
            generation: ((word >> 44) & 0xF) as u8,
        }
    }

    /// Every memtable a reader holding this view can still reach, as a
    /// bitmask: the active one, the frozen ones, and the ones already
    /// folded into the version.
    fn reachable_ids(&self) -> u16 {
        let mut mask = 1u16 << self.active | self.flushed;
        for id in &self.frozen[..self.frozen_len as usize] {
            mask |= 1u16 << id;
        }
        mask
    }

    /// The memtables this view still names in memory, version
    /// excluded.
    fn in_memory_ids(&self) -> u16 {
        let mut mask = 1u16 << self.active;
        for id in &self.frozen[..self.frozen_len as usize] {
            mask |= 1u16 << id;
        }
        mask
    }
}

/// Seal the active memtable and hand writers a fresh one, in one
/// publication. Transcribes `RegolithEngine::rotate_memtable`
/// (src/engine/mod.rs:1805).
fn rotate(view: View, fresh: u8) -> View {
    let mut next = view;
    next.frozen[view.frozen_len as usize] = view.active;
    next.frozen_len = view.frozen_len + 1;
    next.active = fresh;
    next.generation = view.generation + 1;
    next
}

/// The prepending variant this protocol must not have. Used only by
/// the negative control for [`two_publishers_never_lose_a_memtable`].
fn rotate_prepending(view: View, fresh: u8) -> View {
    let mut next = view;
    next.frozen = [0; MAX_FROZEN];
    next.frozen[0] = view.active;
    for (i, id) in view.frozen[..view.frozen_len as usize].iter().enumerate() {
        next.frozen[i + 1] = *id;
    }
    next.frozen_len = view.frozen_len + 1;
    next.active = fresh;
    next.generation = view.generation + 1;
    next
}

/// Drop the oldest frozen memtable. Transcribes
/// `RegolithEngine::retire_oldest_frozen` (src/engine/mod.rs:1315),
/// including its `frozen.get(1..).unwrap_or_default()` behaviour on an
/// empty list.
fn retire_oldest(view: View) -> View {
    let mut next = view;
    next.frozen = [0; MAX_FROZEN];
    next.frozen_len = view.frozen_len.saturating_sub(1);
    for i in 0..next.frozen_len as usize {
        next.frozen[i] = view.frozen[i + 1];
    }
    next.generation = view.generation + 1;
    next
}

/// Install a version that has absorbed `flushed_id` and collected
/// everything shadowed below `gc_floor`. Transcribes
/// `ReadViewCell::publish_version` (src/engine/read_view.rs), which
/// `VersionGuard::drop` is the only caller of.
fn publish_version(view: View, flushed_id: u8, gc_floor: u8) -> View {
    let mut next = view;
    next.flushed = view.flushed | (1u16 << flushed_id);
    next.gc_floor = gc_floor;
    next.generation = view.generation + 1;
    next
}

/// The published cell. Transcribes `Atom<ReadView>` as regolith uses it:
/// an acquire load, and an `AcqRel`/`Acquire` compare-exchange retry
/// loop that rebuilds on whatever won (kovan `src/atom.rs:557`).
struct Cell {
    word: AtomicU64,
}

impl Cell {
    fn new(view: View) -> Self {
        Self {
            word: AtomicU64::new(view.pack()),
        }
    }

    fn load(&self) -> View {
        self.load_with(Ordering::Acquire)
    }

    fn load_with(&self, order: Ordering) -> View {
        View::unpack(self.word.load(order))
    }

    /// Returns the number of attempts the publication took.
    fn rcu(&self, mutate: impl Fn(View) -> View) -> u32 {
        self.rcu_with(Ordering::AcqRel, mutate)
    }

    fn rcu_with(&self, success: Ordering, mutate: impl Fn(View) -> View) -> u32 {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let current = self.word.load(Ordering::Acquire);
            let next = mutate(View::unpack(current)).pack();
            if self
                .word
                .compare_exchange(current, next, success, Ordering::Acquire)
                .is_ok()
            {
                return attempts;
            }
        }
    }
}

/// Run a loom model, report how many interleavings it explored, and
/// fail if that number collapsed below `floor`. A model that stops
/// exploring stops proving anything, so the floor is an assertion and
/// not a log line. Each floor is set at roughly half the count loom
/// reaches today, which leaves room for a loom release to prune
/// differently while still failing if a model degenerates.
fn run_model(name: &str, floor: usize, body: impl Fn() + Send + Sync + 'static) -> usize {
    let executions = StdArc::new(StdAtomicUsize::new(0));
    let counter = StdArc::clone(&executions);
    loom::model(move || {
        counter.fetch_add(1, StdOrdering::Relaxed);
        body();
    });
    let explored = executions.load(StdOrdering::Relaxed);
    println!("loom model `{name}`: {explored} interleavings explored");
    assert!(
        explored >= floor,
        "`{name}` explored only {explored} interleavings (floor {floor}): the \
         state space collapsed and this model proves nothing",
    );
    explored
}

/// Assert that loom finds a violation once one decision is mutated.
/// loom's report for the mutated model is printed below the marker;
/// it is evidence that the model discriminates, not a failure.
fn expect_violation(name: &str, body: impl Fn() + Send + Sync + 'static) {
    println!("negative control `{name}`: loom must report a violation below");
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| loom::model(body)));
    assert!(
        outcome.is_err(),
        "negative control `{name}` passed: the model cannot tell the mutated \
         protocol from the real one",
    );
    println!("negative control `{name}`: violation reported, as required");
}

/// A reader loading the view while a publisher installs a new one.
///
/// The publisher builds the fresh memtable before it publishes
/// (`let fresh = Arc::new(MemTable::new())` above the closure at
/// src/engine/mod.rs:1804). A reader that observes the new view must
/// see that memtable initialised. Both sides touch the contents with
/// `Relaxed`, so the only thing that can order them is the
/// release/acquire pair on the published word itself - which is the
/// claim `ReadViewCell::load`'s doc makes.
#[test]
fn a_reader_that_observes_a_publication_sees_what_built_it() {
    run_model("reader_vs_publisher", 64, || {
        let cell = Arc::new(Cell::new(View::initial()));
        let fresh_contents = Arc::new(AtomicU64::new(0));

        let publisher = {
            let cell = Arc::clone(&cell);
            let contents = Arc::clone(&fresh_contents);
            thread::spawn(move || {
                contents.store(u64::from(FRESH), Ordering::Relaxed);
                cell.rcu(|view| rotate(view, FRESH))
            })
        };

        let reader = {
            let cell = Arc::clone(&cell);
            let contents = Arc::clone(&fresh_contents);
            thread::spawn(move || {
                let view = cell.load();
                if view.active == FRESH {
                    assert_eq!(
                        contents.load(Ordering::Relaxed),
                        u64::from(FRESH),
                        "a reader observed the fresh memtable in the view but \
                         not the writes that initialised it",
                    );
                }
                view
            })
        };

        let attempts = publisher.join().unwrap();
        let observed = reader.join().unwrap();

        assert_eq!(attempts, 1, "an uncontended publication takes one attempt");
        let published = rotate(View::initial(), FRESH);
        assert!(
            observed == View::initial() || observed == published,
            "a reader observed {observed:?}, which was never published",
        );
        assert_eq!(cell.load(), published);
    });
}

#[test]
fn a_relaxed_publication_is_caught_by_the_reader_model() {
    expect_violation("reader_vs_publisher/relaxed_publish", || {
        let cell = Arc::new(Cell::new(View::initial()));
        let fresh_contents = Arc::new(AtomicU64::new(0));

        let publisher = {
            let cell = Arc::clone(&cell);
            let contents = Arc::clone(&fresh_contents);
            thread::spawn(move || {
                contents.store(u64::from(FRESH), Ordering::Relaxed);
                cell.rcu_with(Ordering::Relaxed, |view| rotate(view, FRESH));
            })
        };

        let reader = {
            let cell = Arc::clone(&cell);
            let contents = Arc::clone(&fresh_contents);
            thread::spawn(move || {
                let view = cell.load_with(Ordering::Relaxed);
                if view.active == FRESH {
                    assert_eq!(contents.load(Ordering::Relaxed), u64::from(FRESH));
                }
            })
        };

        publisher.join().unwrap();
        reader.join().unwrap();
    });
}

/// Two publishers racing: a rotation against the two publications a
/// flush completion makes.
///
/// The rotation appends to `frozen` and the retirement removes index 0,
/// so the two commute and every interleaving must land on the same
/// view. That is the claim `retire_oldest_frozen`'s doc makes
/// ("rotation only ever pushes to the tail of `frozen`, so index 0 is
/// still the oldest on a re-read") and it is the reason a lost CAS can
/// simply rebuild instead of serialising.
#[test]
fn two_publishers_never_lose_a_memtable() {
    let worst_attempts = StdArc::new(StdAtomicUsize::new(0));
    let observed = StdArc::clone(&worst_attempts);

    run_model("rotation_vs_flush_completion", 64, move || {
        let cell = Arc::new(Cell::new(View::initial()));

        let rotation = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || cell.rcu(|view| rotate(view, FRESH)))
        };

        let flush_completion = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || {
                let publish = cell.rcu(|view| publish_version(view, FROZEN_AT_START, 1));
                let retire = cell.rcu(retire_oldest);
                publish.max(retire)
            })
        };

        let rotation_attempts = rotation.join().unwrap();
        let flush_attempts = flush_completion.join().unwrap();
        observed.fetch_max(
            rotation_attempts.max(flush_attempts) as usize,
            StdOrdering::Relaxed,
        );

        let expected = retire_oldest(publish_version(
            rotate(View::initial(), FRESH),
            FROZEN_AT_START,
            1,
        ));
        let final_view = cell.load();
        assert_eq!(
            final_view, expected,
            "the two publishers must commute; rotation took \
             {rotation_attempts} attempt(s), flush completion \
             {flush_attempts}",
        );
        assert_eq!(
            final_view.reachable_ids(),
            (1 << ACTIVE_AT_START) | (1 << FROZEN_AT_START) | (1 << FRESH),
            "every memtable is still reachable as active, frozen, or flushed",
        );
        // A publication retries only because another one succeeded, so
        // it takes at most one attempt more than the number of
        // publications that can beat it: three for the rotation, which
        // the flush completion can outrun twice, and two for each of
        // the flush completion's own two publications.
        assert!(
            rotation_attempts <= 3,
            "rotation took {rotation_attempts} attempts against two \
             competing publications",
        );
        assert!(
            flush_attempts <= 2,
            "a flush publication took {flush_attempts} attempts against one \
             competing publication",
        );
    });

    println!(
        "loom model `rotation_vs_flush_completion`: worst-case attempts for one \
         publication across all interleavings = {}",
        worst_attempts.load(StdOrdering::Relaxed),
    );
}

#[test]
fn a_prepending_rotation_is_caught_by_the_two_publisher_model() {
    expect_violation("rotation_vs_flush_completion/prepending_rotation", || {
        let cell = Arc::new(Cell::new(View::initial()));

        let rotation = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || cell.rcu(|view| rotate_prepending(view, FRESH)))
        };

        let flush_completion = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || {
                cell.rcu(|view| publish_version(view, FROZEN_AT_START, 1));
                cell.rcu(retire_oldest);
            })
        };

        rotation.join().unwrap();
        flush_completion.join().unwrap();

        assert_eq!(
            cell.load().reachable_ids(),
            (1 << ACTIVE_AT_START) | (1 << FROZEN_AT_START) | (1 << FRESH),
            "a memtable was dropped without being flushed",
        );
    });
}

/// A reader holding a loaded view while the publisher moves on.
///
/// The held view is what an `AtomGuard` gives a reader: an immutable
/// snapshot the publisher cannot edit. What has to hold is the third
/// invariant in `read_view.rs` - successive views only move data in
/// the older direction and never lose it - so every memtable the
/// stale reader can still name is, in the newest view, still active,
/// still frozen, or folded into the version. The reader's two loads
/// must also never travel backwards.
#[test]
fn a_stale_reader_loses_no_data_and_never_travels_backwards() {
    run_model("stale_reader_vs_publications", 512, || {
        let cell = Arc::new(Cell::new(View::initial()));

        let publisher = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || {
                cell.rcu(|view| rotate(view, FRESH));
                cell.rcu(|view| publish_version(view, FROZEN_AT_START, 1));
                cell.rcu(retire_oldest);
            })
        };

        let reader = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || {
                let held = cell.load();
                let later = cell.load();
                (held, later)
            })
        };

        publisher.join().unwrap();
        let (held, later) = reader.join().unwrap();

        let after_rotate = rotate(View::initial(), FRESH);
        let after_flush = publish_version(after_rotate, FROZEN_AT_START, 1);
        let chain = [
            View::initial(),
            after_rotate,
            after_flush,
            retire_oldest(after_flush),
        ];
        assert!(
            chain.contains(&held) && chain.contains(&later),
            "a reader observed a view that was never published: {held:?} then \
             {later:?}",
        );
        assert!(
            later.generation >= held.generation,
            "the published view travelled backwards: generation \
             {} then {}",
            held.generation,
            later.generation,
        );

        let final_view = cell.load();
        assert_eq!(
            held.in_memory_ids() & !final_view.reachable_ids(),
            0,
            "a memtable the stale reader still names is gone from the newest \
             view: held {held:?}, newest {final_view:?}",
        );
    });
}

/// The load order the read path depends on: the view first, the read
/// horizon second (src/engine/mod.rs:779-780, and the paragraph above
/// `get_latest` that explains why).
///
/// Compaction collects everything shadowed below the horizon it
/// sampled and publishes that version. A reader that samples the
/// horizon *after* loading the view can only ever pair a view with a
/// horizon at least as new as the bound that view was collected at,
/// because the horizon is monotonic and the acquire on the view load
/// pairs with the publisher's `AcqRel` compare-exchange. Sampling the
/// horizon first breaks exactly that, which the negative control
/// below shows.
#[test]
fn the_view_is_loaded_before_the_horizon_is_sampled() {
    run_model("view_then_horizon", 128, || {
        let cell = Arc::new(Cell::new(View::initial()));
        let horizon = Arc::new(AtomicU64::new(1));

        let writer_then_compaction = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                horizon.fetch_max(2, Ordering::AcqRel);
                let bound = horizon.load(Ordering::Acquire);
                cell.rcu(move |view| publish_version(view, FROZEN_AT_START, bound as u8));
            })
        };

        let reader = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                let view = cell.load();
                let sampled = horizon.load(Ordering::Acquire);
                assert!(
                    u64::from(view.gc_floor) <= sampled,
                    "the read is pinned to a version collected at {} but reads \
                     at horizon {sampled}: data at or below {} was collected \
                     out from under it",
                    view.gc_floor,
                    view.gc_floor,
                );
            })
        };

        writer_then_compaction.join().unwrap();
        reader.join().unwrap();
    });
}

#[test]
fn sampling_the_horizon_first_is_caught_by_the_read_path_model() {
    expect_violation("view_then_horizon/horizon_first", || {
        let cell = Arc::new(Cell::new(View::initial()));
        let horizon = Arc::new(AtomicU64::new(1));

        let writer_then_compaction = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                horizon.fetch_max(2, Ordering::AcqRel);
                let bound = horizon.load(Ordering::Acquire);
                cell.rcu(move |view| publish_version(view, FROZEN_AT_START, bound as u8));
            })
        };

        let reader = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                let sampled = horizon.load(Ordering::Acquire);
                let view = cell.load();
                assert!(u64::from(view.gc_floor) <= sampled);
            })
        };

        writer_then_compaction.join().unwrap();
        reader.join().unwrap();
    });
}

/// Preemptions the write-free models explore. At this bound the search is
/// saturated: raising it to 12 explores no new interleaving (525 for
/// `horizon_then_view` and 114 for `apply_then_frontier`, the same at 8 and
/// at 12, measured). loom's search with no bound at all reports fewer, 336 and
/// 114, because without a bound it prunes more orders as equivalent, not
/// because it reaches more states. Each model has two threads of a few steps.
const WRITE_FREE_PREEMPTIONS: usize = 8;

/// [`run_model`] with [`WRITE_FREE_PREEMPTIONS`] as an explicit bound.
fn run_bounded_model(name: &str, floor: usize, body: impl Fn() + Send + Sync + 'static) -> usize {
    let executions = StdArc::new(StdAtomicUsize::new(0));
    let counter = StdArc::clone(&executions);
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(WRITE_FREE_PREEMPTIONS);
    builder.check(move || {
        counter.fetch_add(1, StdOrdering::Relaxed);
        body();
    });
    let explored = executions.load(StdOrdering::Relaxed);
    println!("loom model `{name}`: {explored} interleavings explored");
    assert!(
        explored >= floor,
        "`{name}` explored only {explored} interleavings (floor {floor}): the \
         state space collapsed and this model proves nothing",
    );
    explored
}

/// [`expect_violation`] with [`WRITE_FREE_PREEMPTIONS`] as an explicit bound.
fn expect_bounded_violation(name: &str, body: impl Fn() + Send + Sync + 'static) {
    println!("negative control `{name}`: loom must report a violation below");
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut builder = loom::model::Builder::new();
        builder.preemption_bound = Some(WRITE_FREE_PREEMPTIONS);
        builder.check(body);
    }));
    assert!(
        outcome.is_err(),
        "negative control `{name}` passed: the model cannot tell the mutated \
         protocol from the real one",
    );
    println!("negative control `{name}`: violation reported, as required");
}

/// Whether `view` can reach the fresh memtable: `reachable_ids` is a bit set
/// with bit `n` standing for memtable `n`, so the bit for [`FRESH`] says
/// whether the view still holds it (active, frozen, or flushed into the
/// version).
fn reaches_fresh(view: &View) -> bool {
    view.reachable_ids() & (1u16 << FRESH) != 0
}

/// The load order the write-free commit depends on (E50): its points (the
/// published horizon and the decided frontier) first, the view second
/// (`RegolithEngine::commit_write_free`, src/engine/commit/write_free.rs:118
/// and 129). It is the reverse of the read path's, and for a different
/// reason: a plain read is placed where it loaded its view, while a commit
/// that writes nothing is placed at its points, so every commit at or below
/// them must be reachable through the view it checks in.
///
/// The committer seals the active memtable and only then publishes its
/// sequence, the order the pipeline keeps: its write goes into the fresh
/// memtable before it is published. The model has no write of its own: that
/// the view reaches the fresh memtable stands for the view holding the write
/// in it. A checker whose horizon covers the commit must load a view that
/// reaches the fresh memtable.
#[test]
fn the_write_free_check_samples_the_horizon_before_it_loads_the_view() {
    run_bounded_model("horizon_then_view", 262, || {
        let cell = Arc::new(Cell::new(View::initial()));
        let horizon = Arc::new(AtomicU64::new(1));

        let committer = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                cell.rcu(|view| rotate(view, FRESH));
                horizon.store(2, Ordering::Release);
            })
        };

        let checker = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                let sampled = horizon.load(Ordering::Acquire);
                let view = cell.load();
                if sampled >= 2 {
                    assert!(
                        reaches_fresh(&view),
                        "the check's horizon covers the commit but its view \
                         misses the memtable the commit wrote to: {view:?}",
                    );
                }
            })
        };

        committer.join().unwrap();
        checker.join().unwrap();
    });
}

/// The negative control of the model above: the broken order, view first
/// and horizon second. The checker can load the old view, then see the
/// horizon cover the commit, and miss the write; loom must find it.
#[test]
fn loading_the_view_first_is_caught_by_the_write_free_model() {
    expect_bounded_violation("horizon_then_view/view_first", || {
        let cell = Arc::new(Cell::new(View::initial()));
        let horizon = Arc::new(AtomicU64::new(1));

        let committer = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                cell.rcu(|view| rotate(view, FRESH));
                horizon.store(2, Ordering::Release);
            })
        };

        let checker = {
            let cell = Arc::clone(&cell);
            let horizon = Arc::clone(&horizon);
            thread::spawn(move || {
                let view = cell.load();
                let sampled = horizon.load(Ordering::Acquire);
                if sampled >= 2 {
                    assert!(reaches_fresh(&view));
                }
            })
        };

        committer.join().unwrap();
        checker.join().unwrap();
    });
}

/// One slot of the commit pipeline as the write-free check meets it: a
/// decision installed, its operation applied to the memtable, then the tip
/// (the decided frontier) moved past it (`settle`,
/// src/engine/commit/pipeline.rs:671).
struct Slot {
    /// The slot holds a decision.
    decided: AtomicBool,
    /// Its operation is in the memtable.
    in_memtable: AtomicBool,
    /// The decided frontier: 1 before the slot, 2 once it is settled.
    tip: AtomicU64,
}

impl Slot {
    fn new() -> Self {
        Self {
            decided: AtomicBool::new(false),
            in_memtable: AtomicBool::new(false),
            tip: AtomicU64::new(1),
        }
    }

    /// Apply the slot and move the tip past it: what the deciding thread
    /// does, and what any helper does for it. Two threads may both do it,
    /// which is harmless (the apply is idempotent and only one moves the
    /// tip).
    fn settle(&self) {
        self.in_memtable.store(true, Ordering::Relaxed);
        let _ = self
            .tip
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
    }

    /// The pipeline's thread: install the decision, then settle the slot.
    fn decide_and_settle(&self) {
        self.decided.store(true, Ordering::Release);
        self.settle();
    }

    /// The write-free check: when the slot holds a decision, settle it
    /// (`apply_decided`, write_free.rs:203-216), then read the decided
    /// frontier, then look at the memtable. Returns whether a decision was
    /// there when the check began, the frontier it read, and whether the
    /// memtable held the write. `help` off is the calibration's broken check.
    fn check(&self, help: bool) -> (bool, u64, bool) {
        let decided = self.decided.load(Ordering::Acquire);
        if help && decided {
            self.settle();
        }
        let frontier = self.tip.load(Ordering::Acquire);
        let seen = self.in_memtable.load(Ordering::Relaxed);
        (decided, frontier, seen)
    }
}

/// E50 F2: a write-free check that begins after a commit is decided counts
/// it, applied or not. The check settles a decided slot itself before it
/// reads the decided frontier, so the frontier it reads is past that slot,
/// and a frontier past a slot means its write is in the memtable.
#[test]
fn the_write_free_check_counts_a_commit_decided_before_it() {
    run_bounded_model("apply_then_frontier", 57, || {
        let slot = Arc::new(Slot::new());
        let pipeline = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || slot.decide_and_settle())
        };
        let (decided, frontier, seen) = slot.check(true);
        if decided {
            assert_eq!(frontier, 2, "the check missed a commit decided before it");
        }
        if frontier >= 2 {
            assert!(
                seen,
                "the frontier counts a write the memtable does not hold"
            );
        }
        pipeline.join().unwrap();
    });
}

/// The negative control of the model above: a check that reads the decided
/// frontier without settling a decided slot first. It can begin after the
/// decision and before the apply, read the old frontier and miss the commit,
/// which a commit through the pipeline would be checked against; loom must
/// find it.
#[test]
fn reading_the_frontier_without_helping_is_caught() {
    expect_bounded_violation("apply_then_frontier/no_help", || {
        let slot = Arc::new(Slot::new());
        let pipeline = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || slot.decide_and_settle())
        };
        let (decided, frontier, _) = slot.check(false);
        if decided {
            assert_eq!(frontier, 2);
        }
        pipeline.join().unwrap();
    });
}

#[test]
fn the_packing_round_trips_every_field() {
    let view = View {
        active: 9,
        frozen: [1, 2, 3, 4],
        frozen_len: 4,
        flushed: 0xBEEF,
        gc_floor: 7,
        generation: 15,
    };
    assert_eq!(View::unpack(view.pack()), view);
    assert_eq!(View::unpack(View::initial().pack()), View::initial());
}

// ---------------------------------------------------------------------------
// Reclamation and freshness: what `ReadViewCell` relies on kovan for.
//
// kovan's epoch protocol is not loom-instrumented (see the module docs), so
// these two models transcribe the guarantee regolith builds on rather than
// kovan's code: a reader announces itself before it loads the published
// view, and a publisher frees the view it replaced only once no reader that
// could have loaded it is still announced. That is the shape of
// `ViewGuard` (pin, then load) against `Atom::compare_and_swap` (swap, then
// retire), with one reader standing in for every reservation slot.

/// The two views of the reclamation models and the cells around them.
struct Reclaim {
    /// Index of the published view.
    published: AtomicU64,
    /// Whether each view's memory has been given back.
    freed: [AtomicBool; 2],
    /// The reader's reservation: set before its load, cleared after its
    /// last use of the view.
    reading: AtomicBool,
}

impl Reclaim {
    fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            freed: [AtomicBool::new(false), AtomicBool::new(false)],
            reading: AtomicBool::new(false),
        }
    }
}

/// One reader against one publication. `wait_for_readers` is the real
/// rule; without it the publisher frees the view it replaced at once.
///
/// The announcement and the two cross-checks are read-modify-writes. kovan
/// orders a reservation against a retirement with sequentially consistent
/// fences, which loom models only as acquire/release; a read-modify-write
/// always reads the newest value of its word and synchronizes with the one
/// it reads, which gives the model the same "one side sees the other"
/// guarantee the fences give kovan.
fn reclaim_model(wait_for_readers: bool, deferred: &StdArc<StdAtomicUsize>) {
    let cell = Arc::new(Reclaim::new());

    let reader = {
        let cell = Arc::clone(&cell);
        thread::spawn(move || {
            cell.reading.swap(true, Ordering::AcqRel);
            let index = cell.published.fetch_add(0, Ordering::AcqRel) as usize;
            assert!(
                !cell.freed[index].load(Ordering::Acquire),
                "a reader read a view that was already freed"
            );
            cell.reading.swap(false, Ordering::AcqRel);
        })
    };

    let publisher = {
        let cell = Arc::clone(&cell);
        let deferred = StdArc::clone(deferred);
        thread::spawn(move || {
            let replaced = cell.published.swap(1, Ordering::AcqRel) as usize;
            if wait_for_readers && cell.reading.fetch_or(false, Ordering::AcqRel) {
                // A reader may hold it: the free waits for that reader.
                deferred.fetch_add(1, StdOrdering::Relaxed);
            } else {
                cell.freed[replaced].store(true, Ordering::Release);
            }
        })
    };

    reader.join().unwrap();
    publisher.join().unwrap();
}

/// A replaced view is freed only when no reader holds it.
#[test]
fn a_reader_never_reads_a_freed_view() {
    let deferred = StdArc::new(StdAtomicUsize::new(0));
    let seen = StdArc::clone(&deferred);
    run_model("reclaim_waits_for_readers", 8, move || {
        reclaim_model(true, &seen)
    });
    assert!(
        deferred.load(StdOrdering::Relaxed) > 0,
        "no schedule ever had a reader pinned across the publication"
    );
}

/// RED: freeing the replaced view without waiting for readers.
#[test]
fn freeing_without_waiting_for_readers_is_caught() {
    let deferred = StdArc::new(StdAtomicUsize::new(0));
    expect_violation("reclaim_waits_for_readers/free_at_once", move || {
        reclaim_model(false, &deferred)
    });
}

/// One publication that happened before a reader's load began, made known
/// to the reader by a release/acquire pair the way a commit's horizon is.
/// `cached_load` is the defect: a reader answering from a view it loaded
/// before it learned of the publication.
fn freshness_model(cached_load: bool, witnessed: &StdArc<StdAtomicUsize>) {
    let cell = Arc::new(Cell::new(View::initial()));
    let announced = Arc::new(AtomicBool::new(false));

    let publisher = {
        let cell = Arc::clone(&cell);
        let announced = Arc::clone(&announced);
        thread::spawn(move || {
            cell.rcu(|view| rotate(view, FRESH));
            announced.store(true, Ordering::Release);
        })
    };

    let reader = {
        let cell = Arc::clone(&cell);
        let announced = Arc::clone(&announced);
        let witnessed = StdArc::clone(witnessed);
        thread::spawn(move || {
            let early = cell.load();
            if announced.load(Ordering::Acquire) {
                let view = if cached_load { early } else { cell.load() };
                witnessed.fetch_add(1, StdOrdering::Relaxed);
                assert!(
                    view.generation >= 1,
                    "a load that began after a publication returned an older view"
                );
            }
        })
    };

    publisher.join().unwrap();
    reader.join().unwrap();
}

/// A load never returns a view older than one published before the load
/// began.
#[test]
fn a_load_after_a_publication_sees_it_or_newer() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    let seen = StdArc::clone(&witnessed);
    run_model("load_after_publication", 8, move || {
        freshness_model(false, &seen)
    });
    assert!(
        witnessed.load(StdOrdering::Relaxed) > 0,
        "no schedule ever had the reader learn of the publication"
    );
}

/// RED: answering from a view loaded before the reader learned of the
/// publication.
#[test]
fn a_reader_answering_from_an_earlier_load_is_caught() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    expect_violation("load_after_publication/cached_load", move || {
        freshness_model(true, &witnessed)
    });
}
