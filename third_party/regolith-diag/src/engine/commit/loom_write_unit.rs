//! Loom models of the write unit (`write_unit.rs`, E45, L58): the claim,
//! the park on a record and its wakes, a run's look after it counted in at
//! the gate, and the job a `commit_nowait` commit leaves on its queue.
//!
//! # The story
//!
//! A commit's owner encodes its record, then needs it in the log. One thread
//! at a time, the holder of the write unit, writes a run of encoded records
//! with one write. A thread whose record is in the holder's hands parks on
//! it, and must be woken: when the holder writes the record, or when the
//! holder lets the unit go and the record still waits. A thread that is
//! never woken sleeps for ever, and its commit with it. A run must also
//! never put bytes in flight over a range a sync has begun to make durable,
//! even the same bytes (WG4): so the holder looks at its records again once
//! it counted in at the gate. And a `commit_nowait` commit that leaves the
//! unit on its queue must be told when the unit is let go.
//!
//! # What runs the production code and what mirrors it
//!
//! The unit's word ([`WriteUnit`]: the claim, the notes, the let go, the
//! line job), the slot's flags, record buffer and park ([`Descriptor`]:
//! `fill`, `record`, `mark_quiet`, `park`, `register`, `wake`) and the log's
//! gate ([`Gate`]) are the production types: under `--cfg loom` their
//! atomics, cells and parking are loom's. The record buffer is a tracked
//! cell, so a holder that read a record before its owner finished encoding
//! it fails the model as a data race. The holder's few lines between them
//! (gather, count in, look again, write, mark, let go, tell) live on the
//! engine, which owns files loom cannot run, so the models repeat them
//! against the production cells, and the holder's test of whom to wake is
//! the production [`waits_for_the_unit`]. They are protocol models and say
//! so.
//!
//! # What was shrunk
//!
//! One record, two threads: the holder and the one that waits (or copies,
//! or leaves). Every model is bounded by an explicit preemption bound, never
//! by time; a park has no time out under loom, so a wake that never comes is
//! a deadlock loom reports. Each positive model has a calibration that gets
//! one step wrong and must fail its assertion; a calibration does not park
//! for real (a thread loom finds asleep for good stops the test process),
//! it registers as a park does and the model then asks whether anybody
//! woke it.

#![allow(unsafe_code)]

use std::time::Duration;

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use loom::thread;

use super::descriptor::{Descriptor, FILLED, FILLING, PARKED, WRITTEN, Work};
use super::write_unit::{QUEUED, WANTED, WriteUnit, waits_for_the_unit, wake_if_waiting};
use crate::engine::loom_model::explore_bounded;
use crate::engine::wal_gate::Gate;

/// A slot whose record nobody encoded yet.
fn unfilled_slot() -> Arc<Descriptor> {
    Arc::new(Descriptor::new(
        Work::Allocate {
            key: Vec::new(),
            n: 0,
            read_queue: None,
        },
        true,
    ))
}

/// The encoding of the models' record into a slot's buffer.
fn encode(buf: &mut Vec<u8>) -> std::io::Result<()> {
    buf.extend_from_slice(b"record");
    Ok(())
}

/// A slot whose record its owner encoded: what a holder may gather (D4).
fn filled_slot() -> Arc<Descriptor> {
    let desc = unfilled_slot();
    desc.fill(encode).expect("a record is encoded");
    desc
}

/// What a writer does to have its filled record in the log while the unit
/// may be another's: claim it and write the record itself, or park on the
/// record, asking the claim it saw to wake it (`bring_written` and
/// `park_on`). `true` when it parked at least once.
fn wait_for_record(unit: &WriteUnit, desc: &Descriptor) -> bool {
    let mut slept = false;
    loop {
        if desc.is(WRITTEN) {
            return slept;
        }
        match unit.try_claim() {
            Ok(claimed) => {
                // The unit is free: this thread's run writes the record.
                assert_eq!(desc.record(), Some(&b"record"[..]));
                let _ = desc.mark_quiet(WRITTEN);
                let _ = unit.release(claimed);
                return slept;
            }
            Err(seen) => {
                slept |= desc.park(Duration::ZERO, || {
                    desc.is(WRITTEN) || !unit.note(seen, WANTED)
                });
            }
        }
    }
}

