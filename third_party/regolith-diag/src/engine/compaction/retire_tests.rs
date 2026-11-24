//! E27: a tombstone retires once compaction has pushed it below every
//! version it covers and no live snapshot needs it, so dropped column
//! families stop costing every later compaction (TombstoneRetirement.tla).

use tempfile::TempDir;

use crate::{Db, Options};

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

/// Every range tombstone the current version's tables carry.
fn carried_tombstones(db: &Db) -> usize {
    db.engine
        .published_version()
        .levels
        .iter()
        .flatten()
        .map(|file| file.reader.range_tombstones().len())
        .sum()
}

/// Every entry the current version's tables hold: what a pass over all of
/// them reads.
fn table_entries(db: &Db) -> u64 {
    db.engine
        .published_version()
        .levels
        .iter()
        .flatten()
        .map(|file| file.meta.num_entries)
        .sum()
}

#[test]
fn dropped_column_families_stop_costing_compaction() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    let mut carried = Vec::new();
    let mut left_behind = Vec::new();
    for cycle in 0..24u32 {
        let cf = db.create_column_family(&format!("cf{cycle}")).unwrap();
        for i in 0..16u32 {
            db.put_cf(&cf, format!("k{i:02}").as_bytes(), b"dropped")
                .unwrap();
        }
        db.flush().unwrap();
        db.drop_column_family(cf).unwrap();
        db.put(format!("live{cycle:02}").as_bytes(), b"kept")
            .unwrap();
        db.flush().unwrap();
        // What the next pass has to carry, measured before it runs.
        carried.push(carried_tombstones(&db));
        db.compact_range(None, None).wait().unwrap();
        assert_eq!(
            carried_tombstones(&db),
            0,
            "cycle {cycle}: a full compaction left a dropped family's tombstone behind"
        );
        // Beyond the live keys: the metadata family's own entries, and the
        // deletion of each dropped family's name unless it retired.
        left_behind.push(table_entries(&db) - u64::from(cycle + 1));
    }
    assert!(
        carried.iter().all(|&n| n <= 1),
        "a pass carried more than the latest drop: {carried:?}"
    );
    assert!(
        left_behind.windows(2).all(|pair| pair[0] == pair[1]),
        "the tables grow with the drops: {left_behind:?}"
    );
    for cycle in 0..24u32 {
        assert_eq!(
            db.get(format!("live{cycle:02}").as_bytes())
                .unwrap()
                .as_deref(),
            Some(&b"kept"[..])
        );
    }
    let recreated = db.create_column_family("cf0").unwrap();
    assert_eq!(db.get_cf(&recreated, b"k00").unwrap(), None);
}

#[test]
fn deleted_keys_stay_deleted_once_their_tombstone_retires() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    for i in 0..32u32 {
        db.put(format!("k{i:02}").as_bytes(), b"old").unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    db.delete_range(b"k04", b"k20").unwrap();
    db.flush().unwrap();

    // The first passes move the tombstone down past nothing it covers: the
    // keys it deletes are still deeper, so it must survive them.
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(carried_tombstones(&db), 0);
    let check = |db: &Db| {
        for i in 0..32u32 {
            let expected = (!(4..20).contains(&i)).then_some(&b"old"[..]);
            assert_eq!(
                db.get(format!("k{i:02}").as_bytes()).unwrap().as_deref(),
                expected,
                "k{i:02}"
            );
        }
        assert_eq!(db.scan(None, None).unwrap().len(), 16);
    };
    check(&db);
    drop(db);
    check(&Db::open(dir.path(), options()).unwrap());
}

#[test]
fn a_tombstone_a_live_snapshot_needs_is_kept_until_a_pass_after_its_release() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    for i in 0..8u32 {
        db.put(format!("k{i}").as_bytes(), b"old").unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    let before = db.snapshot();
    db.delete_range(b"k2", b"k6").unwrap();
    db.compact_range(None, None).wait().unwrap();

    assert_eq!(
        carried_tombstones(&db),
        1,
        "a snapshot older than the tombstone still reads the keys it deletes"
    );
    let deleted = |i: u32| (2..6).contains(&i);
    for i in 0..8u32 {
        let key = format!("k{i}");
        assert_eq!(
            before.get(key.as_bytes()).unwrap().as_deref(),
            Some(&b"old"[..])
        );
        let expected = (!deleted(i)).then_some(&b"old"[..]);
        assert_eq!(
            db.get(key.as_bytes()).unwrap().as_deref(),
            expected,
            "{key}"
        );
    }

    // Released, and replaced by a snapshot that sees the tombstone, which
    // does not hold it. The next pass over its range retires it. A pass
    // rewrites a bottom-level table only when data from above reaches its
    // range, so a write into the range brings one.
    drop(before);
    let after = db.snapshot();
    db.put(b"k7", b"new").unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(carried_tombstones(&db), 0);
    for i in 0..8u32 {
        let key = format!("k{i}");
        let expected = (!deleted(i)).then_some(&b"old"[..]);
        assert_eq!(
            after.get(key.as_bytes()).unwrap().as_deref(),
            expected,
            "{key}"
        );
        let head = if i == 7 { Some(&b"new"[..]) } else { expected };
        assert_eq!(db.get(key.as_bytes()).unwrap().as_deref(), head, "{key}");
    }
}
