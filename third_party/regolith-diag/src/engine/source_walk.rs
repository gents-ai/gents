//! The newest-first walk over a view's sources for one key. Every single-key
//! probe (the point read, the merge-chain read, the commit check's
//! latest-version and terminator probes) shares it, so they cannot disagree
//! about which source shadows which. `multi_get_in_view` keeps its own walk
//! because it serves many keys at once: source by source through the
//! memtables and L0, so each is visited once for the batch, and key by key
//! below that, where [`covering`](super::manifest::covering) finds a key's
//! tables directly.

use std::io;
use std::ops::ControlFlow;

use super::manifest::covering;
use super::memtable::MemTable;
use super::read_view::ReadView;
use super::sstable::SsTableReader;

/// One source a key's entries can come from.
pub(crate) enum Source<'view> {
    /// The active memtable or a frozen one.
    Memtable(&'view MemTable),
    /// One SSTable that may hold the key.
    Table(&'view SsTableReader),
}

/// What visiting one source returns: break to stop the walk there.
pub(crate) type VisitResult<B> = io::Result<ControlFlow<B>>;

/// How a walk ended: broken with a visit's value, or carried past every
/// source with the newest covering range-tombstone sequence.
pub(crate) type WalkResult<B> = io::Result<ControlFlow<B, u64>>;

/// Visits one source of a view with the newest sequence of a range
/// tombstone covering the key seen so far.
pub(crate) trait VisitSource<'view, B>: FnMut(Source<'view>, u64) -> VisitResult<B> {}

impl<'view, B, F> VisitSource<'view, B> for F where F: FnMut(Source<'view>, u64) -> VisitResult<B> {}
/// Where one source's skip over a key's merge operands above a floor ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Skip {
    /// Whether an operand above the floor was passed over.
    pub(crate) passed: bool,
    /// The entry the skip stopped at: a value or deletion above the floor,
    /// or any entry at or below it. `None` when the key's entries here ran
    /// out first.
    pub(crate) stop: Option<u64>,
    /// The value type of the entry `stop` names. Meaningless when `stop` is
    /// `None`.
    pub(crate) stop_type: u8,
}

/// How one source's visit of a key's entries above a floor ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Above<R> {
    /// The visit broke with a result.
    Broke(R),
    /// It reached an entry at or below the floor, so every older entry, in
    /// this source and the older ones, is below it too.
    Floor,
    /// This source's entries for the key ran out above the floor; an older
    /// source may hold more.
    Exhausted,
}

