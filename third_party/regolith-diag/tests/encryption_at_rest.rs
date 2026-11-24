//! Encryption at rest (`Options::key_provider`): every file sealed, values
//! plaintext inside the engine, and every mechanism unchanged on an
//! encrypted store.
//!
//! The frame-level rules (tags in place of checksums, the O < P rule on
//! sealed logs, `StampNotSealed`) are pinned next to the code in the
//! `sealed` test modules of the SSTable, write-ahead log and manifest; this
//! file drives the public API: round trips, merges, byte equality,
//! value-validated reads, content-addressed keys, appends, allocations,
//! wrong and unknown keys, rotation, the upgrade of an unencrypted database,
//! ingestion, checkpoints and backups.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::keys::Keys;
use regolith::prelude::*;
use regolith::{
    BackupEngine, Checkpoint, Db, DurabilityMode, Error, EventListener, IngestOptions,
    SstFileWriter,
};
use tempfile::TempDir;

/// Bytes no file of an encrypted database may hold in plaintext.
const MARKER: &[u8] = b"ENCRYPTION-AT-REST-MARKER";

fn value(i: usize) -> Vec<u8> {
    let mut v = MARKER.to_vec();
    v.extend_from_slice(format!("/{i:06}").as_bytes());
    v
}

fn key(i: usize) -> Vec<u8> {
    format!("user-key-{i:06}").into_bytes()
}

/// Sums big-endian i64 deltas.
struct CounterMerge;

impl MergeOperator for CounterMerge {
    fn name(&self) -> &'static str {
        "counter"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total = match base {
            Some(bytes) => i64::from_be_bytes(bytes.try_into().ok()?),
            None => 0,
        };
        for operand in operands {
            total += i64::from_be_bytes((*operand).try_into().ok()?);
        }
        Some(total.to_be_bytes().to_vec())
    }
}

/// Small buffers, so a few hundred writes flush and compact, under `keys`.
fn options(keys: &Arc<Keys>) -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .block_size(1024)
        .merge_operator(Some(Arc::new(CounterMerge)))
        .key_provider(keys.clone())
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// The files under `dir` that hold `needle` in plaintext.
fn files_holding(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    files_under(dir)
        .into_iter()
        .filter(|path| {
            std::fs::read(path)
                .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
                .unwrap_or(false)
        })
        .collect()
}

fn fill(db: &Db, from: usize, to: usize) {
    for i in from..to {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
    }
}

fn check(db: &Db, from: usize, to: usize) {
    for i in from..to {
        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)), "key {i}");
    }
}

#[test]
fn an_encrypted_database_round_trips_and_holds_no_plaintext_on_disk() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        fill(&db, 0, 600);
        regolith::through_stalls(|| db.delete(&key(5))).unwrap();
        regolith::through_stalls(|| db.delete_range(&key(10), &key(20))).unwrap();
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        fill(&db, 600, 700);
        db.close().unwrap();
    }
    assert!(
        files_holding(dir.path(), MARKER).is_empty(),
        "plaintext values on disk: {:?}",
        files_holding(dir.path(), MARKER)
    );
    assert!(files_holding(dir.path(), b"user-key-").is_empty());

    let db = Db::open(dir.path(), options(&keys)).unwrap();
    check(&db, 0, 5);
    assert_eq!(db.get(&key(5)).unwrap(), None);
    assert_eq!(db.get(&key(15)).unwrap(), None);
    check(&db, 20, 700);
    let mut iter = db.iter();
    iter.seek_to_first();
    let mut scanned = 0;
    while iter.valid() {
        assert!(iter.value().unwrap().starts_with(MARKER));
        scanned += 1;
        iter.next();
    }
    iter.status().unwrap();
    assert_eq!(scanned, 700 - 1 - 10);
}

#[test]
fn merges_fold_plaintext_operands_across_flushes_and_compactions() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    for round in 0..300i64 {
        regolith::through_stalls(|| db.merge(b"counter", &round.to_be_bytes())).unwrap();
        regolith::through_stalls(|| db.put(&key(round as usize), &value(round as usize))).unwrap();
        if round % 100 == 99 {
            db.flush().unwrap();
        }
    }
    db.compact_range(None, None).wait().unwrap();
    let total: i64 = (0..300).sum();
    assert_eq!(
        db.get(b"counter").unwrap(),
        Some(total.to_be_bytes().to_vec())
    );
    drop(db);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    assert_eq!(
        db.get(b"counter").unwrap(),
        Some(total.to_be_bytes().to_vec())
    );
}

