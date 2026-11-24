//! The census counts the deletions of tables recorded without counts, a
//! bounded step at a time, and the deletion rule then applies to them.

use std::cell::Cell;
use std::sync::Arc;

use tempfile::TempDir;

use super::super::deletion::tests::wait_for;
use super::*;
use crate::engine::manifest::forget_counts;
use crate::{CompactionOutcome, CompactionStyle, CompressionType, Db, Options, Statistics, Ticker};

const UNCOUNTED: &str = "regolith.num-uncounted-tables";

struct Fixture {
    db: Db,
    stats: Arc<Statistics>,
    dir: TempDir,
}

fn key(prefix: &str, i: u32) -> Vec<u8> {
    format!("{prefix}{i:04}").into_bytes()
}

fn fill(db: &Db, prefix: &str, count: u32) {
    for i in 0..count {
        db.put(&key(prefix, i), &[b'v'; 64]).unwrap();
    }
}

/// A database as an earlier build left it: `build` writes it with no
/// worker, it is closed, and its manifest is rewritten without any table's
/// counts. Reopened with `options`. Small blocks and no compression, so a
/// table's size in blocks follows from what a test puts in it.
fn upgraded(options: Options, build: impl FnOnce(&Db)) -> Fixture {
    let options = options.compression(CompressionType::None);
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(
            dir.path(),
            options
                .clone()
                .max_background_compactions(0)
                .block_size(512),
        )
        .unwrap();
        build(&db);
        db.close().unwrap();
    }
    forget_counts(dir.path());
    let stats = Arc::new(Statistics::new());
    let db = Db::open(
        dir.path(),
        options.block_size(512).statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    Fixture { db, stats, dir }
}

/// A bottom-level table that is mostly dead (100 deletions over the 100
/// values a snapshot held under them, and 20 live keys), and two L0 tables
/// of other ranges.
fn deleted_prefix_at_the_bottom(db: &Db) {
    fill(db, "p/", 100);
    fill(db, "q/", 20);
    db.compact_range(None, None).wait().unwrap();
    let held = db.snapshot();
    for i in 0..100 {
        db.delete(&key("p/", i)).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    fill(db, "x/", 50);
    db.flush().unwrap();
    fill(db, "y/", 50);
    db.flush().unwrap();
    // Released only now, with no write after it: while it was held nothing
    // compacted the deletions it pinned, and nothing does before the close.
    drop(held);
}

impl Fixture {
    fn uncounted(&self) -> u64 {
        self.db.get_int_property(UNCOUNTED).unwrap()
    }

    fn deletion_passes(&self) -> u64 {
        self.stats.get_ticker(Ticker::CompactionDeletionTriggered)
    }

    fn bottom_entries(&self) -> Vec<u64> {
        self.db.engine().published_version().levels[MAX_LEVELS - 1]
            .iter()
            .map(|table| table.meta.num_entries)
            .collect()
    }

    fn blocks_missed(&self) -> u64 {
        self.stats.get_ticker(Ticker::BlockCacheMiss)
    }

    /// One step of the census as the engine's own pass runs it, stopped
    /// when `interrupted` says so.
    fn census_step(&self, interrupted: &dyn Fn() -> bool) {
        let engine = self.db.engine();
        step(
            &engine.versions,
            &engine.cache,
            &engine.options.to_compaction_options(),
            &engine.compaction_claims,
            false,
            interrupted,
        );
    }
}

#[test]
fn tables_without_counts_are_counted_one_idle_pass_each_and_then_compacted() {
    let f = upgraded(
        Options::default().max_background_compactions(0),
        deleted_prefix_at_the_bottom,
    );
    assert_eq!(f.uncounted(), 3);
    assert_eq!(f.bottom_entries(), [220]);

    // Each idle pass counts one table, the deepest first. Nothing is
    // compacted for its deletions before they are known.
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    assert_eq!((f.uncounted(), f.deletion_passes()), (2, 0));
    let bottom = Arc::clone(&f.db.engine().published_version().levels[MAX_LEVELS - 1][0]);
    assert_eq!(
        bottom.meta.deletions.map(|d| (d.points, d.dead)),
        Some((100, 200)),
        "the deletions and the values under them"
    );

    // Known now, and mostly dead: the next pass rewrites it, without a
    // call that names it.
    assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!((f.uncounted(), f.deletion_passes()), (2, 1));
    assert_eq!(f.bottom_entries(), [20]);

    // The other two tables, one idle pass each, and then nothing.
    for left in [1, 0, 0, 0] {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
        assert_eq!(f.uncounted(), left);
    }
    assert_eq!(f.deletion_passes(), 1);
    assert_eq!(f.db.scan(None, None).unwrap().len(), 120);
}

#[test]
fn a_worker_counts_and_compacts_an_upgraded_database_without_being_asked() {
    let f = upgraded(Options::default(), deleted_prefix_at_the_bottom);
    wait_for("every table to be counted", || f.uncounted() == 0);
    wait_for("the pass for the deleted prefix", || {
        f.deletion_passes() == 1
    });
    wait_for("the bottom table to be rewritten", || {
        f.bottom_entries() == [20]
    });
    assert_eq!(f.db.scan(None, None).unwrap().len(), 120);
}

#[test]
fn counts_the_census_recorded_are_there_after_a_reopen() {
    let f = upgraded(Options::default().max_background_compactions(0), |db| {
        fill(db, "a/", 50);
        db.flush().unwrap();
        fill(db, "b/", 50);
        db.flush().unwrap();
    });
    assert_eq!(f.uncounted(), 2);
    for left in [1, 0] {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
        assert_eq!(f.uncounted(), left);
    }
    let Fixture { db, dir, .. } = f;
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), Options::default().max_background_compactions(0)).unwrap();
    assert_eq!(db.get_int_property(UNCOUNTED), Some(0));
    let version = db.engine().published_version();
    assert_eq!(version.levels[0].len(), 2);
    assert!(
        version.levels[0]
            .iter()
            .all(|table| { table.meta.deletions.map(|d| (d.points, d.dead)) == Some((0, 0)) })
    );
}

