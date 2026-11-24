#![cfg(loom)]

//! loom models of an in-memory file's extents: the chain of segments an
//! append in place grows, the copy that replaces an extent, and the reads
//! that race both (`src/env/mem_file.rs`, `src/env/mem_file/extent.rs`).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_mem_file
//! ```
//!
//! # What these models check, and what they do not
//!
//! `MemFile` publishes its extent through a kovan `Atom`, which carries no
//! loom instrumentation (see `tests/loom_read_view.rs`), and an extent's
//! bytes are `std::cell::UnsafeCell<MaybeUninit<u8>>` copied with
//! `ptr::copy_nonoverlapping`, which loom cannot see either; reading room
//! nobody wrote is undefined behaviour there, not a value a model can assert
//! on. A model that called the real `MemFile` would run on hardware atomics
//! and check nothing.
//!
//! So these models transcribe, in loom atomics and loom cells, the protocol
//! `mem_file.rs` builds on the `Atom`: the state word (`LEN`, `WRITER`,
//! `FROZEN`), the segment chain and its Release link, the append in place
//! and its publishing CAS, the copy's freeze and swap, and the reader's
//! Acquire loads. Each function names the lines it transcribes. Every byte
//! is a loom cell made holding [`UNINIT`] when its segment is made, and
//! every segment's room is a loom cell too, so loom fails a model that reads
//! a byte, a segment's room or a segment's link its writer's write (or the
//! segment's making) is not ordered before; and a read that returns
//! [`UNINIT`] is room nobody wrote, published. The
//! transcription is the boundary of what is proved: a divergence between a
//! function here and the lines it names is not caught.
//!
//! Out of scope, and where it is checked instead:
//!
//! - **Reclamation.** No replaced extent is freed while a model runs; when
//!   one may be is kovan's guard contract, and that every use of an extent
//!   holds its guard is `proofs/tla/MemFileExtents.tla`'s NoUseAfterFree.
//! - **The settle loop** (`settled`, `mem_file.rs:452`): transcribed as one
//!   load of the state word. Its extra loads only look again later, which
//!   is another schedule of the same load.
//! - **`append_at` and the Charge**: they decide whether a write goes on
//!   before it touches the extent (`EnvFiles.tla` has `append_at`).
//!
//! Each model is paired with a calibration that plants one step wrong
//! ([`Mutant`]) and must fail.

use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::Arc as StdArc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use loom::thread;

/// The published length (`mem_file.rs:56`).
const LEN: u64 = (1 << 62) - 1;
/// An append in place holds the room above the length (`mem_file.rs:58`).
const WRITER: u64 = 1 << 62;
/// A copy replaced or is replacing this extent (`mem_file.rs:60`).
const FROZEN: u64 = 1 << 63;
/// The smallest segment a file grows by: 64 in `extent.rs:35`, one here, so
/// that a one-byte append to a full segment links a new one.
const MIN_CAPACITY: usize = 1;
/// What a byte of room holds until somebody writes it.
const UNINIT: u8 = 0xEE;

/// The one step a calibration plants wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutant {
    /// The transcribed protocol.
    None,
    /// The append in place publishes its length with `Relaxed`.
    RelaxedPublish,
    /// A copy swaps its extent in without freezing the old one.
    NoFreeze,
    /// A segment is linked with a `Relaxed` store.
    RelaxedLink,
    /// set_len does not zero what it exposes, in place or in a copy.
    NoZero,
    /// The copy's swap on the `Atom` is `Relaxed`.
    RelaxedSwap,
}

/// One buffer of an extent's bytes (`extent.rs:42`).
struct Segment {
    /// `bytes.len()` in Rust: a plain field written when the segment is
    /// made, read by whoever walks the chain.
    room: UnsafeCell<usize>,
    bytes: Box<[UnsafeCell<u8>]>,
    next: AtomicPtr<Segment>,
}

