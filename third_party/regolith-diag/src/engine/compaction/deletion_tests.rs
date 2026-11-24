//! D14: a key range that was written and then deleted is compacted for its
//! deletions alone, by an ordinary pass nobody asked for, and nothing else
//! is.
//!
//! Costs are counted, never timed: the blocks a scan loads (block cache
//! lookups, hits and misses together), the entries of the tables under a
//! range, the compaction passes run.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::{CompactionOutcome, CompactionStyle, CompressionType, Db, Options, Statistics, Ticker};

struct Fixture {
    db: Db,
    stats: Arc<Statistics>,
    _dir: TempDir,
}

/// A database with its statistics, on small blocks so a scan's cost shows
/// in blocks.
fn open(options: Options) -> Fixture {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(
        dir.path(),
        options.block_size(512).statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    Fixture {
        db,
        stats,
        _dir: dir,
    }
}

/// No worker: a pass runs only in the step a write owes, or when the test
/// calls `compact_step`.
fn no_worker() -> Options {
    Options::default().max_background_compactions(0)
}

fn key(prefix: &str, i: u32) -> Vec<u8> {
    format!("{prefix}{i:04}").into_bytes()
}

impl Fixture {
    /// Put `prefix0000..` up to `count` keys, each with a 64-byte value.
    fn fill(&self, prefix: &str, count: u32) {
        for i in 0..count {
            self.db.put(&key(prefix, i), &[b'v'; 64]).unwrap();
        }
    }

    fn delete(&self, prefix: &str, keys: std::ops::Range<u32>) {
        for i in keys {
            self.db.delete(&key(prefix, i)).unwrap();
        }
    }

    fn flush(&self) {
        self.db.flush().unwrap();
    }

    /// Run passes until one finds nothing to do, and return how many ran.
    fn settle(&self) -> u64 {
        let before = self.passes();
        while self.db.compact_step().unwrap() == CompactionOutcome::DidWork {}
        self.passes() - before
    }

    /// Every compaction pass so far, whatever triggered it.
    fn passes(&self) -> u64 {
        self.stats.get_ticker(Ticker::CompactionCount)
    }

    /// The passes run for their deletions.
    fn deletion_passes(&self) -> u64 {
        self.stats.get_ticker(Ticker::CompactionDeletionTriggered)
    }

    /// The rows a scan of `[prefix, prefix + 0xff)` returns and the blocks
    /// it loaded to find them.
    fn scan(&self, prefix: &str) -> (usize, u64) {
        let lookups = || {
            self.stats.get_ticker(Ticker::BlockCacheHit)
                + self.stats.get_ticker(Ticker::BlockCacheMiss)
        };
        let before = lookups();
        let rows = self
            .db
            .scan(Some(prefix.as_bytes()), Some(&end_of(prefix)))
            .unwrap()
            .len();
        (rows, lookups() - before)
    }

    /// The entries of every table whose key range meets `prefix`: what a
    /// scan of the prefix has to walk at most.
    fn entries_under(&self, prefix: &str) -> u64 {
        let lo = crate::column_family::prefix_key(
            crate::column_family::DEFAULT_CF_ID,
            prefix.as_bytes(),
        );
        let hi =
            crate::column_family::prefix_key(crate::column_family::DEFAULT_CF_ID, &end_of(prefix));
        self.tables()
            .iter()
            .filter(|table| table.meta.smallest_key < hi && lo <= table.meta.largest_key)
            .map(|table| table.meta.num_entries)
            .sum()
    }

    /// Every table of the current version, shallowest level first.
    fn tables(&self) -> Vec<Arc<LiveSst>> {
        self.db
            .engine()
            .published_version()
            .levels
            .iter()
            .flatten()
            .cloned()
            .collect()
    }

    fn file_ids(&self) -> Vec<u64> {
        self.tables()
            .iter()
            .map(|table| table.meta.file_id)
            .collect()
    }

    /// Point deletions and range tombstones the tables still hold.
    fn deletions_held(&self) -> u64 {
        self.tables()
            .iter()
            .map(|table| {
                table.meta.deletions.map_or(0, |deletions| deletions.points)
                    + table.reader.range_tombstones().len() as u64
            })
            .sum()
    }
}

/// The exclusive end of the keys beginning with `prefix`.
fn end_of(prefix: &str) -> Vec<u8> {
    let mut end = prefix.as_bytes().to_vec();
    end.push(0xff);
    end
}

/// Wait for `done`, with a bounded backoff, and fail naming `what` after a
/// minute.
pub(in crate::engine::compaction) fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pause = Duration::from_micros(50);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen in a minute"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------
// The rule, level style.

#[test]
fn a_deleted_prefix_is_compacted_by_the_step_the_next_write_owes() {
    let f = open(no_worker());
    f.fill("p/", 200);
    f.fill("q/", 20);
    f.flush();
    f.delete("p/", 0..200);
    f.flush();

    // Two L0 tables, under the L0 trigger of four: nothing size-triggered.
    assert_eq!(f.db.engine().published_version().l0_count(), 2);
    assert_eq!(f.entries_under("p/"), 420);
    let (rows, blocks_before) = f.scan("p/");
    assert_eq!(rows, 0);
    assert!(
        blocks_before >= 20,
        "the scan walks the deletes: {blocks_before} blocks"
    );

    // The write after the flush owes one step, and that step is the pass.
    f.db.put(b"zz", b"1").unwrap();

    assert_eq!(f.deletion_passes(), 1);
    assert_eq!(f.entries_under("p/"), 0, "the prefix's tables are gone");
    assert_eq!(
        f.deletions_held(),
        0,
        "the deletions retired with what they deleted"
    );
    let (rows, blocks_after) = f.scan("p/");
    assert_eq!(rows, 0);
    assert!(
        blocks_after <= 2 && blocks_after < blocks_before,
        "the scan still loads {blocks_after} blocks, {blocks_before} before"
    );
    assert_eq!(f.scan("q/").0, 20, "what was not deleted is all there");
}

#[test]
fn a_deleted_prefix_is_compacted_by_the_worker_without_being_asked() {
    let f = open(Options::default());
    f.fill("p/", 200);
    f.fill("q/", 20);
    f.flush();
    f.delete("p/", 0..200);
    f.flush();

    wait_for("the pass for the deleted prefix", || {
        f.deletion_passes() >= 1
    });
    wait_for("the prefix's tables to go", || f.entries_under("p/") == 0);
    let (rows, blocks) = f.scan("p/");
    assert_eq!(rows, 0);
    assert!(blocks <= 2, "the scan still loads {blocks} blocks");
    assert_eq!(f.scan("q/").0, 20);
}

/// With no worker a write pays for one step and no more, whether or not
/// stopped writes relieve their own stalls.
#[test]
fn each_write_runs_at_most_one_pass() {
    for inline in [false, true] {
        // The L0 trigger is out of the way, so every pass is for deletions.
        let f = open(
            no_worker()
                .inline_compaction(inline)
                .l0_compaction_trigger(10),
        );
        f.fill("a/", 100);
        f.flush();
        f.fill("m/", 100);
        f.flush();
        // Held while the deletions are written, so both passes come due
        // together once it is released.
        let before = f.db.snapshot();
        f.delete("a/", 0..100);
        f.flush();
        f.delete("m/", 0..100);
        f.flush();
        assert_eq!(f.tables().len(), 4, "inline {inline}");
        assert_eq!(f.passes(), 0, "inline {inline}");
        drop(before);

        let ran: Vec<u64> = (0..6u32)
            .map(|i| {
                let before = f.passes();
                f.db.put(&key("w/", i), b"1").unwrap();
                f.passes() - before
            })
            .collect();
        // One range per write, then the step the second pass owed finds
        // nothing, and after that no step is owed.
        assert_eq!(ran, [1, 1, 0, 0, 0, 0], "inline {inline}");
        assert_eq!(f.deletion_passes(), 2, "inline {inline}");
        assert_eq!(
            f.entries_under("a/") + f.entries_under("m/"),
            0,
            "inline {inline}"
        );
    }
}

#[test]
fn a_range_with_few_deletions_is_left_alone() {
    let f = open(no_worker());
    f.fill("p/", 200);
    f.flush();
    f.delete("p/", 0..20);
    f.flush();
    let tables = f.file_ids();

    assert_eq!(f.settle(), 0);
    assert_eq!(f.deletion_passes(), 0);
    assert_eq!(f.file_ids(), tables, "no table was rewritten");
    assert_eq!(f.scan("p/").0, 180);
}

#[test]
fn only_the_tables_of_the_deleted_range_are_rewritten() {
    let f = open(no_worker());
    f.fill("a/", 200);
    f.fill("b/", 10);
    f.flush();
    f.fill("z/", 200);
    f.flush();
    let untouched = f.file_ids()[1];
    f.delete("a/", 0..200);
    f.flush();
    assert_eq!(f.file_ids().len(), 3);

    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);

    let version = f.db.engine().published_version();
    assert_eq!(
        version.levels[0]
            .iter()
            .map(|t| t.meta.file_id)
            .collect::<Vec<_>>(),
        vec![untouched],
        "the table of the other range stays where it was, as it was"
    );
    assert_eq!(version.levels[1].len(), 1, "one output, for what survived");
    assert_eq!(version.levels[1][0].meta.num_entries, 10);
    assert_eq!(
        (f.scan("a/").0, f.scan("b/").0, f.scan("z/").0),
        (0, 10, 200)
    );
}

