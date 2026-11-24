#![cfg(not(target_arch = "wasm32"))]

mod recording_env;

use std::sync::Arc;

pub(crate) use recording_env::{Recording, RecordingEnv};

use super::{MemTable, Wal, WalEntry, rewrite_recovered_memtable_to_wal};

#[test]
fn recovery_groups_preserve_versions_and_all_record_types() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovered.wal");
    let memtable = MemTable::new(&Default::default()).unwrap();
    let mut expected = Vec::new();
    for seq in 1..=100u64 {
        let key = seq.to_be_bytes().to_vec();
        let value = vec![seq as u8; if seq == 50 { 128 * 1024 } else { 2048 }];
        memtable.put(&key, &value, seq);
        expected.push(WalEntry::Put { key, value, seq });
    }
    memtable.put(b"versioned", b"old", 101);
    memtable.put(b"versioned", b"new", 102);
    expected.push(WalEntry::Put {
        key: b"versioned".to_vec(),
        value: b"old".to_vec(),
        seq: 101,
    });
    expected.push(WalEntry::Put {
        key: b"versioned".to_vec(),
        value: b"new".to_vec(),
        seq: 102,
    });
    memtable.delete(b"deleted", 103);
    expected.push(WalEntry::Delete {
        key: b"deleted".to_vec(),
        seq: 103,
    });
    memtable.merge(b"merged", b"operand", 104);
    expected.push(WalEntry::Merge {
        key: b"merged".to_vec(),
        operand: b"operand".to_vec(),
        seq: 104,
    });
    memtable.delete_range(b"start", b"stop", 105);
    expected.push(WalEntry::DeleteRange {
        start: b"start".to_vec(),
        end: b"stop".to_vec(),
        seq: 105,
    });
    let mut wal = Wal::create(&path).unwrap();
    rewrite_recovered_memtable_to_wal(&memtable, &mut wal).unwrap();
    drop(wal);
    let actual = Wal::replay(&path).unwrap();
    assert_eq!(actual.len(), expected.len());
    for record in expected {
        assert!(actual.contains(&record), "missing recovered record");
    }
}

#[test]
fn recovery_sync_failure_keeps_original_logs_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let db = crate::Db::open(dir.path(), crate::Options::default()).unwrap();
    for seq in 1..=100u64 {
        db.put(&seq.to_be_bytes(), &[42; 2048]).unwrap();
    }
    drop(db);
    let originals: Vec<_> = std::fs::read_dir(&wal_dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(!originals.is_empty());
    super::wal::fault::arm_sync_failure(dir.path());
    let result = crate::Db::open(dir.path(), crate::Options::default());
    super::wal::fault::disarm_sync_failure(dir.path());
    assert!(result.is_err(), "recovery must propagate the sync failure");
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    let db = crate::Db::open(dir.path(), crate::Options::default()).unwrap();
    for seq in 1..=100u64 {
        assert_eq!(
            db.get(&seq.to_be_bytes()).unwrap().unwrap().as_slice(),
            &[42; 2048]
        );
    }
}

fn framed_put(memtable: &MemTable, key: &[u8], seq: u64, size: usize) -> Vec<u8> {
    let value = vec![seq as u8; size - super::wal::put_record_len(key, &[])];
    memtable.put(key, &value, seq);
    let mut record = Vec::new();
    super::wal::encode_put_record(&mut record, key, &value, seq);
    assert_eq!(record.len(), size);
    record
}

fn recording_wal(path: &std::path::Path) -> (Wal, Arc<RecordingEnv>) {
    let recording = Arc::new(RecordingEnv::default());
    let env: Arc<dyn crate::env::Env> = recording.clone();
    let wal = Wal::create_in(&env, path, None).unwrap();
    *recording.events.lock().unwrap() = Recording::default();
    (wal, recording)
}

#[test]
fn recovery_groups_bound_writes_and_keep_large_records_separate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovered.wal");
    let memtable = MemTable::new(&Default::default()).unwrap();
    let sizes = [
        16_384, 16_384, 32_768, 16_384, 49_153, 131_072, 32_768, 32_768,
    ];
    let records: Vec<_> = sizes
        .into_iter()
        .enumerate()
        .map(|(index, size)| framed_put(&memtable, &[index as u8], index as u64 + 1, size))
        .collect();
    let (mut wal, recording) = recording_wal(&path);
    rewrite_recovered_memtable_to_wal(&memtable, &mut wal).unwrap();
    let expected = vec![
        records[..3].concat(),
        records[3].clone(),
        records[4].clone(),
        records[5].clone(),
        records[6..].concat(),
    ];
    let events = recording.events.lock().unwrap();
    assert_eq!(payloads(&events.writes), expected);
    assert_eq!(events.data_syncs, 1);
    assert_eq!(Wal::replay(&path).unwrap().len(), records.len());
}

/// Each host write is one group record: its frame, then its operations.
/// The operations, with the frames taken off.
fn payloads(writes: &[Vec<u8>]) -> Vec<Vec<u8>> {
    writes
        .iter()
        .map(|w| w[super::wal_frame::HEADER_LEN..].to_vec())
        .collect()
}

#[test]
fn recovery_packs_small_records_into_four_host_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovered.wal");
    let memtable = MemTable::new(&Default::default()).unwrap();
    let records: Vec<_> = (1..=256u64)
        .map(|seq| framed_put(&memtable, &seq.to_be_bytes(), seq, 1024))
        .collect();
    let (mut wal, recording) = recording_wal(&path);
    rewrite_recovered_memtable_to_wal(&memtable, &mut wal).unwrap();
    let events = recording.events.lock().unwrap();
    assert_eq!(events.writes.len(), 4);
    let groups = payloads(&events.writes);
    assert!(groups.iter().all(|group| group.len() == 64 * 1024));
    assert_eq!(groups.concat(), records.concat());
    assert_eq!(events.data_syncs, 1);
}