impl Segment {
    /// `Segment::with_capacity` (`extent.rs:48`): the room is unwritten.
    fn with_capacity(room: usize) -> Self {
        Self {
            room: UnsafeCell::new(room),
            bytes: (0..room).map(|_| UnsafeCell::new(UNINIT)).collect(),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn room(&self) -> usize {
        // SAFETY: written only when the segment is made.
        self.room.with(|room| unsafe { *room })
    }

    /// `Segment::next` (`extent.rs:74`): an Acquire load of the link.
    fn next(&self) -> Option<&Segment> {
        // SAFETY: a link points to a segment the chain owns until the
        // extent is dropped, after every thread of the model joined.
        unsafe { self.next.load(Ordering::Acquire).as_ref() }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: dropped after every thread of the model joined.
        let mut next = unsafe { self.next.unsync_load() };
        while !next.is_null() {
            // SAFETY: each link came from `Box::into_raw` and is owned by the
            // chain alone.
            let segment = unsafe { Box::from_raw(next) };
            next = unsafe { segment.next.unsync_load() };
            segment.next.store(ptr::null_mut(), Ordering::Relaxed);
        }
    }
}

/// One chain of segments and its state word (`extent.rs:95`).
struct Extent {
    first: Segment,
    state: AtomicU64,
}

// SAFETY: as `extent.rs:106`; loom checks every byte and room access.
unsafe impl Sync for Extent {}
// SAFETY: the same.
unsafe impl Send for Extent {}

impl Extent {
    /// `Extent::with_capacity` (`extent.rs:110`).
    fn with_capacity(room: usize) -> Self {
        Self {
            first: Segment::with_capacity(room),
            state: AtomicU64::new(0),
        }
    }

    /// A first extent holding `bytes` in one segment of `room`.
    fn holding(bytes: &[u8], room: usize) -> Self {
        let extent = Self::with_capacity(room);
        extent.copy_in(0, bytes);
        extent.state.store(bytes.len() as u64, Ordering::Relaxed);
        extent
    }

    fn segments(&self) -> impl Iterator<Item = &Segment> {
        std::iter::successors(Some(&self.first), |segment| segment.next())
    }

    /// `Extent::capacity` (`extent.rs:145`): the room of every segment
    /// linked so far.
    fn capacity(&self) -> usize {
        self.segments().map(Segment::room).sum()
    }

    /// `Extent::reserve` (`extent.rs:161-180`), by the thread holding
    /// `WRITER`: link one segment when the room ends before `end`.
    fn reserve(&self, end: usize, mutant: Mutant) {
        let mut room = 0;
        let mut tail = &self.first;
        loop {
            room += tail.room();
            match tail.next() {
                Some(next) => tail = next,
                None => break,
            }
        }
        if end <= room {
            return;
        }
        let grow = (end - room).max(room).max(MIN_CAPACITY);
        let segment = Box::into_raw(Box::new(Segment::with_capacity(grow)));
        let order = match mutant {
            Mutant::RelaxedLink => Ordering::Relaxed,
            _ => Ordering::Release,
        };
        tail.next.store(segment, order);
    }

    /// `Extent::pieces` (`extent.rs:189-215`), a byte at a time: `f` gets
    /// each byte of `[at, at + n)` and its offset in the range. Stops where
    /// the chain ends.
    fn pieces<F>(&self, mut at: usize, n: usize, mut f: F)
    where
        F: FnMut(&UnsafeCell<u8>, usize),
    {
        let mut done = 0;
        let mut segment = &self.first;
        while done < n {
            let len = segment.room();
            if at < len {
                let take = (n - done).min(len - at);
                for i in 0..take {
                    f(&segment.bytes[at + i], done + i);
                }
                done += take;
                at = 0;
            } else {
                at -= len;
            }
            if done < n {
                match segment.next() {
                    Some(next) => segment = next,
                    None => return,
                }
            }
        }
    }

    /// `Extent::copy_out` (`extent.rs:223`).
    fn copy_out(&self, at: usize, out: &mut [u8]) {
        let n = out.len();
        // SAFETY: loom checks the read is ordered after the byte's write.
        self.pieces(at, n, |cell, i| out[i] = cell.with(|byte| unsafe { *byte }));
    }

    /// `Extent::copy_in` (`extent.rs:241`).
    fn copy_in(&self, at: usize, src: &[u8]) {
        // SAFETY: loom checks no other access races the write.
        self.pieces(at, src.len(), |cell, i| {
            cell.with_mut(|byte| unsafe { *byte = src[i] })
        });
    }

    /// `Extent::zero` (`extent.rs:256`).
    fn zero(&self, at: usize, n: usize) {
        self.copy_in(at, &vec![0; n]);
    }

    /// `Extent::copy_prefix` (`extent.rs:267`).
    fn copy_prefix(&self, src: &Extent, n: usize) {
        let mut bytes = vec![0; n];
        src.copy_out(0, &mut bytes);
        self.copy_in(0, &bytes);
    }
}

/// What a write does (`mem_file.rs:90`).
enum Change {
    Append(Vec<u8>),
    SetLen(u64),
}

impl Change {
    /// `Change::bounds` (`mem_file.rs:105`).
    fn bounds(&self, len: u64) -> (u64, u64) {
        match self {
            Change::Append(bytes) => (len, len + bytes.len() as u64),
            Change::SetLen(new) => (len.min(*new), *new),
        }
    }

    /// `Change::fill` (`mem_file.rs:130`).
    fn fill(&self, extent: &Extent, at: usize) {
        if let Change::Append(bytes) = self {
            extent.copy_in(at, bytes);
        }
    }
}

/// `MemFile` (`mem_file.rs:163`), its `Atom` a loom pointer. Replaced
/// extents are kept until the model ends.
struct MemFile {
    current: AtomicPtr<Extent>,
    retired: StdMutex<Vec<*mut Extent>>,
    mutant: Mutant,
    /// Copies that walked a chain an append in place had grown.
    walked_growth: StdAtomicUsize,
}

// SAFETY: the extents are shared as `mem_file.rs` shares them.
unsafe impl Sync for MemFile {}
// SAFETY: the same.
unsafe impl Send for MemFile {}

impl MemFile {
    fn holding(bytes: &[u8], room: usize, mutant: Mutant) -> Self {
        Self {
            current: AtomicPtr::new(Box::into_raw(Box::new(Extent::holding(bytes, room)))),
            retired: StdMutex::new(Vec::new()),
            mutant,
            walked_growth: StdAtomicUsize::new(0),
        }
    }

    /// `to_vec` / `read_exact_at` of the whole file (`mem_file.rs:215-241`):
    /// load the extent, load its length with Acquire, copy below it.
    fn read_all(&self) -> Vec<u8> {
        // SAFETY: no extent is freed while the model runs.
        let extent = unsafe { &*self.current.load(Ordering::Acquire) };
        let len = (extent.state.load(Ordering::Acquire) & LEN) as usize;
        let mut out = vec![0; len];
        extent.copy_out(0, &mut out);
        out
    }

    /// `MemFile::change` (`mem_file.rs:282-330`).
    fn change(&self, change: &Change) {
        loop {
            let pointer = self.current.load(Ordering::Acquire);
            // SAFETY: no extent is freed while the model runs.
            let extent = unsafe { &*pointer };
            let state = extent.state.load(Ordering::Acquire);
            let len = state & LEN;
            let (at, end) = change.bounds(len);
            let in_place = state & (WRITER | FROZEN) == 0 && at == len && end >= len;
            let landed = if in_place {
                self.in_place(extent, state, change, len, end)
            } else {
                self.copy(pointer, extent, state, change, at, len, end)
            };
            if landed {
                return;
            }
        }
    }

    /// `MemFile::in_place` (`mem_file.rs:334-381`).
    fn in_place(&self, extent: &Extent, state: u64, change: &Change, len: u64, end: u64) -> bool {
        let (from, to) = (len as usize, end as usize);
        if extent
            .state
            .compare_exchange(state, state | WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        extent.reserve(to, self.mutant);
        if let Change::SetLen(_) = change
            && self.mutant != Mutant::NoZero
        {
            extent.zero(from, to - from);
        }
        change.fill(extent, from);
        let publish = match self.mutant {
            Mutant::RelaxedPublish => Ordering::Relaxed,
            _ => Ordering::Release,
        };
        extent
            .state
            .compare_exchange(state | WRITER, end, publish, Ordering::Relaxed)
            .is_ok()
    }

    /// `MemFile::copy` (`mem_file.rs:386-441`).
    #[allow(clippy::too_many_arguments)]
    fn copy(
        &self,
        pointer: *mut Extent,
        extent: &Extent,
        state: u64,
        change: &Change,
        at: u64,
        len: u64,
        end: u64,
    ) -> bool {
        let need = end as usize;
        let have = extent.capacity();
        if extent.first.next().is_some() {
            self.walked_growth.fetch_add(1, StdOrdering::Relaxed);
        }
        let capacity = if need <= have {
            have
        } else {
            need.max(have * 2).max(MIN_CAPACITY)
        };
        let fresh = Extent::with_capacity(capacity);
        let kept = len.min(end) as usize;
        let gap_end = match change {
            Change::SetLen(_) => need,
            Change::Append(_) => at as usize,
        };
        fresh.copy_prefix(extent, kept);
        if gap_end > kept && self.mutant != Mutant::NoZero {
            fresh.zero(kept, gap_end - kept);
        }
        change.fill(&fresh, at as usize);
        fresh.state.store(end, Ordering::Relaxed);
        if self.mutant != Mutant::NoFreeze
            && state & FROZEN == 0
            && extent
                .state
                .compare_exchange(state, state | FROZEN, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        let fresh = Box::into_raw(Box::new(fresh));
        let (swap, failed) = match self.mutant {
            Mutant::RelaxedSwap => (Ordering::Relaxed, Ordering::Relaxed),
            _ => (Ordering::AcqRel, Ordering::Acquire),
        };
        match self.current.compare_exchange(pointer, fresh, swap, failed) {
            Ok(old) => {
                self.retired.lock().expect("model lock").push(old);
                true
            }
            Err(_) => {
                // SAFETY: never published.
                drop(unsafe { Box::from_raw(fresh) });
                false
            }
        }
    }
}

impl Drop for MemFile {
    fn drop(&mut self) {
        // SAFETY: every thread of the model joined; each extent was boxed
        // once and is freed once.
        unsafe {
            drop(Box::from_raw(self.current.unsync_load()));
            for old in self.retired.lock().expect("model lock").drain(..) {
                drop(Box::from_raw(old));
            }
        }
    }
}

/// Run `model` under loom at `preemptions` (none: unbounded), and fail
/// unless it explored at least `floor` schedules and `witnessed` of them
/// reached the state it exists to check.
fn explore<M>(name: &str, preemptions: Option<usize>, floor: usize, witnessed: usize, model: M)
where
    M: Fn(&StdAtomicUsize) + Send + Sync + 'static,
{
    let runs = StdArc::new(StdAtomicUsize::new(0));
    let witness = StdArc::new(StdAtomicUsize::new(0));
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(preemptions);
    let (counted, seeing) = (StdArc::clone(&runs), StdArc::clone(&witness));
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        builder.check(move || {
            model(&seeing);
            counted.fetch_add(1, StdOrdering::Relaxed);
        })
    }));
    println!(
        "loom model {name}: {} interleavings explored, {} witnessed",
        runs.load(StdOrdering::Relaxed),
        witness.load(StdOrdering::Relaxed)
    );
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    let (explored, seen) = (
        runs.load(StdOrdering::Relaxed),
        witness.load(StdOrdering::Relaxed),
    );
    assert!(
        explored >= floor,
        "{name} explored only {explored} interleavings, below {floor}"
    );
    assert!(
        seen >= witnessed,
        "{name} reached its state {seen} times, below {witnessed}"
    );
}

/// Run a calibration and require it to fail with a message holding one of
/// `any_of`: a planted bug can show as a loom causality violation or as a
/// wrong value, depending on which schedule loom reaches first.
fn fails_with<M>(name: &str, preemptions: Option<usize>, any_of: &[&str], model: M)
where
    M: Fn(&StdAtomicUsize) + Send + Sync + 'static,
{
    let outcome =
        std::panic::catch_unwind(AssertUnwindSafe(|| explore(name, preemptions, 1, 0, model)));
    let panic = match outcome {
        Ok(()) => panic!("calibration {name} passed: the model cannot tell the planted bug"),
        Err(panic) => panic,
    };
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        any_of.iter().any(|expected| message.contains(expected)),
        "calibration {name} failed for another reason: {message}"
    );
    println!("calibration {name}: failed as required: {message}");
}