// ---------------------------------------------------------------------
// Snapshots.

#[test]
fn a_snapshot_below_the_deletions_defers_the_pass_until_it_is_released() {
    let f = open(no_worker());
    f.fill("p/", 200);
    f.flush();
    let before = f.db.snapshot();
    f.delete("p/", 0..200);
    f.flush();

    // Pinned: however often a pass is asked for, none runs on the range.
    for _ in 0..8 {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!((f.passes(), f.deletion_passes()), (0, 0));
    assert_eq!(
        before.scan(None, None).unwrap().len(),
        200,
        "the snapshot keeps its view"
    );
    assert_eq!(f.scan("p/").0, 0);

    // Released: one pass, and then nothing more to do.
    drop(before);
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::DidWork);
    for _ in 0..8 {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!((f.passes(), f.deletion_passes()), (1, 1));
    assert_eq!(f.entries_under("p/"), 0);
}

#[test]
fn a_snapshot_above_the_deletions_keeps_its_view_through_the_pass() {
    let f = open(no_worker());
    f.fill("p/", 200);
    f.fill("q/", 20);
    f.flush();
    f.delete("p/", 0..200);
    let after = f.db.snapshot();
    f.db.put(b"q/new", b"1").unwrap();
    f.flush();

    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);
    assert_eq!(f.entries_under("p/"), 0);
    let seen = after.scan(None, None).unwrap();
    assert_eq!(
        seen.len(),
        20,
        "the snapshot reads what it read: {}",
        seen.len()
    );
    assert_eq!(f.db.scan(None, None).unwrap().len(), 21);
}