/// A step reads no more than one `target_file_size` of a table, so a table
/// larger than that takes several steps, and loses nothing between them.
#[test]
fn a_table_larger_than_one_step_is_counted_over_several() {
    let f = upgraded(
        Options::default()
            .max_background_compactions(0)
            .target_file_size(2048),
        |db| {
            fill(db, "a/", 400);
            for i in 0..50 {
                db.delete(&key("a/", i)).unwrap();
            }
            db.flush().unwrap();
        },
    );
    let table = Arc::clone(&f.db.engine().published_version().levels[0][0]);
    assert!(
        table.meta.file_size > 8 * 2048,
        "the table is many steps long"
    );

    let mut steps = 0;
    let before = f.blocks_missed();
    while f.uncounted() > 0 {
        assert!(steps < 64, "the count never finishes");
        f.census_step(&|| false);
        steps += 1;
    }
    assert!(
        steps >= 8,
        "one step read more than its bound: {steps} steps"
    );
    // Each block was read once: the place was kept from step to step.
    let blocks = f.blocks_missed() - before;
    let version = f.db.engine().published_version();
    assert_eq!(
        version.levels[0][0]
            .meta
            .deletions
            .map(|d| (d.points, d.dead)),
        Some((50, 100))
    );
    for _ in 0..4 {
        f.census_step(&|| false);
    }
    assert_eq!(
        f.blocks_missed() - before,
        blocks,
        "a counted table is not read again"
    );
}

#[test]
fn an_interrupted_step_stops_at_a_block_and_the_next_goes_on_from_there() {
    let f = upgraded(Options::default().max_background_compactions(0), |db| {
        fill(db, "a/", 200);
        db.flush().unwrap();
    });
    // Told to stop from the start: nothing is read.
    let before = f.blocks_missed();
    f.census_step(&|| true);
    assert_eq!(f.blocks_missed(), before);

    // Told to stop after three looks: a few blocks, then the place is kept.
    let mut steps = 0;
    while f.uncounted() > 0 {
        assert!(steps < 200, "the count never finishes");
        let looks = Cell::new(0);
        let step_before = f.blocks_missed();
        f.census_step(&|| {
            looks.set(looks.get() + 1);
            looks.get() > 3
        });
        assert!(
            f.blocks_missed() - step_before <= 3,
            "a step went on reading after it was told to stop"
        );
        steps += 1;
    }
    assert!(steps > 4, "the table took {steps} steps");
    assert_eq!(
        f.db.engine().published_version().levels[0][0]
            .meta
            .deletions,
        Some(crate::engine::sstable::Deletions::default())
    );
}