fn txn_db(dir: &Path, keys: &Arc<Keys>) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, options(keys)).unwrap()
}

#[test]
fn identical_writes_and_value_validated_reads_compare_plaintext_bytes() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = txn_db(dir.path(), &keys);
    regolith::through_stalls(|| db.db().put(b"version", b"v1")).unwrap();
    db.db().flush().unwrap();

    // Byte equality: two blind writes of the same bytes, the first flushed
    // into a sealed table before the second commits.
    let first = db.begin(&TxnOptions::new());
    let second = db.begin(&TxnOptions::new());
    first.put(b"block", MARKER).unwrap();
    second.put(b"block", MARKER).unwrap();
    first.commit().unwrap();
    db.db().flush().unwrap();
    second
        .commit()
        .expect("an identical write is not a conflict");

    // A value-validated read: rewritten away and back (ABA), each version
    // flushed into a sealed table, the read still holds.
    let reader = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    assert_eq!(reader.get(b"version").unwrap().as_deref(), Some(&b"v1"[..]));
    reader.put(b"reader-wrote", b"x").unwrap();
    regolith::through_stalls(|| db.db().put(b"version", b"v2")).unwrap();
    db.db().flush().unwrap();
    regolith::through_stalls(|| db.db().put(b"version", b"v1")).unwrap();
    db.db().flush().unwrap();
    reader
        .commit()
        .expect("the value the read returned is still current");

    // And a read whose value did change still conflicts.
    let reader = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    reader.get(b"version").unwrap();
    reader.put(b"reader-wrote", b"y").unwrap();
    regolith::through_stalls(|| db.db().put(b"version", b"v3")).unwrap();
    db.db().flush().unwrap();
    assert!(matches!(
        reader.commit(),
        Err(TransactionError::Conflict(_))
    ));
}

/// Keys under `b/` are named by their content.
struct Blocks;

impl KeyClassifier for Blocks {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else {
            KeyClass::Ordinary
        }
    }
}

#[test]
fn content_addressed_keys_compare_plaintext_bytes() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = txn_db(dir.path(), &keys).with_policy(Arc::new(Blocks));
    let defra = || db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));

    let tx = defra();
    tx.put(b"b/shared", MARKER).unwrap();
    regolith::through_stalls(|| db.db().put(b"b/shared", MARKER)).unwrap();
    db.db().flush().unwrap();
    tx.commit()
        .expect("the same bytes under a content-addressed key");

    let tx = defra();
    tx.put(b"b/shared", b"other bytes").unwrap();
    regolith::through_stalls(|| db.db().put(b"b/shared", MARKER)).unwrap();
    db.db().flush().unwrap();
    assert!(matches!(
        tx.commit(),
        Err(TransactionError::Engine(Error::ContentMismatch))
    ));
}

/// Entries at `log/<20 digits>`, head at `log-head`.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"log-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("log/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        4 + 20
    }
}

#[test]
fn appends_and_allocations_continue_across_flushes_and_reopens() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let log: Arc<dyn LogLayout> = Arc::new(Journal);
    let mut allocated = Vec::new();
    for round in 0..3 {
        let db = txn_db(dir.path(), &keys);
        for i in 0..5 {
            let tx = db.begin(&TxnOptions::new());
            tx.append(&log, format!("entry {round}.{i}").as_bytes(), None)
                .unwrap();
            tx.commit().unwrap();
            allocated.push(db.db().allocate(b"ids", 2).unwrap());
        }
        db.db().flush().unwrap();
    }
    let db = txn_db(dir.path(), &keys);
    for position in 1..=15u64 {
        let entry = db
            .db()
            .get(format!("log/{position:020}").as_bytes())
            .unwrap()
            .unwrap_or_else(|| panic!("entry {position} is missing"));
        let (round, i) = ((position - 1) / 5, (position - 1) % 5);
        assert_eq!(entry, format!("entry {round}.{i}").into_bytes());
    }
    let expected: Vec<_> = (0..15u64).map(|n| 1 + 2 * n..3 + 2 * n).collect();
    assert_eq!(allocated, expected);
    assert!(files_holding(dir.path(), b"entry 1.").is_empty());
}

#[test]
fn an_encrypted_database_opened_without_a_provider_refuses() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        fill(&db, 0, 10);
    }
    let plain = Options::default().merge_operator(Some(Arc::new(CounterMerge)));
    for result in [
        Db::open(dir.path(), plain.clone()).map(|_| ()),
        Db::open_read_only(dir.path(), plain).map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(Error::KeyProviderRequired)),
            "{result:?}"
        );
    }
    let read_only = Db::open_read_only(dir.path(), options(&keys)).unwrap();
    check(&read_only, 0, 10);
    drop(read_only);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    check(&db, 0, 10);
}