/// What E27 leaves behind: a pass at the bottom level keeps the deletions a
/// snapshot needs, and with them the versions they hide. Once the snapshot
/// is gone the table is mostly dead, and one pass in place clears it.
#[test]
fn deletions_a_snapshot_held_at_the_bottom_go_once_the_snapshot_does() {
    let f = open(no_worker());
    f.fill("p/", 100);
    f.fill("q/", 20);
    f.db.compact_range(None, None).wait().unwrap();
    let before = f.db.snapshot();
    f.delete("p/", 0..100);
    f.db.compact_range(None, None).wait().unwrap();

    let bottom = |f: &Fixture| f.db.engine().published_version().levels[MAX_LEVELS - 1].clone();
    assert_eq!(bottom(&f).len(), 1);
    assert_eq!(
        bottom(&f)[0].meta.num_entries,
        220,
        "100 deletions over 100 values, and 20"
    );
    assert_eq!(f.settle(), 0, "nothing runs while the snapshot pins them");
    assert_eq!(before.scan(None, None).unwrap().len(), 120);

    drop(before);
    let passes = f.passes();
    assert_eq!(f.settle(), 1, "one pass in place");
    assert_eq!(f.deletion_passes(), 1);
    assert_eq!(f.passes(), passes + 1);
    assert_eq!(bottom(&f).len(), 1);
    assert_eq!(bottom(&f)[0].meta.num_entries, 20);
    assert_eq!(f.deletions_held(), 0);
    assert_eq!((f.scan("p/").0, f.scan("q/").0), (0, 20));
}

