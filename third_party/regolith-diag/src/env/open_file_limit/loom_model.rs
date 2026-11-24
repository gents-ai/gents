//! Loom models of the open-file slot table: acquire, evict and release.
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_tables
//! ```
//!
//! Each model drives the production [`SlotTable`], whose state words, owner
//! words and descriptor cells come from `crate::sync::internal` and so are
//! loom's under `--cfg loom`. The descriptor is a [`ModelFile`] that knows
//! when it is being read and fails the model if it is closed (dropped) then,
//! or read after. What each proves:
//!
//! - **Never above the limit**: every model counts the descriptors alive at
//!   once and fails above the capacity.
//! - **Never closed in use**: a reader inside a read and a claimer evicting
//!   the same slot race; the claim waits for the read or takes another slot.
//! - **A reader reads its own file**: a reader joining from a stale hint
//!   races the slot being reloaded for another file; it reads its own bytes
//!   or misses, never the other file's.
//! - **A starved reader drains**: with one slot held by a reader, a second
//!   file's open waits for that read and then takes the slot.
//! - **A parked reopen is never forgotten** (D60): the unit a `CacheOnly`
//!   read queued runs its reopen without waiting while another thread's
//!   read holds the only slot, or loads it, and frees it. The unit parks or
//!   reads; once the slot is free, its queue holds the message that sends it
//!   back to run, and the run then reads. The production `Unit`,
//!   `QueueShared` and slot table are driven, the work standing in for
//!   `OpenFileLimit::hold` in a unit run.
//!
//! Each is paired with a calibration that plants one step wrong
//! ([`Mutant`]) and must fail.

use std::io;
use std::num::NonZeroU64;
use std::sync::Arc as StdArc;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;

use super::slots::{Mutant, SlotTable, SlotWaiter};
use crate::engine::block_cache::BlockCache;
use crate::engine::io::shared::{Message, QueueShared};
use crate::engine::io::unit::{Ran, Unit, UnitKey};
use crate::engine::loom_model::{Witness, explore, explore_bounded};
use crate::env::ReadFile;
use crate::io_queue::QueueId;

/// What every file of one model run shares: how many are open right now.
struct Device {
    open: AtomicUsize,
    capacity: usize,
}

/// A descriptor for file `id`: a read fills the buffer with `id`, and the
/// file fails the model if it is closed during a read or read after a close.
struct ModelFile {
    id: u8,
    device: Arc<Device>,
    reading: AtomicUsize,
    closed: AtomicBool,
}

impl ModelFile {
    fn open(id: u8, device: &Arc<Device>) -> io::Result<StdArc<dyn ReadFile>> {
        let now = device.open.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(
            now <= device.capacity,
            "{now} descriptors open, over the capacity {}",
            device.capacity
        );
        Ok(StdArc::new(Self {
            id,
            device: Arc::clone(device),
            reading: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }))
    }
}

impl ReadFile for ModelFile {
    fn read_exact_at(&self, _offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.reading.fetch_add(1, Ordering::SeqCst);
        assert!(!self.closed.load(Ordering::SeqCst), "read a closed file");
        buf.fill(self.id);
        assert!(
            !self.closed.load(Ordering::SeqCst),
            "a file was closed in use"
        );
        self.reading.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(1)
    }
}