#[test]
fn a_missing_key_and_an_unprovided_current_key_refuse_to_open() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), options(&Keys::new(&[1]))).unwrap();
        fill(&db, 0, 400);
        db.flush().unwrap();
        fill(&db, 400, 410);
    }
    // The provider has key 2 only: every file names key 1.
    let result = Db::open(dir.path(), options(&Keys::new(&[2])));
    assert!(
        matches!(result, Err(Error::UnknownKey { id: KeyId(1) })),
        "{result:?}"
    );
    // The provider names a current key it does not provide.
    let keys = Keys::new(&[1]);
    keys.set_current(9);
    let result = Db::open(dir.path(), options(&keys));
    assert!(
        matches!(result, Err(Error::UnknownKey { id: KeyId(9) })),
        "{result:?}"
    );
}

/// Every file under `dir` with its bytes, to tell whether anything wrote.
fn contents_under(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    files_under(dir)
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

/// Whether `path` is a database directory's lock file.
fn is_lock_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "LOCK")
}

/// The open checks the provider's current key before it writes anything.
/// This directory makes an open write twice before it would first need the
/// current key (to seal the next log): the manifest's torn tail is trimmed
/// and the newest log's dropped tail truncated. A provider that cannot
/// provide its current key must refuse with every byte left as it was, and
/// the same directory must then open, dropping that tail, under a provider
/// that can.
#[test]
fn a_refused_open_writes_nothing_first_not_even_a_dropped_log_tail() {
    let live = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    {
        let opts = options(&keys)
            .write_buffer_size(64 << 20)
            .durability(DurabilityMode::Immediate);
        let db = Db::open(live.path(), opts).unwrap();
        fill(&db, 0, 20);
        // Copied while open, so the copy is what a power cut leaves right
        // after the last acknowledged write: no clean close.
        for path in files_under(live.path()) {
            let to = dir.path().join(path.strip_prefix(live.path()).unwrap());
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            // The lock file is no byte of the database, and Windows lets no
            // other handle read it while the database holds it locked.
            if is_lock_file(&path) {
                continue;
            }
            std::fs::copy(&path, &to).unwrap();
        }
    }
    let append = |path: &Path, bytes: &[u8]| {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
    };
    let log = files_under(&dir.path().join("wal"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "log"))
        .unwrap();
    // Torn tails: a record header the power cut left half written, and a
    // manifest batch whose length says more than the file holds.
    append(&log, &[0xA5; 11]);
    append(
        &dir.path().join("MANIFEST"),
        &[0xFF, 0x00, 0x00, 0x00, 0x01],
    );
    let data_under = |dir: &Path| {
        let mut contents = contents_under(dir);
        // Taking the lock is where an open starts, before it reads a byte.
        contents.retain(|path, _| !is_lock_file(path));
        contents
    };
    let before = data_under(dir.path());

    let unprovided = Keys::new(&[1]);
    unprovided.set_current(9);
    let result = Db::open(dir.path(), options(&unprovided));
    assert!(
        matches!(result, Err(Error::UnknownKey { id: KeyId(9) })),
        "{result:?}"
    );
    assert!(
        data_under(dir.path()) == before,
        "the refused open wrote before it refused"
    );

    let reports = common::wal_format::TailReports::new();
    let db = Db::open(
        dir.path(),
        options(&keys).listeners(vec![reports.clone() as Arc<dyn EventListener>]),
    )
    .unwrap();
    check(&db, 0, 20);
    let tails = reports.taken();
    assert_eq!(tails.len(), 1, "the newest log's tail was not dropped");
    assert_eq!(tails[0].discarded_bytes, 11);
}

#[test]
fn a_wrong_key_under_the_right_id_refuses_to_open() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), options(&Keys::new(&[1]))).unwrap();
        fill(&db, 0, 400);
        db.flush().unwrap();
        fill(&db, 400, 410);
    }
    let Err(err) = Db::open(dir.path(), options(&Keys::wrong(&[1]))) else {
        panic!("a wrong key opened the database");
    };
    assert!(matches!(err, Error::Corruption(_)), "{err:?}");
    assert!(
        err.to_string().contains("does not verify under key id 1"),
        "{err}"
    );
}