/// Appends each operand to the value.
struct Append;

impl crate::MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands
            .iter()
            .for_each(|operand| out.extend_from_slice(operand));
        Some(out)
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

/// A deletion with a merge operand on it cannot retire, so a table of such
/// pairs comes out of a rewrite as it went in. It is exactly half
/// deletions, which the exact count a one-table pass is judged on does not
/// call mostly dead: the pass is never picked, where an estimate of what
/// the deletions hide would pick it for ever.
#[test]
fn deletions_under_merge_operands_do_not_get_a_table_rewritten() {
    let f = open(no_worker().merge_operator(Some(Arc::new(Append))));
    f.fill("k/", 50);
    f.db.compact_range(None, None).wait().unwrap();
    f.delete("k/", 0..50);
    // Between the deletions and the operands, so no pass folds the two
    // together; at or above every deletion, so it defers nothing.
    let between = f.db.snapshot();
    for i in 0..50 {
        f.db.merge(&key("k/", i), b"x").unwrap();
    }
    f.db.compact_range(None, None).wait().unwrap();

    let bottom = Arc::clone(&f.db.engine().published_version().levels[MAX_LEVELS - 1][0]);
    assert_eq!(
        bottom.meta.num_entries, 100,
        "an operand and a deletion for each key"
    );
    assert_eq!(
        bottom.meta.deletions.map(|d| (d.points, d.dead)),
        Some((50, 50))
    );
    let passes = f.passes();
    for _ in 0..4 {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!((f.passes(), f.deletion_passes()), (passes, 0));
    assert_eq!(between.scan(None, None).unwrap().len(), 0);
    assert_eq!(f.scan("k/").0, 50);
}

// ---------------------------------------------------------------------
// Range tombstones.

#[test]
fn a_range_delete_over_a_whole_table_is_compacted() {
    let f = open(no_worker());
    f.fill("r/", 200);
    f.flush();
    f.db.delete_range(b"r/", &end_of("r/")).unwrap();
    f.flush();
    assert_eq!(f.entries_under("r/"), 200);

    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);
    assert!(
        f.tables().is_empty(),
        "the table and the tombstone are both gone"
    );
}

#[test]
fn a_range_delete_over_most_of_a_table_is_compacted_and_over_little_of_it_is_not() {
    for (deleted, kept, compacted) in [(200, 40, true), (40, 200, false)] {
        let f = open(no_worker());
        f.fill("r/", deleted);
        f.fill("s/", kept);
        f.flush();
        f.db.delete_range(b"r/", &end_of("r/")).unwrap();
        f.flush();

        assert_eq!(
            f.settle(),
            u64::from(compacted),
            "{deleted} deleted of {}",
            deleted + kept
        );
        assert_eq!(f.deletion_passes(), u64::from(compacted));
        assert_eq!((f.scan("r/").0, f.scan("s/").0), (0, kept as usize));
        if compacted {
            assert_eq!(f.entries_under("r/"), 0);
            assert_eq!(f.deletions_held(), 0);
        } else {
            assert_eq!(f.entries_under("r/"), u64::from(deleted + kept));
        }
    }
}