#[test]
fn a_table_compacted_away_while_it_is_counted_is_dropped_from_the_count() {
    let f = upgraded(
        Options::default()
            .max_background_compactions(0)
            .target_file_size(2048),
        |db| {
            fill(db, "a/", 200);
            db.flush().unwrap();
        },
    );
    f.census_step(&|| false);
    assert_eq!(f.uncounted(), 1, "one step is not the whole table");
    // Rewritten by a compaction: the new table has its counts already.
    f.db.compact_range(None, None).wait().unwrap();
    assert_eq!(f.uncounted(), 0);
    f.census_step(&|| false);
    f.census_step(&|| false);
    assert_eq!(f.uncounted(), 0);
    assert_eq!(f.db.scan(None, None).unwrap().len(), 200);
}

#[test]
fn a_table_that_does_not_read_back_is_set_aside_and_stays_uncounted() {
    let dir = TempDir::new().unwrap();
    let options = || {
        Options::default()
            .max_background_compactions(0)
            .block_size(512)
    };
    {
        let db = Db::open(dir.path(), options()).unwrap();
        fill(&db, "a/", 200);
        db.flush().unwrap();
        db.close().unwrap();
    }
    forget_counts(dir.path());
    // Damage a data block near the start of the table: it opens (its footer
    // and index are whole), and the block's checksum fails when read.
    let sst_dir = dir.path().join("sst");
    let table = std::fs::read_dir(&sst_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&table).unwrap();
    bytes[100] ^= 0xFF;
    std::fs::write(&table, bytes).unwrap();

    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), options().statistics(Some(Arc::clone(&stats)))).unwrap();
    let f = Fixture { db, stats, dir };
    assert_eq!(f.uncounted(), 1);
    f.census_step(&|| false);
    assert_eq!(f.uncounted(), 1, "not counted, and still said so");
    let after_first = f.blocks_missed();
    for _ in 0..8 {
        f.census_step(&|| false);
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!(f.blocks_missed(), after_first, "it is not read again");
    assert_eq!(f.uncounted(), 1);
}

#[test]
fn universal_runs_without_counts_are_counted_too() {
    let f = upgraded(
        Options::default()
            .max_background_compactions(0)
            .compaction_style(CompactionStyle::Universal),
        |db| {
            for i in 0..100 {
                db.put(&key("p/", i), &[b'v'; 1024]).unwrap();
            }
            db.flush().unwrap();
            let held = db.snapshot();
            for i in 0..100 {
                db.delete(&key("p/", i)).unwrap();
            }
            db.flush().unwrap();
            drop(held);
        },
    );
    assert_eq!(f.uncounted(), 2);
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        outcomes.push(f.db.compact_step().unwrap());
    }
    // Two idle passes count the two runs; the third merges them for their
    // deletions; the fourth finds an empty pool.
    assert_eq!(
        outcomes,
        [
            CompactionOutcome::Idle,
            CompactionOutcome::Idle,
            CompactionOutcome::DidWork,
            CompactionOutcome::Idle
        ]
    );
    assert_eq!((f.uncounted(), f.deletion_passes()), (0, 1));
    assert!(f.db.engine().published_version().levels[0].is_empty());
}

#[test]
fn fifo_tables_are_not_counted() {
    let f = upgraded(
        Options::default()
            .max_background_compactions(0)
            .compaction_style(CompactionStyle::Fifo),
        |db| {
            fill(db, "a/", 50);
            db.flush().unwrap();
        },
    );
    let before = f.blocks_missed();
    for _ in 0..4 {
        assert_eq!(f.db.compact_step().unwrap(), CompactionOutcome::Idle);
    }
    assert_eq!(f.uncounted(), 1);
    assert_eq!(f.blocks_missed(), before);
}