/// WU4, the mark: the holder's run holds the record. It reads the record's
/// bytes, marks it written and lets the unit go, then wakes the threads
/// parked on it, while the owner parks on the record. The owner is always
/// woken: the mark and the park's flag are read-modify-writes of one word,
/// so one sees the other. The holder reads only finished bytes (D4).
///
/// Bound: 3 preemptions.
pub fn a_parked_writer_is_woken_when_its_record_is_written() {
    explore_bounded(
        "a_parked_writer_is_woken_when_its_record_is_written",
        Some(3),
        8,
        1,
        |witness| {
            let unit = Arc::new(WriteUnit::new(0));
            let desc = filled_slot();
            let claimed = unit.try_claim().expect("the holder claims first");
            let holder = {
                let (unit, desc) = (Arc::clone(&unit), Arc::clone(&desc));
                thread::spawn(move || {
                    // The run: the record's bytes go to the host.
                    assert_eq!(desc.record(), Some(&b"record"[..]));
                    let (_, parked) = desc.mark_quiet(WRITTEN);
                    let _ = unit.release(claimed);
                    if parked {
                        desc.wake();
                    }
                })
            };
            if wait_for_record(&unit, &desc) {
                witness.record();
            }
            holder.join().expect("the holder");
            assert!(desc.is(WRITTEN));
        },
    );
}

/// WU4, the let go: the holder's run does not hold the record (it was not
/// filled when the holder gathered). The holder lets the unit go, and wakes
/// the record's parked threads when a note asked it to. The owner parks on
/// the record after it noted the claim it saw as wanted. It is always woken,
/// claims the unit and writes the record: the note and the let go are
/// read-modify-writes of the unit's word.
///
/// Bound: 3 preemptions.
pub fn a_writer_parked_for_the_next_claim_is_woken_when_the_unit_is_let_go() {
    explore_bounded(
        "a_writer_parked_for_the_next_claim_is_woken_when_the_unit_is_let_go",
        Some(3),
        8,
        1,
        |witness| {
            let unit = Arc::new(WriteUnit::new(0));
            let desc = filled_slot();
            let claimed = unit.try_claim().expect("the holder claims first");
            let holder = {
                let (unit, desc) = (Arc::clone(&unit), Arc::clone(&desc));
                thread::spawn(move || {
                    // A run of another record, written; now the unit is let
                    // go, and the waiting are told (`tell_waiters`).
                    let notes = unit.release(claimed);
                    if notes & WANTED != 0 && waits_for_the_unit(desc.flags()) {
                        desc.wake();
                    }
                })
            };
            if wait_for_record(&unit, &desc) {
                witness.record();
            }
            holder.join().expect("the holder");
            assert!(desc.is(WRITTEN));
        },
    );
}

/// Calibration for
/// [`a_writer_parked_for_the_next_claim_is_woken_when_the_unit_is_let_go`]:
/// a holder that lets the unit go and tells nobody. An owner that noted the
/// claim and parked is never woken.
pub fn calibration_a_holder_that_tells_nobody_leaves_a_writer_asleep() {
    explore_bounded(
        "calibration_a_holder_that_tells_nobody_leaves_a_writer_asleep",
        Some(3),
        1,
        0,
        |_| {
            let unit = Arc::new(WriteUnit::new(0));
            let desc = filled_slot();
            let claimed = unit.try_claim().expect("the holder claims first");
            let holder = {
                let unit = Arc::clone(&unit);
                // The bug: the notes are not read.
                thread::spawn(move || {
                    let _ = unit.release(claimed);
                })
            };
            // The owner registers as a park does, without sleeping for
            // real (see the module docs).
            let seen = match unit.try_claim() {
                Ok(_) => {
                    holder.join().expect("the holder");
                    return;
                }
                Err(seen) => seen,
            };
            let sleeps = desc.register(|| desc.is(WRITTEN) || !unit.note(seen, WANTED));
            holder.join().expect("the holder");
            assert!(
                !(sleeps && desc.has_sleepers()),
                "a writer parked for the next claim was never woken"
            );
        },
    );
}