#[test]
fn keys_written_and_range_deleted_in_one_memtable_are_compacted() {
    let f = open(no_worker());
    f.fill("r/", 200);
    f.fill("s/", 20);
    f.db.delete_range(b"r/", &end_of("r/")).unwrap();
    f.flush();
    // One table: the tombstone and the 200 entries it hides, counted as the
    // flush wrote them.
    let table = Arc::clone(&f.tables()[0]);
    assert_eq!(table.meta.num_entries, 220);
    assert_eq!(
        table.meta.deletions.map(|d| (d.points, d.dead)),
        Some((0, 200))
    );

    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);
    let tables = f.tables();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].meta.num_entries, 20);
    assert_eq!(f.deletions_held(), 0);
}

/// The stated limit: a tombstone that covers little of a large live table
/// is not worth a rewrite of the table, and stays.
#[test]
fn a_leftover_tombstone_over_little_of_a_live_table_is_left_alone() {
    let f = open(no_worker());
    f.fill("k/", 200);
    f.db.compact_range(None, None).wait().unwrap();
    let before = f.db.snapshot();
    f.db.delete_range(&key("k/", 2), &key("k/", 6)).unwrap();
    f.db.compact_range(None, None).wait().unwrap();
    drop(before);

    assert_eq!(f.settle(), 0);
    assert_eq!(f.deletions_held(), 1);
    assert_eq!(f.scan("k/").0, 196);
}

// ---------------------------------------------------------------------
// Turns.

/// Deletions written over data at the bottom level, with the L0 trigger at
/// one: every flush leaves size-triggered work due for the step the next
/// write owes, and the table of deletions, once that work has taken it from
/// L0 to L1, is a deletion pass due beside it.
fn deletions_over_the_bottom_under_steady_flushes(options: Options) -> Fixture {
    let f = open(options.l0_compaction_trigger(1));
    f.fill("d/", 200);
    f.db.compact_range(None, None).wait().unwrap();
    f.delete("d/", 0..200);
    f.flush();
    assert_eq!(f.deletion_passes(), 0);
    f
}

/// One write, which runs the one step owed, then a flush that leaves the
/// next step size-triggered work in L0.
fn write_then_flush(f: &Fixture, i: u32) {
    let before = f.passes();
    f.db.put(&key("w/", i), b"1").unwrap();
    assert_eq!(f.passes(), before + 1, "write {i} ran one pass");
    f.flush();
    assert_eq!(f.db.engine().published_version().l0_count(), 1);
}

#[test]
fn steady_size_work_passes_a_due_deletion_pass_over_a_bounded_number_of_times() {
    let f = deletions_over_the_bottom_under_steady_flushes(no_worker());
    let bound = u64::from(crate::sync::MAX_BYPASS);
    let at_start = f.passes();
    let mut writes = 0;
    while f.deletion_passes() == 0 {
        assert!(writes <= 2 * bound, "the deletion pass never got its turn");
        // Before the write runs its step, L0 holds the last flush: there is
        // size-triggered work for it every time.
        assert_eq!(f.db.engine().published_version().l0_count(), 1);
        let before = f.passes();
        f.db.put(&key("w/", writes as u32), b"1").unwrap();
        assert_eq!(f.passes(), before + 1);
        writes += 1;
        if f.deletion_passes() == 0 {
            f.flush();
        }
    }
    let size_passes_first = f.passes() - at_start - 1;
    assert_eq!(
        size_passes_first, bound,
        "size-triggered passes that went ahead of the deletion pass"
    );
    // The deletion pass went ahead of size-triggered work that was due,
    // which is still there for the next pass.
    assert_eq!(f.db.engine().published_version().l0_count(), 1);
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(f.db.engine().published_version().l0_count(), 0);
    assert_eq!((f.passes() - at_start, f.deletion_passes()), (bound + 2, 1));
}