impl ReadView {
    /// Visits the sources of this view that may hold `key`, newest first: the
    /// active memtable, the frozen ones, every L0 table, then each deeper
    /// level's tables whose key range covers `key`, found by binary search
    /// because such a level is one sorted run. `visit` sees each with the
    /// newest sequence of a range tombstone covering `key` at `snapshot_seq`
    /// seen so far: a memtable's or an L0 table's own count before it is
    /// visited, a deeper level's are gathered across all its covering tables
    /// before any of them is, since a tombstone can sit in a table other than
    /// the one holding the key. `visit` breaks to stop the walk; once every
    /// source was visited, the walk continues with the final tombstone
    /// sequence.
    #[inline]
    pub(crate) fn walk_newest_first<'view, B>(
        &'view self,
        key: &[u8],
        snapshot_seq: u64,
        mut visit: impl VisitSource<'view, B>,
    ) -> WalkResult<B> {
        let mut max_rt_seq: u64 = 0;

        for mt in std::iter::once(&self.active).chain(self.frozen.iter().rev()) {
            max_rt_seq = max_rt_seq.max(mt.covering_range_tombstone_seq(key, snapshot_seq));
            if let ControlFlow::Break(done) = visit(Source::Memtable(mt), max_rt_seq)? {
                return Ok(ControlFlow::Break(done));
            }
        }

        let levels = &self.version.levels;
        // Every L0 table, whatever its key range: they may overlap, and a
        // filter here would change which bloom checks a point read runs.
        for file in levels[0].iter().rev() {
            max_rt_seq =
                max_rt_seq.max(file.reader.covering_range_tombstone_seq(key, snapshot_seq));
            if let ControlFlow::Break(done) = visit(Source::Table(&file.reader), max_rt_seq)? {
                return Ok(ControlFlow::Break(done));
            }
        }

        for files in levels.iter().skip(1) {
            let run = covering(files, key);
            if run.is_empty() {
                continue;
            }
            for file in run {
                max_rt_seq =
                    max_rt_seq.max(file.reader.covering_range_tombstone_seq(key, snapshot_seq));
            }
            for file in run.iter().filter(|file| file.meta.num_entries > 0) {
                if let ControlFlow::Break(done) = visit(Source::Table(&file.reader), max_rt_seq)? {
                    return Ok(ControlFlow::Break(done));
                }
            }
        }

        Ok(ControlFlow::Continue(max_rt_seq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
    use crate::engine::lookup_key::LookupKey;
    use crate::engine::manifest::Version;
    use crate::engine::memtable::MemTableConfig;
    use crate::engine::sstable::{LiveSst, Materialize, PointValue, SsTableMeta, SsTableWriter};
    use crate::options::CompressionType;
    use crate::{Db, Options};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// A database where only an explicit flush moves data between sources.
    fn open(dir: &TempDir) -> Db {
        let options = Options::default().max_background_compactions(0);
        Db::open(dir.path(), options).unwrap()
    }

    /// `name` as the default column family stores it.
    fn key_of(name: &[u8]) -> Vec<u8> {
        prefix_key(DEFAULT_CF_ID, name)
    }

    /// Seal the active memtable and leave it frozen.
    fn freeze_active(db: &Db) {
        db.engine().seal_active().unwrap();
    }

    /// Where `view` keeps `source`: "active", "frozen N" or "L<level> N",
    /// N counting from the oldest.
    fn name_of(view: &ReadView, source: &Source<'_>) -> String {
        match source {
            Source::Memtable(mt) if std::ptr::eq(*mt, &*view.active) => "active".to_string(),
            Source::Memtable(mt) => {
                let n = view
                    .frozen
                    .iter()
                    .position(|frozen| std::ptr::eq(*mt, &**frozen))
                    .expect("a frozen memtable of the view");
                format!("frozen {n}")
            }
            Source::Table(reader) => view
                .version
                .levels
                .iter()
                .enumerate()
                .find_map(|(level, files)| {
                    files
                        .iter()
                        .position(|file| std::ptr::eq(*reader, &*file.reader))
                        .map(|n| format!("L{level} {n}"))
                })
                .expect("a table of the view"),
        }
    }

    /// Walk `view` for `key` without ever breaking: every source visited
    /// with the tombstone sequence it was handed, and the sequence the walk
    /// ended with.
    fn walk_all(view: &ReadView, key: &[u8], snapshot_seq: u64) -> (Vec<(String, u64)>, u64) {
        let mut visited = Vec::new();
        let ended = view
            .walk_newest_first(&key_of(key), snapshot_seq, |source, max_rt_seq| {
                visited.push((name_of(view, &source), max_rt_seq));
                Ok(ControlFlow::<()>::Continue(()))
            })
            .unwrap();
        match ended {
            ControlFlow::Continue(max_rt_seq) => (visited, max_rt_seq),
            ControlFlow::Break(()) => unreachable!("the visit never breaks"),
        }
    }

    #[test]
    fn the_walk_visits_memtables_before_tables_newest_first_and_stops_at_the_first_break() {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        crate::through_stalls(|| db.put(b"a", b"1")).unwrap();
        db.flush().unwrap();
        crate::through_stalls(|| db.put(b"b", b"2")).unwrap();
        db.flush().unwrap();
        crate::through_stalls(|| db.put(b"c", b"3")).unwrap();
        freeze_active(&db);
        crate::through_stalls(|| db.put(b"d", b"4")).unwrap();
        let view = db.engine().view.load();

        // Every L0 table is visited, whatever its key range.
        let (visited, ended) = walk_all(&view, b"a", u64::MAX);
        let places: Vec<&str> = visited.iter().map(|(place, _)| place.as_str()).collect();
        assert_eq!(places, ["active", "frozen 0", "L0 1", "L0 0"]);
        assert_eq!(ended, 0);

        for (stop_at, expected) in [
            ("active", &["active"][..]),
            ("L0 1", &["active", "frozen 0", "L0 1"][..]),
        ] {
            let mut seen = Vec::new();
            let walked = view
                .walk_newest_first(&key_of(b"a"), u64::MAX, |source, _| {
                    let name = name_of(&view, &source);
                    let stop = name == stop_at;
                    seen.push(name);
                    Ok(if stop {
                        ControlFlow::Break(7)
                    } else {
                        ControlFlow::Continue(())
                    })
                })
                .unwrap();
            assert!(matches!(walked, ControlFlow::Break(7)));
            assert_eq!(seen, expected);
        }
    }

    #[test]
    fn an_exhausted_walk_continues_with_the_newest_covering_tombstone_sequence() {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        crate::through_stalls(|| db.put(b"k", b"v")).unwrap();
        db.flush().unwrap();
        // Held below the tombstones, so that the step a later write owes
        // does not compact the first one away with the key it covers
        // (`compaction::deletion`).
        let _below_the_tombstones = db.snapshot();
        crate::through_stalls(|| db.delete_range(b"a", b"m")).unwrap();
        let first = db.latest_sequence();
        {
            let view = db.engine().view.load();
            assert_eq!(walk_all(&view, b"k", u64::MAX).1, first);
            assert_eq!(walk_all(&view, b"z", u64::MAX).1, 0, "not covered");
            assert_eq!(walk_all(&view, b"m", u64::MAX).1, 0, "the end is exclusive");
        }

        // The first tombstone moves into a table; a newer one covers `k` too.
        db.flush().unwrap();
        crate::through_stalls(|| db.delete_range(b"b", b"z")).unwrap();
        let second = db.latest_sequence();
        let view = db.engine().view.load();
        assert_eq!(walk_all(&view, b"k", u64::MAX).1, second);
        assert_eq!(
            walk_all(&view, b"k", first).1,
            first,
            "a tombstone above the snapshot is not seen"
        );
        assert_eq!(walk_all(&view, b"k", first - 1).1, 0);
    }

    #[test]
    fn a_tombstone_in_a_newer_source_reaches_the_visit_of_an_older_table() {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        crate::through_stalls(|| db.put(b"k", b"v")).unwrap();
        db.flush().unwrap();
        crate::through_stalls(|| db.delete_range(b"a", b"z")).unwrap();
        let tombstone = db.latest_sequence();
        db.flush().unwrap();
        let view = db.engine().view.load();

        let (visited, ended) = walk_all(&view, b"k", u64::MAX);
        let expected = [("active", 0), ("L0 1", tombstone), ("L0 0", tombstone)];
        assert_eq!(visited, expected.map(|(place, rt)| (place.to_string(), rt)));
        assert_eq!(ended, tombstone);
    }

    #[test]
    fn a_tombstone_in_an_older_source_does_not_reach_the_visit_of_a_newer_one() {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        // A table holding nothing but a tombstone is compacted away by the
        // step the next write owes, unless a snapshot below the tombstone
        // is live (`compaction::deletion`). This one keeps it in L0.
        let _below_the_tombstone = db.snapshot();
        crate::through_stalls(|| db.delete_range(b"a", b"z")).unwrap();
        let tombstone = db.latest_sequence();
        db.flush().unwrap();
        crate::through_stalls(|| db.put(b"k", b"v")).unwrap();
        db.flush().unwrap();
        let view = db.engine().view.load();

        let (visited, ended) = walk_all(&view, b"k", u64::MAX);
        let expected = [("active", 0), ("L0 1", 0), ("L0 0", tombstone)];
        assert_eq!(visited, expected.map(|(place, rt)| (place.to_string(), rt)));
        assert_eq!(ended, tombstone);
    }

    /// A table of the point entries `points` (key, sequence) and the range
    /// tombstones `tombstones` (start, end, sequence), as a level lists it.
    fn live_table(
        dir: &TempDir,
        file_id: u64,
        points: &[(&[u8], u64)],
        tombstones: &[(&[u8], &[u8], u64)],
    ) -> Arc<LiveSst> {
        let path = dir.path().join(format!("{file_id}.sst"));
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        for &(key, seq) in points {
            let internal = encode_internal_key(&key_of(key), seq, VALUE_TYPE_VALUE);
            writer.add(&internal, b"v").unwrap();
        }
        for &(start, end, seq) in tombstones {
            writer.add_range_tombstone(&key_of(start), &key_of(end), seq);
        }
        let summary = writer.finish().unwrap().unwrap();
        let meta = SsTableMeta {
            file_id,
            smallest_key: summary.smallest_user_key,
            largest_key: summary.largest_user_key,
            file_size: 0,
            num_entries: summary.num_entries,
            global_seq: None,
            deletions: None,
        };
        LiveSst::new(
            meta,
            Arc::new(SsTableReader::open(&path, file_id).unwrap()),
            summary.newest_seq,
        )
    }

    /// A view of an empty memtable over `version`.
    fn view_over(version: Version) -> ReadView {
        ReadView {
            active: Arc::new(MemTable::new(&MemTableConfig::default()).unwrap()),
            frozen: Vec::new(),
            version: Arc::new(version),
        }
    }

    #[test]
    fn a_deeper_levels_tombstones_are_gathered_before_any_of_its_tables_is_visited() {
        let dir = TempDir::new().unwrap();
        // L1 lists the table holding `m` first and, after it, one holding only
        // a newer tombstone over `m` and beyond, which starts where it ends.
        let holder = live_table(&dir, 1, &[(b"a", 5), (b"k", 5), (b"m", 5)], &[]);
        let tombstone_only = live_table(&dir, 2, &[], &[(b"m", b"p", 10)]);
        let mut version = Version::new();
        version.levels[1] = vec![holder, tombstone_only];
        assert!(version.levels_are_sorted_runs());
        let view = view_over(version);
        let at = |place: &str, rt| (place.to_string(), rt);

        // The table without entries is not visited, but its tombstone is
        // seen by the one that holds the key.
        let (visited, ended) = walk_all(&view, b"m", u64::MAX);
        assert_eq!(visited, [at("active", 0), at("L1 0", 10)]);
        assert_eq!(ended, 10);

        // Before the tombstone's range, the holder sees none.
        let (visited, ended) = walk_all(&view, b"k", u64::MAX);
        assert_eq!(visited, [at("active", 0), at("L1 0", 0)]);
        assert_eq!(ended, 0);

        // Where only the tombstone's table covers the key, nothing is visited
        // but the walk still ends with the tombstone.
        let (visited, ended) = walk_all(&view, b"n", u64::MAX);
        assert_eq!(visited, [at("active", 0)]);
        assert_eq!(ended, 10);

        // Outside every table's key range, only the memtable is visited.
        for key in [b"0".as_slice(), b"q"] {
            let (visited, ended) = walk_all(&view, key, u64::MAX);
            assert_eq!(visited, [at("active", 0)], "{key:?}");
            assert_eq!(ended, 0, "{key:?}");
        }
    }

    #[test]
    fn a_deep_level_of_many_tables_visits_only_the_one_covering_the_key() {
        let dir = TempDir::new().unwrap();
        // Table i holds t(2i) and t(2i + 1), so the level is 30 tables with a
        // gap between each pair.
        let key = |n: u64| format!("t{n:03}").into_bytes();
        let tables: Vec<_> = (0..30)
            .map(|i| {
                let points = [(key(2 * i), 5), (key(2 * i + 1), 5)];
                let points: Vec<(&[u8], u64)> =
                    points.iter().map(|(k, seq)| (k.as_slice(), *seq)).collect();
                live_table(&dir, i + 1, &points, &[])
            })
            .collect();
        let mut version = Version::new();
        version.levels[2] = tables;
        let view = view_over(version);

        for n in 0..60 {
            let (visited, _) = walk_all(&view, &key(n), u64::MAX);
            let names: Vec<&str> = visited.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, ["active", &format!("L2 {}", n / 2)], "t{n:03}");
        }
        // A key between a table's own two keys is still covered by it.
        let (visited, _) = walk_all(&view, b"t000\0", u64::MAX);
        assert_eq!(visited.len(), 2);

        // Between two tables, before the first, and after the last.
        for missing in [b"t0075".as_slice(), b"s", b"u"] {
            let (visited, ended) = walk_all(&view, missing, u64::MAX);
            assert_eq!(visited.len(), 1, "{missing:?}");
            assert_eq!(ended, 0, "{missing:?}");
        }
    }

    #[test]
    fn a_batch_read_and_a_single_key_read_agree_on_how_deeper_level_tombstones_hide_entries() {
        let tables = TempDir::new().unwrap();
        // L1 holds, in key order: a, c and e; a tombstone-only table over
        // [e, h) at sequence 10 that starts where the first ends; i and k;
        // and a tombstone-only table over [k, m) at sequence 3, older than
        // the entry for k it starts beside. L2 holds older entries.
        let mut version = Version::new();
        version.levels[1] = vec![
            live_table(&tables, 1, &[(b"a", 5), (b"c", 5), (b"e", 5)], &[]),
            live_table(&tables, 2, &[], &[(b"e", b"h", 10)]),
            live_table(&tables, 3, &[(b"i", 5), (b"k", 5)], &[]),
            live_table(&tables, 4, &[], &[(b"k", b"m", 3)]),
        ];
        version.levels[2] = vec![live_table(
            &tables,
            5,
            &[(b"a", 2), (b"e", 2), (b"g", 2), (b"k", 2)],
            &[],
        )];
        assert!(version.levels_are_sorted_runs());
        let view = view_over(version);

        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        let engine = db.engine();

        let keys: [&[u8]; 16] = [
            b"0", b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h", b"hh", b"i", b"j", b"k", b"l",
            b"m", b"z",
        ];
        let prefixed: Vec<Vec<u8>> = keys.iter().map(|key| key_of(key)).collect();
        let refs: Vec<&[u8]> = prefixed.iter().map(Vec::as_slice).collect();
        let batch = engine.multi_get_in_view(&refs, u64::MAX, &view).unwrap();

        for (key, got) in keys.iter().zip(&batch) {
            let lk = LookupKey::new(DEFAULT_CF_ID, key, u64::MAX);
            let single = engine
                .lookup_in_view(
                    lk.prefixed_user_key(),
                    u64::MAX,
                    &lk,
                    Materialize::Value,
                    &view,
                )
                .unwrap()
                .map(|found| match found {
                    PointValue::Value(value) => value.to_vec(),
                    PointValue::Length(_) => unreachable!("a value was asked for"),
                });
            assert_eq!(got, &single, "{key:?}: batch against single-key read");
        }

        // What each read is meant to say: `e` and `g` are hidden by the newer
        // tombstone, `g` from a deeper level than the one that holds it; `k`
        // outlives the older tombstone beside it.
        let visible: Vec<&[u8]> = keys
            .iter()
            .zip(&batch)
            .filter(|(_, got)| got.is_some())
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(visible, [b"a".as_slice(), b"c", b"i", b"k"]);
    }
}