/// The run's write of one record: gathered when the record was filled and
/// unwritten (before the count in), counted in at the gate, and, unless
/// `look_again` is off, looked at again (WU3). `true` when its bytes went
/// out with the sync already begun.
fn run_of_one(gate: &Gate, desc: &Descriptor, synced: &AtomicBool, look_again: bool) -> bool {
    let gathered = desc.flags() & (FILLED | WRITTEN) == FILLED;
    if !gathered {
        return false;
    }
    let Some(entry) = gate.enter(0) else {
        return false;
    };
    let keep = !look_again || !desc.is(WRITTEN);
    // The run's bytes go to the host here.
    let under_the_sync = keep && synced.load(Ordering::SeqCst);
    gate.leave(entry);
    if keep {
        let _ = desc.mark_quiet(WRITTEN);
    }
    under_the_sync
}

/// WU3 and WG4: the holder gathered the record, filled and unwritten, and
/// counts in, while another thread's copy of it lands and a sync of it
/// begins. The run either counted in before the sync turned the gate (the
/// sync waits for it, and is woken when it leaves) or looks again after
/// and finds the record written: no run lands under the sync.
///
/// Bound: 3 preemptions.
pub fn a_run_never_lands_under_a_sync() {
    explore_bounded("a_run_never_lands_under_a_sync", Some(3), 8, 1, |witness| {
        let gate = Arc::new(Gate::new());
        let desc = filled_slot();
        let synced = Arc::new(AtomicBool::new(false));
        let holder = {
            let (gate, desc, synced) = (Arc::clone(&gate), Arc::clone(&desc), Arc::clone(&synced));
            thread::spawn(move || run_of_one(&gate, &desc, &synced, true))
        };
        // A copy of the record lands; the sync's prefix covers it.
        let _ = desc.mark_quiet(WRITTEN);
        let (wakes, waker) = counting();
        let parked = gate.settle(100, &waker).is_pending();
        if !parked {
            synced.store(true, Ordering::SeqCst);
        }
        let under_the_sync = holder.join().expect("the holder");
        assert!(!under_the_sync, "a run landed under the sync");
        if parked {
            witness.record();
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "a sync parked for a run in flight and was not woken when it left"
            );
        }
    });
}

/// Calibration for [`a_run_never_lands_under_a_sync`]: a run that writes
/// what it gathered without looking again once counted in. Counted in after
/// the turn, the sync does not wait for it, and it lands under the sync.
pub fn calibration_a_run_that_does_not_look_again_lands_under_the_sync() {
    explore_bounded(
        "calibration_a_run_that_does_not_look_again_lands_under_the_sync",
        Some(3),
        1,
        0,
        |_| {
            let gate = Arc::new(Gate::new());
            let desc = filled_slot();
            let synced = Arc::new(AtomicBool::new(false));
            let holder = {
                let (gate, desc, synced) =
                    (Arc::clone(&gate), Arc::clone(&desc), Arc::clone(&synced));
                thread::spawn(move || run_of_one(&gate, &desc, &synced, false))
            };
            let _ = desc.mark_quiet(WRITTEN);
            let (_, waker) = counting();
            if gate.settle(100, &waker).is_ready() {
                synced.store(true, Ordering::SeqCst);
            }
            let under_the_sync = holder.join().expect("the holder");
            assert!(!under_the_sync, "a run landed under the sync");
        },
    );
}

/// What a run's write touches in these models: a tracked cell, so two
/// threads writing runs at once is a data race loom fails.
struct Host(UnsafeCell<u64>);

// SAFETY: every access goes through the tracked cell, which is what checks
// that the claim ordered them; the calibration breaks that on purpose.
unsafe impl Sync for Host {}
// SAFETY: as above.
unsafe impl Send for Host {}