#[test]
fn a_damaged_table_block_reads_as_corruption() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        fill(&db, 0, 400);
        db.flush().unwrap();
        db.close().unwrap();
    }
    let table = files_under(&dir.path().join("sst"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "sst"))
        .unwrap();
    let mut bytes = std::fs::read(&table).unwrap();
    bytes[40] ^= 0x01;
    std::fs::write(&table, &bytes).unwrap();

    // The open reads the table's first block, as it does a block whose
    // checksum fails in an unencrypted table, so the damage refuses there;
    // were it read later, the read would fail the same way.
    let err = match Db::open(dir.path(), options(&keys)) {
        Err(err) => err,
        Ok(db) => (0..400)
            .find_map(|i| db.get(&key(i)).err())
            .expect("a damaged block was read back"),
    };
    assert!(matches!(err, Error::Corruption(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("data block at offset 0 does not verify"),
        "{err}"
    );
}

#[test]
fn a_damaged_record_in_a_cleanly_closed_log_refuses_naming_the_log() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    {
        let opts = options(&keys)
            .write_buffer_size(64 << 20)
            .durability(DurabilityMode::Immediate);
        let db = Db::open(dir.path(), opts).unwrap();
        fill(&db, 0, 20);
        db.close().unwrap();
    }
    let log = files_under(&dir.path().join("wal"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "log"))
        .unwrap();
    let mut bytes = std::fs::read(&log).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&log, &bytes).unwrap();
    let err = Db::open(dir.path(), options(&keys)).err().unwrap();
    let name = log.file_name().unwrap().to_string_lossy().into_owned();
    assert!(err.to_string().contains(&name), "{err}");
}

#[test]
fn a_rotated_key_reads_old_data_and_a_full_compaction_retires_it() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1, 2]);
    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        fill(&db, 0, 300);
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        fill(&db, 300, 350);
        db.flush().unwrap();
        keys.set_current(2);
        fill(&db, 350, 600);
        db.flush().unwrap();
        fill(&db, 600, 610);
        check(&db, 0, 610);
    }
    let both = Db::open(dir.path(), options(&keys)).unwrap();
    check(&both, 0, 610);
    drop(both);
    let only_two = Keys::new(&[2]);
    assert!(matches!(
        Db::open(dir.path(), options(&only_two)),
        Err(Error::UnknownKey { id: KeyId(1) })
    ));

    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), options(&only_two)).unwrap();
    check(&db, 0, 610);
}

#[test]
fn an_unencrypted_database_opens_with_a_provider_and_a_full_compaction_seals_it() {
    let dir = TempDir::new().unwrap();
    let plain = Options::default()
        .write_buffer_size(16 * 1024)
        .block_size(1024)
        .merge_operator(Some(Arc::new(CounterMerge)));
    {
        let db = Db::open(dir.path(), plain.clone()).unwrap();
        fill(&db, 0, 400);
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        fill(&db, 400, 420);
    }
    assert!(!files_holding(dir.path(), MARKER).is_empty());

    let keys = Keys::new(&[1]);
    {
        let db = Db::open(dir.path(), options(&keys)).unwrap();
        check(&db, 0, 420);
        fill(&db, 420, 500);
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    assert!(
        files_holding(dir.path(), MARKER).is_empty(),
        "still plaintext after a full compaction: {:?}",
        files_holding(dir.path(), MARKER)
    );
    assert!(matches!(
        Db::open(dir.path(), plain),
        Err(Error::KeyProviderRequired)
    ));
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    check(&db, 0, 500);
}

#[test]
fn external_tables_ingest_into_an_encrypted_database_sealed_or_not() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    let staging = TempDir::new().unwrap();
    let sealed_path = staging.path().join("sealed.sst");
    let plain_path = staging.path().join("plain.sst");
    for (path, opts, base) in [
        (&sealed_path, options(&keys), 0),
        (&plain_path, Options::default(), 1000),
    ] {
        let mut writer = SstFileWriter::create(path, &opts).unwrap();
        for i in base..base + 50 {
            writer.put(&key(i), &value(i)).unwrap();
        }
        writer.finish().unwrap();
    }
    assert!(files_holding(staging.path(), MARKER) == vec![plain_path.clone()]);
    db.ingest_external_files(&[sealed_path, plain_path], IngestOptions::default())
        .wait()
        .unwrap();
    check(&db, 0, 50);
    check(&db, 1000, 1050);
    db.close().unwrap();
    assert!(files_holding(dir.path(), MARKER).is_empty());
}

/// Write an external table of `keys` under `opts`, each holding `value(i)`
/// tagged with `tag`.
fn external(path: &Path, opts: &Options, keys: std::ops::Range<usize>, tag: &[u8]) {
    let mut writer = SstFileWriter::create(path, opts).unwrap();
    for i in keys {
        let mut v = value(i);
        v.extend_from_slice(tag);
        writer.put(&key(i), &v).unwrap();
    }
    writer.finish().unwrap();
}