/// What loom reports for a read its write is not ordered before.
const CAUSALITY: &str = "Causality violation";
/// What a read reports when it returns a file that never was.
const NEVER_HELD: &str = "which the file never held";

/// A reader thread: one read of the whole file. Every actor of a model is a
/// spawned thread: loom 0.7 explores one schedule only when the model's own
/// thread loads an atomic that a spawned thread loads and then CASes.
fn reader(file: &Arc<MemFile>) -> thread::JoinHandle<Vec<u8>> {
    let file = Arc::clone(file);
    thread::spawn(move || file.read_all())
}

/// The panic of a read that returned uninitialized room.
const UNINIT_READ: &str = "a read returned room nobody wrote";

/// Assert `got` is one of `allowed` and holds no unwritten room.
fn assert_snapshot(got: &[u8], allowed: &[&[u8]]) {
    assert!(!got.contains(&UNINIT), "{UNINIT_READ}: {got:?}");
    assert!(
        allowed.contains(&got),
        "a read returned {got:?}, which the file never held"
    );
}

/// An empty file (`MemFile::new`: one segment of no room); one append of a
/// byte in place, which links a segment, races one read.
fn append_and_read(mutant: Mutant, witness: &StdAtomicUsize) {
    let file = Arc::new(MemFile::holding(&[], 0, mutant));
    let writer = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.change(&Change::Append(vec![1])))
    };
    let got = reader(&file).join().expect("reader");
    writer.join().expect("writer");
    if got == [1] {
        witness.fetch_add(1, StdOrdering::Relaxed);
    }
    assert_snapshot(&got, &[&[], &[1]]);
    assert_eq!(file.read_all(), [1], "the append is missing");
}

