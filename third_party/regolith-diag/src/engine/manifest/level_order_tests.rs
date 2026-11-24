//! The order of a level's tables: L0 in arrival order, every deeper level one
//! sorted run, and the binary search over such a run that reads rely on.

use std::collections::BTreeMap;
use std::path::Path;

use proptest::prelude::*;
use tempfile::TempDir;

use super::tests::make_live_sst;
use super::*;
use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::sstable::SsTableWriter;
use crate::options::CompressionType;
use crate::{Db, Options};

/// One real table file, opened once, that stands behind every table a search
/// test builds: the search reads only each table's recorded range.
fn shared_reader(dir: &Path) -> Arc<SsTableReader> {
    let path = dir.join("shared.sst");
    let mut writer =
        SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
    writer
        .add(&encode_internal_key(b"k", 1, VALUE_TYPE_VALUE), b"v")
        .unwrap();
    writer.finish().unwrap().unwrap();
    Arc::new(SsTableReader::open(&path, 1).unwrap())
}

/// A table recorded as covering `[smallest, largest]` with `num_entries`
/// point entries.
fn table(
    reader: &Arc<SsTableReader>,
    file_id: u64,
    smallest: &[u8],
    largest: &[u8],
    num_entries: u64,
) -> Arc<LiveSst> {
    LiveSst::new(
        SsTableMeta {
            file_id,
            smallest_key: smallest.to_vec(),
            largest_key: largest.to_vec(),
            file_size: 1,
            num_entries,
            global_seq: None,
            deletions: None,
        },
        Arc::clone(reader),
        1,
    )
}

fn ids(files: &[Arc<LiveSst>]) -> Vec<u64> {
    files.iter().map(|f| f.meta.file_id).collect()
}

fn version_with(level: usize, files: Vec<Arc<LiveSst>>) -> Version {
    let mut version = Version::new();
    version.levels[level] = files;
    version
}

#[test]
fn a_run_covering_a_key_holds_every_table_that_shares_a_boundary_at_it() {
    let dir = TempDir::new().unwrap();
    let reader = shared_reader(dir.path());
    // a..c, then a single-key table at c, then a table-without-points from c
    // to f, then a gap, then h..k.
    let level = [
        table(&reader, 1, b"a", b"c", 5),
        table(&reader, 2, b"c", b"c", 1),
        table(&reader, 3, b"c", b"f", 0),
        table(&reader, 4, b"h", b"k", 5),
    ];
    assert!(version_with(1, level.to_vec()).levels_are_sorted_runs());

    assert_eq!(ids(covering(&level, b"c")), [1, 2, 3]);
    assert_eq!(ids(covering(&level, b"a")), [1]);
    assert_eq!(ids(covering(&level, b"b")), [1]);
    assert_eq!(ids(covering(&level, b"d")), [3]);
    assert_eq!(ids(covering(&level, b"f")), [3]);
    assert_eq!(ids(covering(&level, b"h")), [4]);
    assert_eq!(ids(covering(&level, b"k")), [4]);
    assert!(covering(&level, b"g").is_empty(), "the gap between f and h");
}

#[test]
fn a_table_without_points_sharing_a_boundary_is_found_beside_the_one_holding_the_key() {
    let dir = TempDir::new().unwrap();
    let reader = shared_reader(dir.path());
    let level = [
        table(&reader, 1, b"a", b"m", 3),
        table(&reader, 2, b"m", b"p", 0),
    ];
    assert!(version_with(1, level.to_vec()).levels_are_sorted_runs());

    assert_eq!(ids(covering(&level, b"l")), [1]);
    assert_eq!(ids(covering(&level, b"m")), [1, 2]);
    assert_eq!(ids(covering(&level, b"n")), [2]);
}

#[test]
fn a_level_with_nothing_in_it_covers_nothing() {
    let level: [Arc<LiveSst>; 0] = [];
    assert!(covering(&level, b"k").is_empty());
    assert!(overlapping(&level, b"a", b"z").is_empty());
}

#[test]
fn a_key_before_or_after_every_table_is_covered_by_none() {
    let dir = TempDir::new().unwrap();
    let reader = shared_reader(dir.path());
    let level = [
        table(&reader, 1, b"d", b"f", 2),
        table(&reader, 2, b"h", b"k", 2),
    ];
    assert!(covering(&level, b"").is_empty());
    assert!(covering(&level, b"a").is_empty());
    assert!(covering(&level, b"c\xff").is_empty());
    assert!(covering(&level, b"k\0").is_empty());
    assert!(covering(&level, b"z").is_empty());
}

