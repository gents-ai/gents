//! Reopening a populated database under different [`Options`] than it
//! was created with: the upgrade path.
//!
//! Every test here follows the same shape, driven by
//! [`assert_option_change_preserves_data`]: seed a directory under one
//! option set, reopen it under another and read every old key, write
//! more so the directory holds files built both ways, read the mix,
//! compact it, then reopen under the original options and read
//! everything again. An upgrade that is readable in only one direction
//! is still a trap, so the round trip is part of every case.

use regolith::{
    CompactionStyle, CompressionType, Db, DurabilityMode, FifoCompactionOptions, Options,
};
use tempfile::TempDir;

use super::{assert_range_present, opts, sst_format_versions, write_range};

/// [`opts`] with one or more knobs overridden, so an option-change test
/// reads as the single knob it varies.
fn tuned(set: impl FnOnce(Options) -> Options) -> Options {
    set(opts())
}

/// A closed database directory holding keys `0..n`, written under
/// `base` and compacted so the data really lives in SSTables.
fn seeded_dir(n: usize, base: &Options) -> TempDir {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), base.clone()).unwrap();
    write_range(&db, 0, n);
    db.compact_range(None, None).wait().unwrap();
    db.close().unwrap();
    drop(db);
    dir
}

/// Drive one option change end to end: seed under `from`, reopen under
/// `to`, read every old key, write more (leaving the directory holding
/// files from both option sets), read both halves out of that mix,
/// compact, then reopen under `from` and read everything. The round
/// trip is the point: an upgrade readable in only one direction is
/// still a trap.
fn assert_option_change_preserves_data(label: &str, from: Options, to: Options) {
    const SEEDED: usize = 400;
    const ALL: usize = 800;
    let stage = |s: &str| format!("{label}: {s}");

    let dir = seeded_dir(SEEDED, &from);
    let db = Db::open(dir.path(), to)
        .unwrap_or_else(|e| panic!("{label}: reopening with the new options failed: {e}"));
    assert_range_present(&db, 0, SEEDED, &stage("after the change"));
    write_range(&db, SEEDED, ALL);
    assert_range_present(&db, 0, ALL, &stage("files from both option sets"));
    db.compact_range(None, None).wait().unwrap();
    assert_range_present(&db, 0, ALL, &stage("after compacting the mix"));
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), from)
        .unwrap_or_else(|e| panic!("{label}: reopening with the original options failed: {e}"));
    assert_range_present(&db, 0, ALL, &stage("after changing back"));
    db.close().unwrap();
}

/// Property: growing or shrinking the memtable budget between runs is a
/// tuning change, not a migration. Catches a recovery path that sizes
/// its replay buffer from the *current* option and drops WAL records
/// that no longer fit.
#[test]
fn reopening_with_a_resized_write_buffer_keeps_every_key() {
    let small = tuned(|o| o.write_buffer_size(4 * 1024));
    let large = tuned(|o| o.write_buffer_size(8 * 1024 * 1024));
    assert_option_change_preserves_data("4 KiB -> 8 MiB", small.clone(), large.clone());
    assert_option_change_preserves_data("8 MiB -> 4 KiB", large, small);
}

/// Property: `block_size` and `bloom_bits_per_key` describe how new
/// SSTables are built, never how existing ones are read - every block
/// and filter carries its own geometry on disk. Catches a reader that
/// takes the block length or filter width from the live options instead
/// of from the file it is reading.
#[test]
fn reopening_with_a_different_block_or_bloom_geometry_keeps_every_key() {
    let tight = tuned(|o| o.block_size(1024).bloom_bits_per_key(4));
    let loose = tuned(|o| o.block_size(64 * 1024).bloom_bits_per_key(20));
    assert_option_change_preserves_data(
        "1 KiB/4 bits -> 64 KiB/20 bits",
        tight.clone(),
        loose.clone(),
    );
    assert_option_change_preserves_data("64 KiB/20 bits -> 1 KiB/4 bits", loose, tight);
}

/// Property: the codec is recorded per block, so a database written
/// with one codec stays readable under any other and may hold blocks in
/// all three at once. Catches a reader that decompresses with the
/// configured codec rather than the one named in the block frame.
#[test]
fn reopening_with_a_different_compression_codec_keeps_every_key() {
    let codecs = [
        CompressionType::None,
        CompressionType::Snappy,
        CompressionType::Lz4,
    ];
    for from in codecs {
        for to in codecs {
            if from == to {
                continue;
            }
            let mk = |c| tuned(|o| o.compression(c));
            assert_option_change_preserves_data(&format!("{from:?} -> {to:?}"), mk(from), mk(to));
        }
    }
}