/// A read racing an append in place sees the file before or after it.
#[test]
fn a_read_racing_growth_in_place_sees_the_file_whole() {
    explore("mem file: append and read", None, 50, 10, |witness| {
        append_and_read(Mutant::None, witness)
    });
}

/// Calibration: a length published with `Relaxed` lets the reader copy the
/// byte before the append's write of it is ordered before the copy.
#[test]
fn calibration_a_relaxed_publish_exposes_an_unwritten_byte() {
    fails_with(
        "mem file: relaxed publish",
        None,
        &[CAUSALITY, NEVER_HELD, UNINIT_READ],
        |witness| append_and_read(Mutant::RelaxedPublish, witness),
    );
}

/// An empty file, two appends of a byte, one read. One append may meet the
/// other's WRITER and copy, freeze and swap; the other's publish then fails
/// and it appends again on the new extent.
fn two_appends(mutant: Mutant, witness: &StdAtomicUsize) {
    let file = Arc::new(MemFile::holding(&[], 0, mutant));
    let writers: Vec<_> = [1u8, 2]
        .into_iter()
        .map(|byte| {
            let file = Arc::clone(&file);
            thread::spawn(move || file.change(&Change::Append(vec![byte])))
        })
        .collect();
    let got = reader(&file).join().expect("reader");
    for writer in writers {
        writer.join().expect("writer");
    }
    let last = file.read_all();
    let mut sorted = last.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        [1, 2],
        "an append that returned is missing: {last:?}"
    );
    if got.len() == 1 {
        witness.fetch_add(1, StdOrdering::Relaxed);
    }
    assert_snapshot(&got, &[&[], &last[..1], &last]);
}