impl Host {
    fn write(&self) {
        // SAFETY: the caller holds the unit, or is the calibration.
        self.0.with_mut(|bytes| unsafe { *bytes += 1 });
    }
}

/// WU1: two threads try the unit at once; whoever claims it writes its run
/// and lets it go. Never two runs at once: the run's write is a tracked
/// cell, and a second holder would race the first.
///
/// Bound: 3 preemptions.
pub fn one_thread_at_a_time_holds_the_unit() {
    explore_bounded(
        "one_thread_at_a_time_holds_the_unit",
        Some(3),
        4,
        1,
        |witness| {
            let unit = Arc::new(WriteUnit::new(0));
            let host = Arc::new(Host(UnsafeCell::new(0)));
            let runs = Arc::new(AtomicUsize::new(0));
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    let (unit, host, runs) =
                        (Arc::clone(&unit), Arc::clone(&host), Arc::clone(&runs));
                    thread::spawn(move || {
                        if let Ok(claimed) = unit.try_claim() {
                            host.write();
                            runs.fetch_add(1, Ordering::SeqCst);
                            let _ = unit.release(claimed);
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().expect("a writer");
            }
            if runs.load(Ordering::SeqCst) == 2 {
                witness.record();
            }
        },
    );
}

/// Calibration for [`one_thread_at_a_time_holds_the_unit`]: a claim that is
/// a load and a store. Both threads find the unit free and write at once.
pub fn calibration_a_claim_that_is_a_load_and_a_store_writes_two_runs_at_once() {
    explore_bounded(
        "calibration_a_claim_that_is_a_load_and_a_store_writes_two_runs_at_once",
        Some(3),
        1,
        0,
        |_| {
            let word = Arc::new(AtomicU64::new(0));
            let host = Arc::new(Host(UnsafeCell::new(0)));
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    let (word, host) = (Arc::clone(&word), Arc::clone(&host));
                    thread::spawn(move || {
                        if word.load(Ordering::Acquire) == 0 {
                            word.store(1, Ordering::Release);
                            host.write();
                            word.store(0, Ordering::Release);
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().expect("a writer");
            }
        },
    );
}

/// Counts the wakes of a waker, as a queue would see them.
struct Wakes(Arc<AtomicUsize>);

impl std::task::Wake for Wakes {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting() -> (Arc<AtomicUsize>, std::task::Waker) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let waker = std::task::Waker::from(std::sync::Arc::new(Wakes(Arc::clone(&wakes))));
    (wakes, waker)
}

/// D53: a `commit_nowait` commit finds the unit claimed and leaves it on its
/// queue: it loads the line job, then notes the claim as queued. The holder
/// lets the unit go, reads the note, releases the line job and puts a fresh
/// one in its place. The job the commit waits on is always released: the
/// one it loaded, or, when a release came between its load and its note,
/// an earlier one that was already released.
///
/// Bound: 3 preemptions.
pub fn a_queued_commit_is_told_when_the_unit_is_let_go() {
    explore_bounded(
        "a_queued_commit_is_told_when_the_unit_is_let_go",
        Some(3),
        4,
        1,
        |witness| {
            let unit = Arc::new(WriteUnit::new(0));
            let claimed = unit.try_claim().expect("the holder claims first");
            let holder = {
                let unit = Arc::clone(&unit);
                thread::spawn(move || {
                    if unit.release(claimed) & QUEUED != 0 {
                        unit.tell_line();
                    }
                })
            };
            let queued = unit.queue_on(claimed);
            holder.join().expect("the holder");
            if let Some(job) = queued {
                witness.record();
                assert!(
                    job.is_done(),
                    "a commit left on its queue waits on a job nobody releases"
                );
            }
        },
    );
}

/// Calibration for [`a_queued_commit_is_told_when_the_unit_is_let_go`]: the
/// note goes on the claim before the line job is loaded. The holder lets go
/// in between, releases the job of that moment, and the commit loads the
/// fresh one, which nobody releases.
pub fn calibration_a_commit_that_notes_before_it_loads_waits_on_a_job_nobody_releases() {
    explore_bounded(
        "calibration_a_commit_that_notes_before_it_loads_waits_on_a_job_nobody_releases",
        Some(3),
        1,
        0,
        |_| {
            let unit = Arc::new(WriteUnit::new(0));
            let claimed = unit.try_claim().expect("the holder claims first");
            let holder = {
                let unit = Arc::clone(&unit);
                thread::spawn(move || {
                    if unit.release(claimed) & QUEUED != 0 {
                        unit.tell_line();
                    }
                })
            };
            let queued = unit.queue_on_noting_first(claimed);
            holder.join().expect("the holder");
            if let Some(job) = queued {
                assert!(
                    job.is_done(),
                    "a commit left on its queue waits on a job nobody releases"
                );
            }
        },
    );
}

/// D4, the encoding claim: the owner and a thread that needs the record
/// written, finding nobody encoding it, both set out to encode it
/// (`RegolithEngine::fill`). Exactly one claims the encoding and writes the
/// buffer; the record is whole once it is filled. The buffer is a tracked
/// cell, so two encoders at once would be a data race loom fails.
///
/// Bound: 3 preemptions.
pub fn one_thread_encodes_a_record() {
    explore_bounded("one_thread_encodes_a_record", Some(3), 4, 1, |witness| {
        let desc = unfilled_slot();
        let encoded = Arc::new(AtomicUsize::new(0));
        let helper = {
            let (desc, encoded) = (Arc::clone(&desc), Arc::clone(&encoded));
            thread::spawn(move || {
                if desc.fill(encode).expect("the helper's encoding") {
                    encoded.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        if desc.fill(encode).expect("the owner's encoding") {
            encoded.fetch_add(1, Ordering::SeqCst);
        } else {
            witness.record();
        }
        helper.join().expect("the helper");
        assert_eq!(
            encoded.load(Ordering::SeqCst),
            1,
            "a record was encoded twice, or not at all"
        );
        assert_eq!(desc.record(), Some(&b"record"[..]));
    });
}

/// Calibration for [`one_thread_encodes_a_record`]: an encoding claimed
/// with a load and a store. Both threads find the record unclaimed and
/// write its buffer at once.
pub fn calibration_an_encoding_claimed_with_a_load_and_a_store_writes_one_buffer_twice() {
    explore_bounded(
        "calibration_an_encoding_claimed_with_a_load_and_a_store_writes_one_buffer_twice",
        Some(3),
        1,
        0,
        |_| {
            let word = Arc::new(AtomicU64::new(0));
            let buffer = Arc::new(Host(UnsafeCell::new(0)));
            let encoders: Vec<_> = (0..2)
                .map(|_| {
                    let (word, buffer) = (Arc::clone(&word), Arc::clone(&buffer));
                    thread::spawn(move || {
                        if word.load(Ordering::Acquire) & FILLING == 0 {
                            word.store(FILLING, Ordering::Release);
                            buffer.write();
                        }
                    })
                })
                .collect();
            for encoder in encoders {
                encoder.join().expect("an encoder");
            }
        },
    );
}

/// D5, the fill's wake: a thread that needs the record finds another
/// encoding it and parks on it until it is filled. It is always woken: the
/// fill's flag and the park's flag are read-modify-writes of one word, so
/// one sees the other. What it then reads is the whole record (D4).
///
/// Bound: 3 preemptions.
pub fn a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled() {
    explore_bounded(
        "a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled",
        Some(3),
        4,
        1,
        |witness| {
            let desc = unfilled_slot();
            let owner = {
                let desc = Arc::clone(&desc);
                thread::spawn(move || {
                    let _ = desc.fill(encode).expect("the owner's encoding");
                })
            };
            if !desc.fill(encode).expect("the waiter's encoding")
                && desc.park(Duration::ZERO, || desc.is(FILLED))
            {
                witness.record();
            }
            owner.join().expect("the owner");
            assert_eq!(desc.record(), Some(&b"record"[..]));
        },
    );
}

/// Calibration for
/// [`a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled`]:
/// an encoder that sets the filled flag without reading the parked one. A
/// thread that parked on the record while it was being encoded is never
/// woken.
pub fn calibration_a_fill_that_wakes_nobody_leaves_a_writer_asleep() {
    explore_bounded(
        "calibration_a_fill_that_wakes_nobody_leaves_a_writer_asleep",
        Some(3),
        1,
        0,
        |_| {
            let desc = unfilled_slot();
            let owner = {
                let desc = Arc::clone(&desc);
                thread::spawn(move || {
                    // The bug: the flag is set, the parked are not woken.
                    if desc.claim(FILLING) & FILLING == 0 {
                        let _ = desc.mark_quiet(FILLED);
                    }
                })
            };
            let sleeps = desc.claim(FILLING) & FILLING != 0 && desc.register(|| desc.is(FILLED));
            owner.join().expect("the owner");
            assert!(
                !(sleeps && desc.has_sleepers()),
                "a writer parked on a record being encoded was never woken"
            );
        },
    );
}

/// What the fast path's write and a run's write of one record do once the
/// writer holds the record's bytes: count in at the gate, look again, read
/// the whole record, count out, mark it written. `true` when it wrote.
fn write_through_gate(gate: &Gate, desc: &Descriptor) -> bool {
    let Some(entry) = gate.enter(0) else {
        return false;
    };
    let wrote = !desc.is(WRITTEN);
    if wrote {
        assert_eq!(
            desc.record(),
            Some(&b"record"[..]),
            "a record went out half encoded"
        );
    }
    gate.leave(entry);
    if wrote {
        desc.mark(WRITTEN);
    }
    wrote
}

/// WU8, the fast path racing a claimer: the owner finds the unit quiet
/// (`WriteUnit::quiet`), encodes its record
/// and writes it by itself, while another thread claims the unit and writes
/// a run of the filled records it finds. Whoever writes reads the whole
/// record: the buffer is a tracked cell, encoded under the claim of its
/// encoding and read only once it is filled (D4). The record ends written,
/// once or twice, the same bytes either way.
///
/// Bound: 3 preemptions.
pub fn the_fast_path_racing_a_claimer_writes_only_whole_records() {
    explore_bounded(
        "the_fast_path_racing_a_claimer_writes_only_whole_records",
        Some(3),
        8,
        1,
        |witness| {
            let unit = Arc::new(WriteUnit::new(0));
            let gate = Arc::new(Gate::new());
            let desc = unfilled_slot();
            let claimer = {
                let (unit, gate, desc) = (Arc::clone(&unit), Arc::clone(&gate), Arc::clone(&desc));
                thread::spawn(move || {
                    let Ok(claimed) = unit.try_claim() else {
                        return false;
                    };
                    // The gather: a filled, unwritten record joins the run.
                    let wrote = desc.flags() & (FILLED | WRITTEN) == FILLED
                        && write_through_gate(&gate, &desc);
                    let _ = unit.release(claimed);
                    wrote
                })
            };
            // The owner: the fast path when the unit is quiet, else the
            // unit's way, here a copy (Await::Copy).
            let fast = unit.quiet();
            let _ = desc.fill(encode).expect("the owner's encoding");
            let owner_wrote = write_through_gate(&gate, &desc);
            let claimer_wrote = claimer.join().expect("the claimer");
            if fast && claimer_wrote {
                witness.record();
            }
            assert!(owner_wrote || claimer_wrote || desc.is(WRITTEN));
            assert!(desc.is(WRITTEN), "the record was never written");
        },
    );
}

/// Calibration for
/// [`the_fast_path_racing_a_claimer_writes_only_whole_records`]: a fast path
/// that marks its record filled before it encodes it. A claimer that
/// gathers it then reads the buffer while the owner writes it.
pub fn calibration_a_fast_path_that_marks_its_record_filled_first_lets_a_claimer_read_it_half_encoded()
 {
    explore_bounded(
        "calibration_a_fast_path_that_marks_its_record_filled_first_lets_a_claimer_read_it_half_encoded",
        Some(3),
        1,
        0,
        |_| {
            let unit = Arc::new(WriteUnit::new(0));
            let gate = Arc::new(Gate::new());
            let desc = unfilled_slot();
            let claimer = {
                let (unit, gate, desc) = (Arc::clone(&unit), Arc::clone(&gate), Arc::clone(&desc));
                thread::spawn(move || {
                    if let Ok(claimed) = unit.try_claim() {
                        if desc.flags() & (FILLED | WRITTEN) == FILLED {
                            write_through_gate(&gate, &desc);
                        }
                        let _ = unit.release(claimed);
                    }
                })
            };
            // The bug: filled is set before the bytes are there.
            let _ = desc.mark_quiet(FILLED);
            let _ = desc.fill(encode).expect("the owner's encoding");
            write_through_gate(&gate, &desc);
            claimer.join().expect("the claimer");
        },
    );
}

/// How a model's waiters note the claim and how its holders tell: the code
/// as it is, or one of the two ways it once lost a wake.
#[derive(Clone, Copy, PartialEq)]
enum Tell {
    /// `WriteUnit::note`, `WriteUnit::tell` and `wake_if_waiting` on every
    /// record: the production code.
    AsBuilt,
    /// The note trusted off a plain load.
    NoteTrustsALoad,
    /// One record woken, `WANTED` put back on the word for the next holder.
    FirstOnly,
}

/// A holder lets the unit go and tells, as Claim's drop and `tell_waiters`
/// do: the notes read in the swap, then every record still waiting woken.
fn let_go(unit: &WriteUnit, claimed: u64, records: &[&Descriptor], how: Tell) {
    let notes = unit.release(claimed);
    if how == Tell::FirstOnly {
        unit.tell_the_first_only(notes, records);
    } else if unit.tell(notes) {
        for desc in records {
            wake_if_waiting(desc);
        }
    }
}

/// The run of a holder that claimed the unit: from the first usable record,
/// in ring order, up to the first that is not, each marked written; then
/// the unit let go and told, and the run's sleepers woken (`try_batch`).
fn run_and_let_go(unit: &WriteUnit, claimed: u64, records: &[&Descriptor], how: Tell) {
    let mut started = false;
    let mut parked = Vec::new();
    for desc in records {
        if desc.flags() & (FILLED | WRITTEN) != FILLED {
            if started {
                break;
            }
            continue;
        }
        started = true;
        let (_, sleeping) = desc.mark_quiet(WRITTEN);
        if sleeping {
            parked.push(*desc);
        }
    }
    let_go(unit, claimed, records, how);
    for desc in parked {
        desc.wake();
    }
}

/// Whether a waiter that saw the claim `seen` should sleep on its record
/// `mine`: it is on the list with `PARKED` set, and looks again, its note
/// on the claim part of the look (`park_on`).
fn should_sleep(unit: &WriteUnit, mine: &Descriptor, seen: u64, how: Tell) -> bool {
    mine.register(|| {
        mine.is(WRITTEN)
            || !match how {
                Tell::NoteTrustsALoad => unit.note_trusting_a_load(seen, WANTED),
                Tell::AsBuilt | Tell::FirstOnly => unit.note(seen, WANTED),
            }
    })
}

/// One writer of a filled record, as `bring_written` with `Await::Park` goes
/// about it: claim the unit and write a run, or park on its record with a
/// note on the claim it saw, and look again when woken. `records` are every
/// record of the model, in ring order.
fn writer(unit: &WriteUnit, mine: &Descriptor, records: &[&Descriptor]) {
    while !mine.is(WRITTEN) {
        match unit.try_claim() {
            Ok(claimed) => run_and_let_go(unit, claimed, records, Tell::AsBuilt),
            Err(seen) => {
                if should_sleep(unit, mine, seen, Tell::AsBuilt) {
                    thread::park();
                }
            }
        }
    }
}

/// A calibration's writer: one try, which writes a run or registers to
/// sleep without sleeping (a thread loom finds asleep for good stops the
/// test process). `true` when it would sleep.
fn writer_once(unit: &WriteUnit, mine: &Descriptor, records: &[&Descriptor], how: Tell) -> bool {
    match unit.try_claim() {
        Ok(claimed) => {
            run_and_let_go(unit, claimed, records, how);
            false
        }
        Err(seen) => should_sleep(unit, mine, seen, how),
    }
}

/// The two lost wakes the review found, as one scenario. A holder holds
/// the unit; records F, X and R are filled, in ring order, and X is
/// written already, so a run that starts at F stops before R. W1 writes F
/// and W2 writes R, each claiming the unit or parking with a note on the
/// claim it saw. Every writer ends with its record written: a parked one is
/// always woken, or its note is refused and it looks again. With the code
/// as built a lost wake leaves a thread asleep for good, which loom
/// reports; a calibration's writers register without sleeping, and the
/// model asks whether anybody woke them.
fn two_writers_behind_a_holder(name: &'static str, how: Tell, min: usize, witnessed: usize) {
    explore_bounded(name, Some(3), min, witnessed, move |witness| {
        let unit = Arc::new(WriteUnit::new(0));
        let (f, x, r) = (filled_slot(), filled_slot(), filled_slot());
        let _ = x.mark_quiet(WRITTEN);
        let claimed = unit.try_claim().expect("the holder claims first");
        let spawn = |mine: Arc<Descriptor>| {
            let (unit, f, x, r) = (
                Arc::clone(&unit),
                Arc::clone(&f),
                Arc::clone(&x),
                Arc::clone(&r),
            );
            thread::spawn(move || {
                let records = [&*f, &*x, &*r];
                if how == Tell::AsBuilt {
                    writer(&unit, &mine, &records);
                    false
                } else {
                    writer_once(&unit, &mine, &records, how)
                }
            })
        };
        let w1 = spawn(Arc::clone(&f));
        let w2 = spawn(Arc::clone(&r));
        let_go(&unit, claimed, &[&f, &x, &r], how);
        let asleep = [(w1.join().expect("W1"), &f), (w2.join().expect("W2"), &r)];
        if how == Tell::AsBuilt {
            if f.is(PARKED) && r.is(PARKED) {
                witness.record();
            }
            assert!(f.is(WRITTEN) && r.is(WRITTEN));
        }
        for (sleeps, desc) in asleep {
            assert!(
                !(sleeps && desc.has_sleepers()),
                "a writer behind the holder was left asleep"
            );
        }
    });
}

/// WU4 on the production note, register, park, mark, release, tell and
/// wake: two writers behind a holder are never left asleep.
///
/// Bound: 3 preemptions.
pub fn two_writers_behind_a_holder_are_never_left_asleep() {
    two_writers_behind_a_holder(
        "two_writers_behind_a_holder_are_never_left_asleep",
        Tell::AsBuilt,
        8,
        1,
    );
}

/// Calibration for [`two_writers_behind_a_holder_are_never_left_asleep`]:
/// the note trusted off a plain load. W2 reads the holder's claim with
/// W1's note on it after that claim was let go, registers to sleep on a
/// note nobody will read, and nobody wakes it.
pub fn calibration_a_note_trusted_off_a_load_leaves_a_writer_asleep() {
    two_writers_behind_a_holder(
        "calibration_a_note_trusted_off_a_load_leaves_a_writer_asleep",
        Tell::NoteTrustsALoad,
        1,
        0,
    );
}

/// Calibration for [`two_writers_behind_a_holder_are_never_left_asleep`]:
/// a holder that wakes the first waiting record only and puts `WANTED`
/// back on the free unit. Nobody claims the unit again, and the writer of
/// the second record is never woken.
pub fn calibration_a_holder_that_wakes_the_first_record_only_leaves_a_writer_asleep() {
    two_writers_behind_a_holder(
        "calibration_a_holder_that_wakes_the_first_record_only_leaves_a_writer_asleep",
        Tell::FirstOnly,
        1,
        0,
    );
}
