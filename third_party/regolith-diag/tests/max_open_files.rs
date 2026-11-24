//! A store capped by `Options::max_open_files` closes and reopens table
//! descriptors without changing what any read returns, including reads
//! through an iterator whose tables a compaction deletes underneath it.

use regolith::{Db, Options};

fn key(i: u32) -> Vec<u8> {
    format!("key{i:08}").into_bytes()
}

fn value(i: u32) -> Vec<u8> {
    format!("value{i:08}").repeat(8).into_bytes()
}

/// A store with `tables` L0 tables of `per` keys each, never compacted.
fn many_tables(dir: &std::path::Path, max_open_files: usize, tables: u32, per: u32) -> Db {
    let db = Db::open(
        dir,
        Options::default()
            .max_open_files(max_open_files)
            .l0_compaction_trigger(10_000)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0),
    )
    .unwrap();
    for t in 0..tables {
        for i in 0..per {
            let k = t * per + i;
            db.put(&key(k), &value(k)).unwrap();
        }
        db.flush().unwrap();
    }
    db
}

#[test]
fn every_key_reads_back_with_far_fewer_descriptors_than_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = many_tables(dir.path(), 4, 40, 200);
    for round in 0..2 {
        for k in 0..8_000u32 {
            assert_eq!(
                db.get(&key(k)).unwrap().as_deref(),
                Some(value(k).as_slice()),
                "round {round}: key {k}"
            );
        }
    }
}

#[test]
fn an_iterator_outlives_the_compaction_that_deletes_its_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = many_tables(dir.path(), 2, 30, 200);

    let mut iter = db.iter();
    iter.seek_to_first();
    // Read a little, so the iterator holds its version mid-scan.
    let mut seen = 0u32;
    for _ in 0..50 {
        assert!(iter.valid());
        iter.next();
        seen += 1;
    }

    db.compact_range(None, None).wait().unwrap();
    // Churn the descriptor cache so nothing the iterator needs is still
    // open by luck.
    for k in (0..6_000u32).step_by(97) {
        assert!(db.get(&key(k)).unwrap().is_some());
    }

    while iter.valid() {
        let k = seen;
        assert_eq!(iter.key(), Some(key(k).as_slice()), "iterator key {k}");
        assert_eq!(
            iter.value(),
            Some(value(k).as_slice()),
            "iterator value {k}"
        );
        iter.next();
        seen += 1;
    }
    iter.status().unwrap();
    assert_eq!(seen, 6_000);
}

#[test]
fn zero_keeps_every_descriptor_open_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let db = many_tables(dir.path(), 0, 10, 100);
    for k in 0..1_000u32 {
        assert!(db.get(&key(k)).unwrap().is_some());
    }
}

/// Readers on many threads read every key while compactions remove the
/// tables under them, with two descriptors for forty tables. Every read
/// returns its value, an iterator opened before the compactions walks the
/// tables they removed, and nothing removed is left behind once the readers
/// are done.
#[test]
fn concurrent_readers_survive_compactions_with_two_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(many_tables(dir.path(), 2, 40, 100));
    let readers: Vec<_> = (0..4u32)
        .map(|t| {
            let db = std::sync::Arc::clone(&db);
            std::thread::spawn(move || {
                let mut iter = db.iter();
                iter.seek_to_first();
                for round in 0..3u32 {
                    for k in (t..4_000u32).step_by(7) {
                        assert_eq!(
                            db.get(&key(k)).unwrap().as_deref(),
                            Some(value(k).as_slice()),
                            "reader {t} round {round}: key {k}"
                        );
                    }
                }
                let mut seen = 0u32;
                while iter.valid() {
                    assert_eq!(iter.key(), Some(key(seen).as_slice()));
                    iter.next();
                    seen += 1;
                }
                iter.status().unwrap();
                assert_eq!(seen, 4_000);
            })
        })
        .collect();
    for _ in 0..2 {
        db.compact_range(None, None).wait().unwrap();
    }
    for reader in readers {
        reader.join().unwrap();
    }
    let leftovers = || -> Vec<String> {
        std::fs::read_dir(dir.path().join("sst"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".removed-"))
            .collect()
    };
    // A table renamed aside goes with its last handle, and the last handle
    // goes with the last read view naming the table. A replaced view is freed
    // by the reclaimer once no thread can still reach it, not at the instant
    // it is replaced, so wait for that, with reads on this thread moving the
    // reclaimer on.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut backoff = std::time::Duration::from_millis(1);
    while !leftovers().is_empty() && std::time::Instant::now() < deadline {
        db.get(&key(0)).unwrap();
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    }
    assert!(leftovers().is_empty(), "{:?}", leftovers());
}