#[test]
fn near_a_write_stall_size_work_goes_first_however_long_deletions_waited() {
    // One L0 table is one short of the slowdown: a stall is near whenever
    // size-triggered work is due here.
    let f = deletions_over_the_bottom_under_steady_flushes(
        no_worker().level0_slowdown_writes_trigger(2),
    );
    for i in 0..2 * crate::sync::MAX_BYPASS {
        write_then_flush(&f, i);
    }
    assert_eq!(f.deletion_passes(), 0);
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(
        f.deletion_passes(),
        0,
        "the last flush is size-triggered work"
    );
    // With L0 empty no stall is near, and nothing size-triggered is due.
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(f.deletion_passes(), 1);
}

fn outcomes() -> (
    impl Fn() -> std::io::Result<CompactionOutcome>,
    std::cell::Cell<u32>,
) {
    (|| Ok(CompactionOutcome::DidWork), std::cell::Cell::new(0))
}

#[test]
fn the_turn_goes_to_deletions_after_the_bound_and_starts_again() {
    let turn = Turn::default();
    let (work, deletion_passes) = outcomes();
    let by_deletions = || {
        deletion_passes.set(deletion_passes.get() + 1);
        Ok(CompactionOutcome::DidWork)
    };
    for round in 0..3 {
        for pass in 0..Turn::MAX {
            turn.pass(false, &work, by_deletions).unwrap();
            assert_eq!(deletion_passes.get(), round, "round {round}, pass {pass}");
        }
        turn.pass(false, &work, by_deletions).unwrap();
        assert_eq!(deletion_passes.get(), round + 1);
        assert_eq!(turn.passed_over(), 0);
    }
}

#[test]
fn a_look_that_finds_no_deletion_pass_starts_the_count_again() {
    let turn = Turn::default();
    let (work, looks) = outcomes();
    let none_due = || {
        looks.set(looks.get() + 1);
        Ok(CompactionOutcome::Idle)
    };
    for _ in 0..3 * (Turn::MAX + 1) {
        assert_eq!(
            turn.pass(false, &work, none_due).unwrap(),
            CompactionOutcome::DidWork
        );
    }
    // One look per bound of size passes, not one per pass.
    assert_eq!(looks.get(), 3);
}

#[test]
fn deletions_run_at_once_when_nothing_size_triggered_does() {
    let turn = Turn::default();
    let idle = || Ok(CompactionOutcome::Idle);
    let contended = || Ok(CompactionOutcome::Contended);
    let work = || Ok(CompactionOutcome::DidWork);
    assert_eq!(
        turn.pass(false, idle, work).unwrap(),
        CompactionOutcome::DidWork
    );
    assert_eq!(
        turn.pass(true, idle, work).unwrap(),
        CompactionOutcome::DidWork
    );
    assert_eq!(
        turn.pass(false, contended, work).unwrap(),
        CompactionOutcome::DidWork
    );
    // Nothing ran, and one of the two was in another pass's way.
    assert_eq!(
        turn.pass(false, contended, idle).unwrap(),
        CompactionOutcome::Contended
    );
    assert_eq!(
        turn.pass(false, idle, contended).unwrap(),
        CompactionOutcome::Contended
    );
    assert_eq!(
        turn.pass(false, idle, idle).unwrap(),
        CompactionOutcome::Idle
    );
}

#[test]
fn near_a_stall_the_turn_stays_with_size_work() {
    let turn = Turn::default();
    let (work, deletion_passes) = outcomes();
    let by_deletions = || {
        deletion_passes.set(deletion_passes.get() + 1);
        Ok(CompactionOutcome::DidWork)
    };
    for _ in 0..4 * Turn::MAX {
        turn.pass(true, &work, by_deletions).unwrap();
    }
    assert_eq!(deletion_passes.get(), 0);
    assert_eq!(
        turn.passed_over(),
        Turn::MAX,
        "the wait is remembered, up to the bound"
    );
    turn.pass(false, &work, by_deletions).unwrap();
    assert_eq!(deletion_passes.get(), 1);
}