/// Two appends racing in place and through a copy both land once, and a
/// read sees the file as it stood at one moment.
#[test]
fn two_appends_land_once_each() {
    explore("mem file: two appends", Some(2), 3_000, 500, |witness| {
        two_appends(Mutant::None, witness)
    });
}

/// Calibration: a copy that does not freeze lets an append in place publish
/// on the extent it replaced, where nobody reads it.
#[test]
fn calibration_a_copy_without_freeze_loses_an_append() {
    fails_with(
        "mem file: no freeze",
        Some(2),
        &["an append that returned is missing"],
        |witness| two_appends(Mutant::NoFreeze, witness),
    );
}

/// A file of one byte in a full segment. An append in place links a
/// segment while set_len(3) copies (it meets WRITER) and walks the chain
/// for its capacity, or goes in place itself; a read races both.
fn growth_and_copy(mutant: Mutant, witness: &StdAtomicUsize) {
    let file = Arc::new(MemFile::holding(&[7], 1, mutant));
    let appender = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.change(&Change::Append(vec![1])))
    };
    let extender = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.change(&Change::SetLen(3)))
    };
    appender.join().expect("writer");
    extender.join().expect("writer");
    if file.walked_growth.load(StdOrdering::Relaxed) > 0 {
        witness.fetch_add(1, StdOrdering::Relaxed);
    }
    let last = file.read_all();
    assert_snapshot(&last, &[&[7, 1, 0], &[7, 0, 0, 1]]);
}