/// A sealed external table is installed as it is, its blocks read at the
/// sequence its sealed manifest record carries (tag 7), before and after a
/// reopen, and a snapshot taken before the ingest never sees it.
#[test]
fn an_ingested_sealed_table_is_installed_as_is_and_reads_at_its_sequence() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    fill(&db, 0, 20);
    let before = db.snapshot();

    let staging = TempDir::new().unwrap();
    let source = staging.path().join("ingest.sst");
    external(&source, &options(&keys), 10..30, b"/ingested");
    let source_bytes = std::fs::read(&source).unwrap();
    let ingest = IngestOptions {
        snapshot_consistency: false,
        ..IngestOptions::default()
    };
    db.ingest_external_files(std::slice::from_ref(&source), ingest)
        .wait()
        .unwrap();

    let ingested = |i: usize| {
        let mut v = value(i);
        v.extend_from_slice(b"/ingested");
        Some(v)
    };
    for i in 10..30 {
        assert_eq!(db.get(&key(i)).unwrap(), ingested(i), "key {i}");
    }
    for i in 10..20 {
        assert_eq!(
            before.get(&key(i)).unwrap(),
            Some(value(i)),
            "snapshot, key {i}"
        );
    }
    assert_eq!(before.get(&key(25)).unwrap(), None);
    drop(before);

    let installed = files_under(&dir.path().join("sst"))
        .into_iter()
        .filter(|p| std::fs::read(p).unwrap() == source_bytes)
        .count();
    assert_eq!(installed, 1, "the sealed source was not installed as it is");
    db.close().unwrap();
    drop(db);
    assert!(files_holding(dir.path(), b"user-key-").is_empty());

    let db = Db::open(dir.path(), options(&keys)).unwrap();
    check(&db, 0, 10);
    for i in 10..30 {
        assert_eq!(db.get(&key(i)).unwrap(), ingested(i), "reopened, key {i}");
    }
}

/// A backup of an encrypted database holding an ingested table (manifest
/// version 3 carries its sequence) restores into a database that opens
/// only under the keys, reads the table at its sequence, and holds no
/// plaintext value.
#[test]
fn a_backup_holding_an_ingested_table_restores_under_the_keys() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    fill(&db, 0, 300);
    db.flush().unwrap();
    let staging = TempDir::new().unwrap();
    let source = staging.path().join("ingest.sst");
    external(&source, &Options::default(), 100..140, b"/ingested");
    db.ingest_external_files(&[source], IngestOptions::default())
        .wait()
        .unwrap();

    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    drop(db);
    let restored = TempDir::new().unwrap();
    engine
        .restore(id, restored.path(), Some(keys.clone()))
        .unwrap();
    assert!(matches!(
        Db::open(restored.path(), Options::default()),
        Err(Error::KeyProviderRequired)
    ));
    let db = Db::open(restored.path(), options(&keys)).unwrap();
    check(&db, 0, 100);
    for i in 100..140 {
        let mut v = value(i);
        v.extend_from_slice(b"/ingested");
        assert_eq!(db.get(&key(i)).unwrap(), Some(v), "key {i}");
    }
    check(&db, 140, 300);
    db.close().unwrap();
    assert!(files_holding(restored.path(), MARKER).is_empty());
}

#[test]
fn checkpoints_and_backups_of_an_encrypted_database_open_under_its_keys() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = Db::open(dir.path(), options(&keys)).unwrap();
    fill(&db, 0, 400);
    db.flush().unwrap();
    fill(&db, 400, 420);

    let checkpoint = TempDir::new().unwrap();
    let checkpoint_dir = checkpoint.path().join("cp");
    Checkpoint::new(&db)
        .unwrap()
        .create(&checkpoint_dir)
        .wait()
        .unwrap();
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    drop(db);

    let restored = TempDir::new().unwrap();
    engine
        .restore(id, restored.path(), Some(keys.clone()))
        .unwrap();
    for at in [checkpoint_dir.as_path(), restored.path()] {
        assert!(matches!(
            Db::open(
                at,
                Options::default().merge_operator(Some(Arc::new(CounterMerge)))
            ),
            Err(Error::KeyProviderRequired)
        ));
        let copy = Db::open(at, options(&keys)).unwrap();
        check(&copy, 0, 420);
        copy.close().unwrap();
        assert!(files_holding(at, MARKER).is_empty(), "{}", at.display());
    }
}