// ---------------------------------------------------------------------
// Universal style.

fn universal() -> Options {
    // Uncompressed, so a run of large values stays much larger than a run
    // of deletions and the size rules leave the two alone.
    no_worker()
        .compaction_style(CompactionStyle::Universal)
        .compression(CompressionType::None)
}

#[test]
fn universal_merges_the_runs_a_deleted_prefix_dominates() {
    let f = open(universal());
    for i in 0..100 {
        f.db.put(&key("p/", i), &[b'v'; 1024]).unwrap();
    }
    f.flush();
    f.delete("p/", 0..100);
    f.flush();
    assert_eq!(f.tables().len(), 2);

    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);
    assert!(
        f.tables().is_empty(),
        "both runs merged away: {:?}",
        f.file_ids()
    );
    assert_eq!(f.scan("p/").0, 0);
}

/// Under universal compaction every merge writes L0. A deletion retires
/// there by the same rule as anywhere: when no older run outside the merge
/// holds what it deletes.
#[test]
fn a_universal_merge_keeps_the_deletion_an_older_run_outside_it_needs() {
    let f = open(universal());
    for i in 0..300 {
        f.db.put(&key("a/", i), &[b'v'; 1024]).unwrap();
    }
    f.flush();
    f.fill("p/", 100);
    f.flush();
    f.delete("p/", 0..100);
    f.db.delete(&key("a/", 0)).unwrap();
    f.flush();
    assert_eq!(f.tables().len(), 3);

    // The two newest runs are mostly dead; the oldest is left out.
    assert_eq!(f.settle(), 1);
    assert_eq!(f.deletion_passes(), 1);
    let runs = f.tables();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[1].meta.num_entries, 1,
        "the deletion of a key the oldest run holds"
    );
    assert_eq!(f.deletions_held(), 1);
    assert_eq!(f.db.get(&key("a/", 0)).unwrap(), None);
    assert!(f.db.get(&key("a/", 1)).unwrap().is_some());
    assert_eq!((f.scan("a/").0, f.scan("p/").0), (299, 0));
}

#[test]
fn a_universal_merge_of_every_run_retires_its_deletions() {
    let f = open(universal());
    f.fill("a/", 50);
    f.flush();
    f.delete("a/", 0..20);
    f.db.delete_range(&key("a/", 30), &key("a/", 40)).unwrap();
    f.flush();
    f.db.compact_range(None, None).wait().unwrap();

    let runs = f.tables();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].meta.num_entries, 20, "only the live keys are left");
    assert_eq!(f.deletions_held(), 0);
    assert_eq!(f.scan("a/").0, 20);
}

/// A pool's only run is rewritten for its deletions only when the rewrite
/// can retire them. Over a deeper level that holds their keys it could not,
/// and would be picked again for ever.
#[test]
fn a_lone_universal_run_is_not_rewritten_over_a_level_that_holds_its_keys() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), no_worker()).unwrap();
        for i in 0..100 {
            db.put(&key("p/", i), &[b'v'; 64]).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), universal().statistics(Some(Arc::clone(&stats)))).unwrap();
    let f = Fixture {
        db,
        stats,
        _dir: dir,
    };
    f.delete("p/", 0..100);
    f.flush();
    let version = f.db.engine().published_version();
    assert_eq!(
        (version.l0_count(), version.levels[MAX_LEVELS - 1].len()),
        (1, 1)
    );
    for _ in 0..4 {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!(f.deletion_passes(), 0);
    assert_eq!(f.scan("p/").0, 0);
}

#[test]
fn fifo_never_compacts_for_deletions() {
    let f = open(no_worker().compaction_style(CompactionStyle::Fifo));
    f.fill("p/", 100);
    f.flush();
    f.delete("p/", 0..100);
    f.flush();
    assert_eq!(f.settle(), 0);
    assert_eq!(f.deletion_passes(), 0);
    assert_eq!(f.tables().len(), 2);
}