/// A copy that walks a chain an append in place is growing reads each
/// segment it finds after that segment was made.
#[test]
fn a_copy_walks_a_growing_chain_safely() {
    explore("mem file: growth and copy", Some(2), 100, 50, |witness| {
        growth_and_copy(Mutant::None, witness)
    });
}

/// Calibration: a segment linked with `Relaxed` is found by the copy's walk
/// before its making is ordered before the walk.
#[test]
fn calibration_a_relaxed_link_exposes_an_unmade_segment() {
    fails_with("mem file: relaxed link", Some(2), &[CAUSALITY], |witness| {
        growth_and_copy(Mutant::RelaxedLink, witness)
    });
}

/// A file of one byte in a full segment; set_len(3) in place links a
/// segment of two bytes and zeroes them, racing a read.
fn extend_and_read(mutant: Mutant, witness: &StdAtomicUsize) {
    let file = Arc::new(MemFile::holding(&[7], 1, mutant));
    let writer = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.change(&Change::SetLen(3)))
    };
    let got = reader(&file).join().expect("reader");
    writer.join().expect("writer");
    if got.len() == 3 {
        witness.fetch_add(1, StdOrdering::Relaxed);
    }
    assert_snapshot(&got, &[&[7], &[7, 0, 0]]);
    assert_snapshot(&file.read_all(), &[&[7, 0, 0]]);
}

/// An extension reads as zeros, never as the room it grew into.
#[test]
fn an_extension_reads_as_zeros() {
    explore("mem file: extend and read", None, 50, 10, |witness| {
        extend_and_read(Mutant::None, witness)
    });
}

/// Calibration: set_len that does not zero publishes room nobody wrote.
#[test]
fn calibration_an_extension_without_zeroing_exposes_room() {
    fails_with("mem file: no zero", None, &[UNINIT_READ], |witness| {
        extend_and_read(Mutant::NoZero, witness)
    });
}

/// A file of two bytes; set_len(1) copies, freezes and swaps a new extent
/// in, racing a read that may hold the old one.
fn shrink_and_read(mutant: Mutant, witness: &StdAtomicUsize) {
    let file = Arc::new(MemFile::holding(&[7, 8], 2, mutant));
    let writer = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.change(&Change::SetLen(1)))
    };
    let got = reader(&file).join().expect("reader");
    writer.join().expect("writer");
    if got == [7] {
        witness.fetch_add(1, StdOrdering::Relaxed);
    }
    assert_snapshot(&got, &[&[7, 8], &[7]]);
}

/// A read racing a replacement sees the old extent's file or the new one's.
#[test]
fn a_read_racing_a_replacement_sees_old_or_new() {
    explore("mem file: shrink and read", None, 50, 10, |witness| {
        shrink_and_read(Mutant::None, witness)
    });
}

/// Calibration: a swap made with `Relaxed` lets the reader load the new
/// extent before its bytes are ordered before its copy.
#[test]
fn calibration_a_relaxed_swap_exposes_an_unwritten_extent() {
    fails_with(
        "mem file: relaxed swap",
        None,
        &[CAUSALITY, NEVER_HELD],
        |witness| shrink_and_read(Mutant::RelaxedSwap, witness),
    );
}