#[test]
fn empty_recovery_does_not_append_or_sync() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovered.wal");
    let memtable = MemTable::new(&Default::default()).unwrap();
    let (mut wal, recording) = recording_wal(&path);
    rewrite_recovered_memtable_to_wal(&memtable, &mut wal).unwrap();
    let events = recording.events.lock().unwrap();
    assert!(events.writes.is_empty());
    assert_eq!(events.data_syncs, 0);
}

#[test]
fn partial_recovery_write_keeps_original_logs_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let recording = Arc::new(RecordingEnv::default());
    let options = crate::Options::default()
        .env(recording.clone())
        .max_background_compactions(0);
    let db = crate::Db::open(dir.path(), options.clone()).unwrap();
    for seq in 1..=100u64 {
        db.put(&seq.to_be_bytes(), &[42; 2048]).unwrap();
    }
    drop(db);
    let originals: Vec<_> = std::fs::read_dir(&wal_dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(!originals.is_empty());
    *recording.events.lock().unwrap() = Recording {
        // The stamp and first data group succeed before the partial write.
        fail_on_write: Some(3),
        ..Default::default()
    };
    let result = crate::Db::open(dir.path(), options.clone());
    assert!(result.is_err(), "recovery must propagate the write failure");
    for (path, bytes) in &originals {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    let events = recording.events.lock().unwrap();
    assert_eq!(events.partial_failures, 1);
    assert_eq!(events.writes.len(), 3);
    assert_eq!(events.data_syncs, 0);
    let replacement_len = events.writes[0].len() + events.writes[1].len() + 7;
    drop(events);
    let replacements: Vec<_> = std::fs::read_dir(&wal_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| !originals.iter().any(|(original, _)| original == path))
        .collect();
    assert_eq!(replacements.len(), 1);
    assert_eq!(
        std::fs::metadata(&replacements[0]).unwrap().len(),
        replacement_len as u64
    );
    let db = crate::Db::open(dir.path(), options).unwrap();
    for seq in 1..=100u64 {
        assert_eq!(
            db.get(&seq.to_be_bytes()).unwrap().unwrap().as_slice(),
            &[42; 2048]
        );
    }
    db.put(b"after-retry", b"ok").unwrap();
    assert_eq!(db.get(b"after-retry").unwrap().unwrap().as_slice(), b"ok");
}

/// RED `NoTruncate` (`WalRecovery.tla`): recovery truncates the newest
/// log's dropped tail, durably, before it creates the next log. An open
/// that fails while writing that next log leaves the old log beside it as
/// an earlier log, so the old log must already be complete: no tail.
#[test]
fn a_dropped_tail_is_truncated_durably_before_the_next_log_exists() {
    let dir = tempfile::tempdir().unwrap();
    let recording = Arc::new(RecordingEnv::default());
    let options = crate::Options::default()
        .env(recording.clone())
        .max_background_compactions(0);
    let db = crate::Db::open(dir.path(), options.clone()).unwrap();
    for seq in 1..=20u64 {
        db.put(&seq.to_be_bytes(), &[7; 512]).unwrap();
    }
    drop(db);
    let log = std::fs::read_dir(dir.path().join("wal"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    let whole = std::fs::read(&log).unwrap();
    let mut damaged = whole.clone();
    damaged.extend_from_slice(&[0x5A; 333]);
    std::fs::write(&log, &damaged).unwrap();

    *recording.events.lock().unwrap() = Recording {
        // The next log's stamp, then its first group fails part way.
        fail_on_write: Some(2),
        ..Default::default()
    };
    assert!(crate::Db::open(dir.path(), options.clone()).is_err());
    assert_eq!(
        std::fs::read(&log).unwrap(),
        whole,
        "the tail is gone before the next log was written"
    );
    let events = recording.events.lock().unwrap();
    assert_eq!(
        events.log.first().map(String::as_str),
        Some("sync"),
        "the truncation is synced before anything else is written: {:?}",
        events.log
    );
    drop(events);

    let db = crate::Db::open(dir.path(), options).unwrap();
    for seq in 1..=20u64 {
        assert_eq!(
            db.get(&seq.to_be_bytes()).unwrap().unwrap().as_slice(),
            &[7; 512]
        );
    }
}

/// A read-write open lists the log directory once to find the logs it
/// replays and to remove the staging files a crash left, and once more to
/// retire the logs it replayed. Removing the staging files took a listing
/// of its own before.
#[test]
fn a_writable_open_lists_the_log_directory_twice() {
    let dir = tempfile::tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let recording = Arc::new(RecordingEnv::default());
    let options = crate::Options::default()
        .env(recording.clone())
        .max_background_compactions(0);
    let db = crate::Db::open(dir.path(), options.clone()).unwrap();
    db.put(b"key", b"value").unwrap();
    drop(db);
    let staged = wal_dir.join("wal_000099.tmp");
    std::fs::write(&staged, b"a stamp a crash left").unwrap();
    recording.events.lock().unwrap().listed.clear();

    let db = crate::Db::open(dir.path(), options).unwrap();
    assert!(!staged.exists(), "the open removes a staging file");
    let listed = recording.events.lock().unwrap().listed.clone();
    assert_eq!(
        listed.iter().filter(|path| **path == wal_dir).count(),
        2,
        "{listed:?}"
    );
    assert_eq!(db.get(b"key").unwrap().as_deref(), Some(&b"value"[..]));
}