#[test]
fn a_range_picks_the_contiguous_run_it_touches() {
    let dir = TempDir::new().unwrap();
    let reader = shared_reader(dir.path());
    let level = [
        table(&reader, 1, b"a", b"b", 2),
        table(&reader, 2, b"c", b"d", 2),
        table(&reader, 3, b"e", b"f", 2),
        table(&reader, 4, b"g", b"h", 2),
    ];
    // Touching a table's end or start counts as overlapping it.
    assert_eq!(ids(overlapping(&level, b"b", b"e")), [1, 2, 3]);
    assert_eq!(ids(overlapping(&level, b"c", b"d")), [2]);
    assert_eq!(ids(overlapping(&level, b"a", b"h")), [1, 2, 3, 4]);
    assert_eq!(ids(overlapping(&level, b"d", b"d")), [2]);
    // Inside a gap, and outside every table.
    assert!(overlapping(&level, b"bb", b"bz").is_empty());
    assert!(overlapping(&level, b"i", b"z").is_empty());
    assert!(overlapping(&level, b"", b"\x01").is_empty());
}

#[test]
fn a_table_added_below_l0_takes_its_sorted_place_and_l0_keeps_arrival_order() {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();

    // Ids and key ranges both out of order on purpose.
    let arrivals: [(usize, u64, &[u8], &[u8]); 8] = [
        (1, 11, b"m", b"n"),
        (1, 12, b"a", b"b"),
        (0, 13, b"x", b"y"),
        (1, 14, b"t", b"u"),
        (0, 15, b"a", b"z"),
        (1, 16, b"c", b"d"),
        (0, 17, b"m", b"n"),
        (2, 18, b"q", b"r"),
    ];
    for (level, id, smallest, largest) in arrivals {
        let file = make_live_sst(&sst_dir, id, smallest, largest);
        vs.apply(&[VersionEdit::AddFile { level, file }]).unwrap();
    }

    let version = vs.current();
    assert_eq!(ids(&version.levels[0]), [13, 15, 17], "L0 is arrival order");
    assert_eq!(ids(&version.levels[1]), [12, 16, 11, 14], "L1 is key order");
    assert_eq!(ids(&version.levels[2]), [18]);
    assert!(version.levels_are_sorted_runs());
}

#[test]
fn tables_with_the_same_smallest_key_stay_in_the_order_they_arrived() {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();

    for id in [7, 3, 9] {
        let file = make_live_sst(&sst_dir, id, b"k", b"k");
        vs.apply(&[VersionEdit::AddFile { level: 1, file }])
            .unwrap();
    }
    assert_eq!(ids(&vs.current().levels[1]), [7, 3, 9]);
}

#[test]
fn recovery_lists_each_level_in_the_order_the_runtime_placed_it() {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();

    let before = {
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let arrivals: [(usize, u64, &[u8], &[u8]); 7] = [
            (1, 21, b"m", b"n"),
            (1, 22, b"a", b"b"),
            (0, 23, b"x", b"y"),
            (1, 24, b"t", b"u"),
            (0, 25, b"a", b"z"),
            (1, 26, b"c", b"d"),
            (2, 27, b"q", b"r"),
        ];
        for (level, id, smallest, largest) in arrivals {
            let file = make_live_sst(&sst_dir, id, smallest, largest);
            vs.apply(&[VersionEdit::AddFile { level, file }]).unwrap();
        }
        // A removal in the middle of a sorted level leaves the rest in place.
        vs.apply(&[VersionEdit::RemoveFile {
            level: 1,
            file_id: 26,
        }])
        .unwrap();
        let version = vs.current();
        version
            .levels
            .iter()
            .map(|files| ids(files))
            .collect::<Vec<_>>()
    };
    assert_eq!(before[1], [22, 21, 24]);

    let reopened = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let after: Vec<Vec<u64>> = reopened.current().levels.iter().map(|f| ids(f)).collect();
    assert_eq!(after, before, "replay of the edit log");
    assert!(reopened.current().levels_are_sorted_runs());

    // The rewritten manifest lists the tables as the sorted level holds them.
    let mut reopened = reopened;
    reopened.compact_manifest().unwrap();
    drop(reopened);
    let rewritten = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let after: Vec<Vec<u64>> = rewritten.current().levels.iter().map(|f| ids(f)).collect();
    assert_eq!(after, before, "replay of the rewritten manifest");
}

