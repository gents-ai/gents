//! Round-trips of the new checksummed SSTable format through every path
//! that moves table files around: backup and restore, checkpoint,
//! external-SST ingestion, and a partitioned-index database that is
//! compacted and reopened.
//!
//! A format change that only survives inside the writing process is not
//! a format change; these are the paths where a table is written by one
//! handle and read by another.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::Path;

use regolith::{BackupEngine, Db, IngestOptions, Options, SstFileWriter};
use tempfile::TempDir;

const N: usize = 4000;

fn key(i: usize) -> Vec<u8> {
    format!("k_{i:07}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    format!("v_{i:07}_{}", "z".repeat(i % 41)).into_bytes()
}

fn opts(partitioned: bool) -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .partitioned_index(partitioned)
        .metadata_block_size(if partitioned { 256 } else { 4096 })
}

fn fill(db: &Db) {
    for i in 0..N {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).expect("put");
    }
    regolith::through_stalls(|| db.delete_range(b"k_0000100", b"k_0000150")).expect("range");
}

fn check(db: &Db, what: &str) {
    let mut wrong = Vec::new();
    for i in 0..N {
        let deleted = (100..150).contains(&i);
        let want = if deleted { None } else { Some(value(i)) };
        if db.get(&key(i)).expect("get") != want {
            wrong.push(i);
        }
    }
    assert!(
        wrong.is_empty(),
        "{what}: {} keys wrong, first {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)],
    );
    assert_eq!(
        db.scan(None, None).expect("scan").len(),
        N - 50,
        "{what}: scan count"
    );
}

fn magics(sst_dir: &Path) -> Vec<u64> {
    let mut out: Vec<u64> = fs::read_dir(sst_dir)
        .expect("read_dir")
        .flatten()
        .map(|e| {
            let b = fs::read(e.path()).expect("read");
            u64::from_le_bytes(b[b.len() - 8..].try_into().expect("8 bytes"))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn a_checksummed_database_survives_backup_and_restore() {
    for partitioned in [false, true] {
        let src = TempDir::new().expect("tempdir");
        let db = Db::open(src.path(), opts(partitioned)).expect("open");
        fill(&db);
        db.compact_range(None, None).wait().expect("compact");

        let backup_dir = TempDir::new().expect("tempdir");
        let mut engine = BackupEngine::open(backup_dir.path()).expect("backup engine");
        let id = engine.create_backup(&db).expect("create_backup");
        db.close().expect("close");
        drop(db);

        let target = TempDir::new().expect("tempdir");
        let restored_root = target.path().join("restored");
        engine.restore(id, &restored_root, None).expect("restore");
        let restored = Db::open(&restored_root, opts(partitioned)).expect("open restored");
        check(&restored, &format!("restored (partitioned={partitioned})"));
        println!(
            "backup/restore partitioned={partitioned}: magics {:x?}",
            magics(&restored_root.join("sst")),
        );
    }
}

#[test]
fn a_checksummed_database_survives_a_checkpoint() {
    for partitioned in [false, true] {
        let src = TempDir::new().expect("tempdir");
        let db = Db::open(src.path(), opts(partitioned)).expect("open");
        fill(&db);
        db.compact_range(None, None).wait().expect("compact");

        let holder = TempDir::new().expect("tempdir");
        let cp = holder.path().join("cp");
        db.checkpoint(&cp).wait().expect("checkpoint");
        db.close().expect("close");
        drop(db);

        let reopened = Db::open(&cp, opts(partitioned)).expect("open checkpoint");
        check(
            &reopened,
            &format!("checkpoint (partitioned={partitioned})"),
        );
        println!(
            "checkpoint partitioned={partitioned}: magics {:x?}",
            magics(&cp.join("sst")),
        );
    }
}

#[test]
fn a_checksummed_external_table_ingests_and_reads_back() {
    for partitioned in [false, true] {
        let hold = TempDir::new().expect("tempdir");
        let path = hold.path().join("ext.sst");
        let mut w = SstFileWriter::create(&path, &opts(partitioned)).expect("create");
        for i in 0..N {
            w.put(&key(i), &value(i)).expect("sst put");
        }
        let meta = w.finish().expect("finish");
        assert_eq!(meta.num_entries as usize, N);
        let bytes = fs::read(&path).expect("read");
        let magic = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().expect("8"));
        assert!(
            magic == 0x5245474F_53535405 || magic == 0x5245474F_53535406,
            "the writer must emit a checksummed REGOSST format, got {magic:#018x}",
        );
        assert_eq!(
            &magic.to_be_bytes()[..7],
            b"REGOSST",
            "an externally written table must carry the current identifier"
        );

        let dir = TempDir::new().expect("tempdir");
        let db = Db::open(dir.path(), opts(partitioned)).expect("open");
        db.ingest_external_files(&[path], IngestOptions::default())
            .wait()
            .expect("ingest");
        for i in 0..N {
            assert_eq!(
                db.get(&key(i)).expect("get"),
                Some(value(i)),
                "ingest key {i}"
            );
        }
        println!("ingest partitioned={partitioned}: magic {magic:#018x}, {N} keys");
    }
}

/// A partitioned-index database compacted across levels: the per-leaf
/// checksum has to survive a compaction rewriting the leaves.
#[test]
fn a_partitioned_database_survives_compaction_and_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts(true)).expect("open");
    fill(&db);
    for round in 0..3 {
        db.compact_range(None, None).wait().expect("compact");
        check(&db, &format!("partitioned after compaction round {round}"));
    }
    db.close().expect("close");
    drop(db);
    let reopened = Db::open(dir.path(), opts(true)).expect("reopen");
    check(&reopened, "partitioned after reopen");
    println!(
        "partitioned compaction: magics {:x?}",
        magics(&dir.path().join("sst")),
    );
}