/// Property: the compaction style governs which files get merged next,
/// not which can be read. Switching a populated database between
/// leveled, universal, and FIFO (capped high enough that nothing is due
/// for eviction) must not hide a key. Catches a read path that consults
/// only the levels the current style would have produced: universal and
/// FIFO keep everything at L0, so a leveled database reopened under
/// either has live L1+ files their pickers never touch and the reader
/// must still see.
#[test]
fn reopening_under_a_different_compaction_style_keeps_every_key() {
    let leveled = tuned(|o| o.compaction_style(CompactionStyle::Level));
    let universal = tuned(|o| o.compaction_style(CompactionStyle::Universal));
    let fifo = tuned(|o| {
        o.compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 1 << 30,
            })
    });

    // Precondition, checked rather than assumed: the leveled seed really
    // does push its files below L0. Without that, "universal and FIFO
    // never look below L0" would be a vacuous claim and this test would
    // pass for the wrong reason.
    let probe = seeded_dir(400, &leveled);
    let db = Db::open(probe.path(), leveled.clone()).unwrap();
    let below_l0: u64 = (1..=6)
        .map(|level| {
            db.get_int_property(&format!("regolith.num-files-at-level{level}"))
                .unwrap()
        })
        .sum();
    assert!(
        below_l0 > 0,
        "the leveled seed left every file at L0, so switching style proves nothing"
    );
    db.close().unwrap();
    drop(db);
    drop(probe);

    assert_option_change_preserves_data("level -> universal", leveled.clone(), universal.clone());
    assert_option_change_preserves_data("level -> fifo", leveled.clone(), fifo.clone());
    assert_option_change_preserves_data("universal -> level", universal, leveled.clone());
    assert_option_change_preserves_data("fifo -> level", fifo, leveled);
}

/// Property: the footer magic distinguishes the flat index (version 3)
/// from the partitioned one (version 4), so toggling `partitioned_index`
/// leaves a directory holding both layouts and every file is read with
/// the layout it was written in. The mix is asserted from the bytes on
/// disk, and `l0_compaction_trigger` is raised out of reach so no
/// background pass can merge the layouts before the check. Catches a
/// reader that picks the index layout from the live option instead of
/// the footer.
#[test]
fn reopening_that_toggles_the_partitioned_index_reads_both_layouts() {
    let flat = tuned(|o| o.l0_compaction_trigger(1000));
    let partitioned = tuned(|o| {
        o.l0_compaction_trigger(1000)
            .partitioned_index(true)
            .metadata_block_size(512)
    });

    // 5 is the flat footer and 6 the partitioned one, both under the
    // REGOSST magic.
    for (label, from, to, new_version) in [
        ("flat -> partitioned", &flat, &partitioned, 6u8),
        ("partitioned -> flat", &partitioned, &flat, 5u8),
    ] {
        let stage = |s: &str| format!("{label}: {s}");
        let dir = seeded_dir(400, from);
        let db = Db::open(dir.path(), to.clone()).unwrap();
        assert_range_present(&db, 0, 400, label);

        write_range(&db, 400, 1200);
        // The worker writes sealed memtables out off the commit path, so a
        // table may still be under way here: a file with no footer yet. An
        // explicit flush returns once every table is whole and installed.
        db.flush().unwrap();
        let versions = sst_format_versions(dir.path());
        assert!(
            versions.len() > 1 && versions.contains(&new_version),
            "{label}: expected both footer versions on disk after the toggle, found {versions:?}"
        );
        assert_range_present(&db, 0, 1200, &stage("mixed index layouts"));
        db.compact_range(None, None).wait().unwrap();
        assert_range_present(&db, 0, 1200, &stage("after compacting the mix"));
        db.close().unwrap();
        drop(db);

        let db = Db::open(dir.path(), from.clone()).unwrap();
        assert_range_present(&db, 0, 1200, &stage("after changing back"));
        db.close().unwrap();
    }
}

/// Property: the durability mode is a per-run choice, not a property of
/// the files. A database written with `Eventual` reopens under
/// `Immediate` and vice versa with every key intact. Catches a WAL
/// format that differs between the two modes.
#[test]
fn reopening_with_a_different_durability_mode_keeps_every_key() {
    let eventual = tuned(|o| o.durability(DurabilityMode::Eventual));
    let immediate = tuned(|o| o.durability(DurabilityMode::Immediate));
    assert_option_change_preserves_data(
        "eventual -> immediate",
        eventual.clone(),
        immediate.clone(),
    );
    assert_option_change_preserves_data("immediate -> eventual", immediate, eventual);
}