impl Drop for ModelFile {
    fn drop(&mut self) {
        assert_eq!(
            self.reading.load(Ordering::SeqCst),
            0,
            "a file was closed in use"
        );
        self.closed.store(true, Ordering::SeqCst);
        self.device.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Read file `owner`'s byte through `table`: join the hinted slot, or load
/// a slot. Returns the byte read.
fn read(table: &SlotTable, device: &Arc<Device>, owner: u64, hint: usize) -> u8 {
    let held = match table.join(hint, owner) {
        Some(held) => held,
        None => table
            .load(owner, || ModelFile::open(owner as u8, device))
            .expect("model opens never fail"),
    };
    let mut byte = [0u8; 1];
    held.file().read_exact_at(0, &mut byte).expect("model read");
    byte[0]
}

fn setup(capacity: usize, mutant: Mutant) -> (Arc<SlotTable>, Arc<Device>) {
    (
        Arc::new(SlotTable::with_mutant(capacity, mutant)),
        Arc::new(Device {
            open: AtomicUsize::new(0),
            capacity,
        }),
    )
}

/// One slot; file 1 is loaded and read again from its hint while file 2's
/// load evicts it. Each reads its own byte, at most one descriptor is open,
/// and none is closed in use.
fn acquire_evict_release(mutant: Mutant, witness: &Witness) {
    let (table, device) = setup(1, mutant);
    // File 1 is in slot 0, idle.
    assert_eq!(read(&table, &device, 1, usize::MAX), 1);
    let first = {
        let (table, device) = (Arc::clone(&table), Arc::clone(&device));
        thread::spawn(move || read(&table, &device, 1, 0))
    };
    let second = {
        let (table, device) = (Arc::clone(&table), Arc::clone(&device));
        thread::spawn(move || read(&table, &device, 2, 0))
    };
    let (one, two) = (
        first.join().expect("reader 1"),
        second.join().expect("reader 2"),
    );
    assert_eq!(one, 1, "file 1's reader read file {one}");
    assert_eq!(two, 2, "file 2's reader read file {two}");
    assert!(table.open_count() <= 1);
    // The interesting schedules: file 1's second read missed its hint
    // because file 2 took the slot.
    if table.join(0, 2).is_some() {
        witness.record();
    }
}

/// Two files share one slot: loads, evictions and hinted joins race. Every
/// read returns its own file, the bound holds, nothing is closed in use.
pub fn a_slot_is_never_closed_in_use_and_never_over_the_limit() {
    explore(
        "a_slot_is_never_closed_in_use_and_never_over_the_limit",
        20,
        1,
        |witness| acquire_evict_release(Mutant::None, witness),
    );
}

/// File 1 holds the only slot mid-read; file 2's open drains it: it waits
/// for that one read and then takes the slot.
pub fn a_starved_open_drains_a_busy_slot() {
    explore("a_starved_open_drains_a_busy_slot", 10, 1, |witness| {
        let (table, device) = setup(1, Mutant::None);
        let held = table
            .load(1, || ModelFile::open(1, &device))
            .expect("model open");
        let opener = {
            let (table, device) = (Arc::clone(&table), Arc::clone(&device));
            thread::spawn(move || read(&table, &device, 2, usize::MAX))
        };
        let mut byte = [0u8; 1];
        held.file().read_exact_at(0, &mut byte).expect("model read");
        assert_eq!(byte[0], 1);
        drop(held);
        assert_eq!(opener.join().expect("opener"), 2);
        assert!(table.open_count() <= 1);
        witness.record();
    });
}

/// Calibration: a claim that takes an open slot whatever its readers closes
/// a file a reader is using. Must fail.
pub fn calibration_an_eviction_that_ignores_readers_closes_a_file_in_use() {
    explore(
        "calibration_an_eviction_that_ignores_readers_closes_a_file_in_use",
        1,
        0,
        |witness| acquire_evict_release(Mutant::IgnoreReaders, witness),
    );
}

/// Calibration: a join that trusts the owner it saw before its CAS reads
/// another file when the slot is reloaded in between. Must fail.
pub fn calibration_a_join_without_the_owner_recheck_reads_another_file() {
    explore(
        "calibration_a_join_without_the_owner_recheck_reads_another_file",
        1,
        0,
        |witness| acquire_evict_release(Mutant::NoOwnerRecheck, witness),
    );
}

/// The unit a `CacheOnly` read of file 2 queued. Its work reopens file 2
/// through `table` without waiting, as `OpenFileLimit::hold` does inside a
/// unit run, parking on `queue`; `parked` stands for `scope::note_parked`.
/// A run that gets the slot reads file 2 and finishes the unit.
fn reopen_unit(
    table: &Arc<SlotTable>,
    device: &Arc<Device>,
    queue: &StdArc<QueueShared>,
    parked: &Arc<AtomicBool>,
) -> StdArc<Unit> {
    let (table, device, queue, parked) = (
        Arc::clone(table),
        Arc::clone(device),
        StdArc::clone(queue),
        Arc::clone(parked),
    );
    let key = UnitKey {
        file_id: 2,
        offset: 0,
        guard: None,
    };
    StdArc::new(Unit::new(
        key,
        1,
        Box::new(move |_| {
            parked.store(false, Ordering::SeqCst);
            let waiter = StdArc::clone(&queue) as StdArc<dyn SlotWaiter>;
            match table.load_or_park(2, waiter, || ModelFile::open(2, &device))? {
                Some(held) => {
                    let mut byte = [0u8; 1];
                    held.file().read_exact_at(0, &mut byte)?;
                    assert_eq!(byte[0], 2, "the reopen read file {}", byte[0]);
                    Err(io::Error::other("modelled read"))
                }
                None => {
                    parked.store(true, Ordering::SeqCst);
                    Err(io::Error::other("no open-file slot"))
                }
            }
        }),
    ))
}

/// One poll's run of `unit`: claim it and run it. Whether it parked.
fn first_run(unit: &StdArc<Unit>, parked: &AtomicBool, cache: &BlockCache) -> bool {
    assert!(unit.claim(), "nobody else runs the unit");
    unit.run(cache, || parked.load(Ordering::SeqCst)) == Ran::Parked
}

/// The owner's next poll, once the slot is free: a `SlotFreed` in the inbox
/// unparks the unit, which runs again and must read. Whether one came.
fn next_poll(
    queue: &QueueShared,
    unit: &StdArc<Unit>,
    parked: &AtomicBool,
    cache: &BlockCache,
) -> bool {
    let told = queue
        .take_inbox()
        .any(|message| matches!(message, Message::SlotFreed));
    if told && unit.unpark() {
        assert!(unit.claim());
        let ran = unit.run(cache, || parked.load(Ordering::SeqCst));
        assert_eq!(
            ran,
            Ran::Finished,
            "a woken unit parked again on a free slot"
        );
    }
    told
}

/// The preemption bound of the parked-reopen models. Each has two threads,
/// and twelve is where their search stops growing: the leave model
/// explores the same 16 schedules as with no bound, the load model the
/// same 24,056, so the bound cuts nothing off today and keeps a later,
/// longer scenario from running for hours.
const PARK_PREEMPTIONS: Option<usize> = Some(12);

fn queue() -> StdArc<QueueShared> {
    StdArc::new(QueueShared::new(QueueId::new(NonZeroU64::MIN), 1 << 20))
}

/// File 1's reader holds the only slot mid-read and then leaves, while a
/// unit's reopen of file 2 runs on another thread without waiting. The unit
/// reads or parks; once the reader is gone, the owner's next poll finds it
/// done or finds the message that sends it back, and it reads then.
fn parked_reopen_woken_by_a_leave(mutant: Mutant, witness: &Witness) {
    let (table, device) = setup(1, mutant);
    let queue = queue();
    let parked = Arc::new(AtomicBool::new(false));
    let unit = reopen_unit(&table, &device, &queue, &parked);
    let cache = StdArc::new(BlockCache::with_config(0, 0, false));
    let held = table
        .load(1, || ModelFile::open(1, &device))
        .expect("model open");
    let runner = {
        let (unit, parked, cache) = (
            StdArc::clone(&unit),
            Arc::clone(&parked),
            StdArc::clone(&cache),
        );
        thread::spawn(move || first_run(&unit, &parked, &cache))
    };
    let mut byte = [0u8; 1];
    held.file().read_exact_at(0, &mut byte).expect("model read");
    assert_eq!(byte[0], 1);
    drop(held);
    let first_parked = runner.join().expect("runner");
    let told = next_poll(&queue, &unit, &parked, &cache);
    assert!(
        unit.is_done(),
        "a freed slot was missed: the parked reopen never ran"
    );
    assert!(table.open_count() <= 1);
    if first_parked && told {
        witness.record();
    }
}

/// File 1's open loads the only slot (claimed, then published), reads and
/// leaves, while a unit's reopen of file 2 runs without waiting. A park
/// that marks the slot while it is claimed must keep its mark through the
/// publish, so the loader's leave wakes it.
fn parked_reopen_woken_through_a_load(mutant: Mutant, witness: &Witness) {
    let (table, device) = setup(1, mutant);
    let queue = queue();
    let parked = Arc::new(AtomicBool::new(false));
    let unit = reopen_unit(&table, &device, &queue, &parked);
    let cache = StdArc::new(BlockCache::with_config(0, 0, false));
    let loader = {
        let (table, device) = (Arc::clone(&table), Arc::clone(&device));
        thread::spawn(move || read(&table, &device, 1, usize::MAX))
    };
    let first_parked = first_run(&unit, &parked, &cache);
    assert_eq!(loader.join().expect("loader"), 1);
    let told = next_poll(&queue, &unit, &parked, &cache);
    assert!(
        unit.is_done(),
        "a freed slot was missed: the parked reopen never ran"
    );
    if first_parked && told {
        witness.record();
    }
}

/// D60: a reopen that may not wait, racing the read that frees the only
/// slot, is never forgotten.
pub fn a_parked_reopen_is_woken_by_the_read_that_frees_its_slot() {
    explore_bounded(
        "a_parked_reopen_is_woken_by_the_read_that_frees_its_slot",
        PARK_PREEMPTIONS,
        10,
        1,
        |witness| parked_reopen_woken_by_a_leave(Mutant::None, witness),
    );
}

/// D60: a park that marks a slot while another open loads it is woken when
/// that open's read leaves.
pub fn a_parked_reopen_is_woken_through_a_load_of_its_slot() {
    explore_bounded(
        "a_parked_reopen_is_woken_through_a_load_of_its_slot",
        PARK_PREEMPTIONS,
        20,
        1,
        |witness| parked_reopen_woken_through_a_load(Mutant::None, witness),
    );
}

/// Calibration: a park that marks the busy slot before it puts its waiter
/// on the list; the reader leaves in between, wakes an empty list, and the
/// unit parks with nobody left to wake it. Must fail.
pub fn calibration_a_park_that_marks_before_it_registers_misses_the_free() {
    explore_bounded(
        "calibration_a_park_that_marks_before_it_registers_misses_the_free",
        PARK_PREEMPTIONS,
        1,
        0,
        |witness| parked_reopen_woken_by_a_leave(Mutant::MarkBeforeRegister, witness),
    );
}

/// Calibration: a publish that stores the slot's word whole drops the mark
/// a park set while the slot was claimed; the loader's leave wakes nobody.
/// Must fail.
pub fn calibration_a_publish_that_drops_the_mark_misses_the_free() {
    explore_bounded(
        "calibration_a_publish_that_drops_the_mark_misses_the_free",
        PARK_PREEMPTIONS,
        1,
        0,
        |witness| parked_reopen_woken_through_a_load(Mutant::PublishDropsWanted, witness),
    );
}