#[test]
fn only_a_boundary_may_be_shared_for_the_levels_below_l0_to_be_sorted_runs() {
    let dir = TempDir::new().unwrap();
    let reader = shared_reader(dir.path());
    let sound = |files: Vec<Arc<LiveSst>>| version_with(1, files).levels_are_sorted_runs();

    assert!(sound(vec![]));
    assert!(sound(vec![table(&reader, 1, b"a", b"z", 2)]));
    assert!(sound(vec![
        table(&reader, 1, b"a", b"c", 2),
        table(&reader, 2, b"c", b"f", 0),
        table(&reader, 3, b"g", b"h", 2),
    ]));
    // A real overlap, in either order.
    assert!(!sound(vec![
        table(&reader, 1, b"a", b"m", 2),
        table(&reader, 2, b"c", b"p", 0),
    ]));
    assert!(!sound(vec![
        table(&reader, 1, b"c", b"p", 2),
        table(&reader, 2, b"a", b"m", 2),
    ]));
    // L0 may overlap freely.
    let l0 = version_with(
        0,
        vec![
            table(&reader, 1, b"a", b"z", 2),
            table(&reader, 2, b"a", b"z", 2),
        ],
    );
    assert!(l0.levels_are_sorted_runs());
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "overlap beyond a shared boundary key")]
fn applying_tables_that_overlap_below_l0_trips_the_debug_check() {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();

    let wide = make_live_sst(&sst_dir, 1, b"a", b"m");
    let inside = make_live_sst(&sst_dir, 2, b"c", b"d");
    vs.apply(&[VersionEdit::AddFile {
        level: 1,
        file: wide,
    }])
    .unwrap();
    let _ = vs.apply(&[VersionEdit::AddFile {
        level: 1,
        file: inside,
    }]);
}

/// A MANIFEST holding exactly `records`, after its stamp.
fn write_manifest(dir: &Path, records: &[ManifestRecord]) {
    let mut bytes = VersionSet::encode_stamp().to_vec();
    bytes.extend_from_slice(
        &VersionSet::encode_records(records, MANIFEST_STAMP_LEN as u64, None)
            .unwrap()
            .0,
    );
    std::fs::write(dir.join("MANIFEST"), bytes).unwrap();
}

#[test]
fn legacy_overlap_is_repaired_but_duplicate_deep_table_references_are_refused() {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    let wide = make_live_sst(&sst_dir, 1, b"a", b"m");
    let inside = make_live_sst(&sst_dir, 2, b"c", b"d");
    let add = |level, file: &Arc<LiveSst>| ManifestRecord::AddFile {
        level,
        meta: file.meta.clone(),
    };

    write_manifest(dir.path(), &[add(1, &wide), add(1, &inside)]);
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert_eq!(ids(&vs.current().levels[0]), [1, 2]);
    assert!(vs.current().levels[1].is_empty());
    let moved = vs.take_demotion().expect("the open moved the level");
    assert_eq!((moved.tables, moved.deepest_level), (2, 1));
    drop(vs);

    // A duplicate reference is not a legacy range-widening defect.
    write_manifest(dir.path(), &[add(2, &wide), add(2, &wide)]);
    let error = match VersionSet::open(dir.path(), &sst_dir) {
        Err(e) => e,
        Ok(_) => panic!("duplicate references must not be repaired into L0"),
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("more than once"));

    // The same records at L0 are fine: its tables overlap by design.
    write_manifest(dir.path(), &[add(0, &wide), add(0, &inside), add(0, &wide)]);
    let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert_eq!(ids(&vs.current().levels[0]), [1, 2, 1]);
}

/// A level laid out from `(gap, width)` pairs over two-byte keys: each table
/// starts `gap` after the previous one ended (zero is a shared boundary) and
/// spans `width` (zero is a single key).
fn laid_out(reader: &Arc<SsTableReader>, shape: &[(u16, u16)]) -> Vec<Arc<LiveSst>> {
    let key = |n: u16| n.to_be_bytes();
    let mut cursor = 0u16;
    let mut files = Vec::new();
    for (index, &(gap, width)) in shape.iter().enumerate() {
        let start = cursor + gap;
        let end = start + width;
        files.push(table(reader, index as u64 + 1, &key(start), &key(end), 1));
        cursor = end;
    }
    files
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn the_search_matches_a_linear_filter_on_any_sorted_level(
        shape in proptest::collection::vec((0u16..4, 0u16..5), 0..24),
        probes in proptest::collection::vec((0u16..140, 0u16..40), 1..16),
    ) {
        let dir = TempDir::new().unwrap();
        let reader = shared_reader(dir.path());
        let level = laid_out(&reader, &shape);
        prop_assert!(version_with(1, level.clone()).levels_are_sorted_runs());

        let linear = |first: &[u8], last: &[u8]| -> Vec<u64> {
            level
                .iter()
                .filter(|f| f.meta.smallest_key.as_slice() <= last && f.meta.largest_key.as_slice() >= first)
                .map(|f| f.meta.file_id)
                .collect()
        };
        for (low, span) in probes {
            let (first, last) = (low.to_be_bytes(), (low + span).to_be_bytes());
            prop_assert_eq!(ids(overlapping(&level, &first, &last)), linear(&first, &last));
            prop_assert_eq!(ids(covering(&level, &first)), linear(&first, &first));
        }
    }
}

/// One step of a random workload over keys `k0000` through `k0299`.
#[derive(Clone, Debug)]
enum Step {
    Put(u16),
    Delete(u16),
    DeleteRange(u16, u16),
    Flush,
    /// One compaction job of the kind the background workers pick.
    Pick,
    /// Everything pushed to the bottom.
    Settle,
    /// A manual compaction of a key range.
    Bounded(u16, u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        6 => (0u16..300).prop_map(Step::Put),
        2 => (0u16..300).prop_map(Step::Delete),
        1 => (0u16..300, 1u16..60).prop_map(|(lo, width)| Step::DeleteRange(lo, lo + width)),
        1 => Just(Step::Flush),
        2 => Just(Step::Pick),
        1 => Just(Step::Settle),
        2 => (0u16..300, 1u16..200).prop_map(|(lo, width)| Step::Bounded(lo, lo + width)),
    ]
}

fn key(n: u16) -> Vec<u8> {
    format!("k{n:04}").into_bytes()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    /// Whatever the engine writes, a level below L0 stays one sorted run:
    /// small tables so levels hold many, tombstones that leave tables of
    /// their own, and manual compactions over arbitrary key ranges.
    #[test]
    fn levels_stay_sorted_runs_through_random_writes_deletes_and_partial_compactions(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        let dir = TempDir::new().unwrap();
        let options = Options::default()
.max_background_compactions(0)
.write_buffer_size(4 * 1024)
.target_file_size(1024)
.block_size(256)
.level_base_bytes(8 * 1024);
        let db = Db::open(dir.path(), options.clone()).unwrap();
        let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (index, step) in steps.iter().enumerate() {
            match *step {
                Step::Put(n) => {
                    let value = format!("value-{n}-{index}-{}", "x".repeat(40)).into_bytes();
                    crate::through_stalls(|| db.put(&key(n), &value)).unwrap();
                    model.insert(key(n), value);
                }
                Step::Delete(n) => {
                    crate::through_stalls(|| db.delete(&key(n))).unwrap();
                    model.remove(&key(n));
                }
                Step::DeleteRange(lo, hi) => {
                    crate::through_stalls(|| db.delete_range(&key(lo), &key(hi))).unwrap();
                    model.retain(|k, _| !(key(lo)..key(hi)).contains(k));
                }
                Step::Flush => db.flush().unwrap(),
                Step::Pick => {
                    db.compact_step().unwrap();
                }
                Step::Settle => db.compact_range(None, None).wait().unwrap(),
                Step::Bounded(lo, hi) => {
                    db.compact_range(Some(&key(lo)), Some(&key(hi))).wait().unwrap();
                }
            }
            prop_assert!(
                db.engine().current_version().levels_are_sorted_runs(),
                "after step {index}: {step:?}"
            );
        }

        let probes: Vec<Vec<u8>> = (0..320).map(key).collect();
        let refs: Vec<&[u8]> = probes.iter().map(Vec::as_slice).collect();
        let check = |db: &Db, what: &str| -> Result<(), TestCaseError> {
            let batch = db.multi_get(&refs).unwrap();
            for (probe, got) in probes.iter().zip(&batch) {
                let single = db.get(probe).unwrap();
                prop_assert_eq!(got.as_ref(), model.get(probe), "{} multi_get {:?}", what, probe);
                prop_assert_eq!(single.as_ref(), model.get(probe), "{} get {:?}", what, probe);
            }
            Ok(())
        };
        check(&db, "live")?;
        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), options).unwrap();
        prop_assert!(db.engine().current_version().levels_are_sorted_runs());
        check(&db, "reopened")?;
    }
}
