//! Regression gate for a silent data-loss bug in
//! `Db::ingest_external_files`, found while reviewing the metadata
//! checksums and not one of
//! the six fixes under review.
//!
//! Every source file was pre-opened as `SsTableReader::open(path, 0)` and
//! then read through the engine's shared block cache. That cache is keyed
//! by `(file_id, block_offset)`, so all sources shared one namespace: the
//! second and every later source read the *first* source's cached blocks
//! back at the same offsets. The re-encoded table the engine wrote
//! therefore held the first file's entries, the later files' entries were
//! gone, and the call returned `Ok(())` with no error, no warning and no
//! ticker. It reproduced identically on the pre-change tree.
//!
//! Sources are now opened under distinct ids and read through a cache
//! private to the call, so neither aliasing can occur. The mechanism is
//! pinned two ways here: the loss disappeared when the block cache was
//! disabled, and it survived across separate `ingest` calls, which is
//! what a shared cache namespace predicts and a within-one-call ordering
//! bug would not. Both probes are kept as the gate.

use std::path::{Path, PathBuf};

use regolith::{Db, DurabilityMode, IngestOptions, Options, SstFileWriter};
use tempfile::TempDir;

const PER_FILE: u32 = 2000;

fn opts(block_cache_size: usize) -> Options {
    Options::default()
        .write_buffer_size(64 * 1024 * 1024)
        .durability(DurabilityMode::Eventual)
        .block_cache_size(block_cache_size)
}

fn make(path: &Path, prefix: &str) {
    let mut w = SstFileWriter::create(path, &Options::default()).expect("create");
    for i in 0..PER_FILE {
        w.put(
            format!("{prefix}{i:08}").as_bytes(),
            format!("val_{prefix}{i:08}").as_bytes(),
        )
        .expect("put");
    }
    w.finish().expect("finish");
}

/// Ingest `prefixes` and return the keys the database does not serve.
fn missing_after_ingest(block_cache_size: usize, prefixes: &[&str], one_call: bool) -> Vec<String> {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts(block_cache_size)).expect("open");

    let staged: Vec<PathBuf> = prefixes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let path = dir.path().join(format!("src{i}.sst"));
            make(&path, p);
            path
        })
        .collect();

    if one_call {
        db.ingest_external_files(&staged, IngestOptions::default())
            .wait()
            .expect("ingest reported success");
    } else {
        for path in staged {
            db.ingest_external_files(&[path], IngestOptions::default())
                .wait()
                .expect("ingest reported success");
        }
    }

    let mut missing = Vec::new();
    for prefix in prefixes {
        for i in 0..PER_FILE {
            let k = format!("{prefix}{i:08}");
            if db.get(k.as_bytes()).expect("get").is_none() {
                missing.push(k);
            }
        }
    }
    drop(db);
    missing
}

/// Every entry of every ingested file must be readable afterwards.
#[test]
fn ingesting_more_than_one_external_table_must_not_silently_drop_entries() {
    let prefixes = ["aaa_", "bbb_", "ccc_"];
    let total = PER_FILE as usize * prefixes.len();
    let missing = missing_after_ingest(512 * 1024 * 1024, &prefixes, true);
    assert!(
        missing.is_empty(),
        "ingest_external_files returned Ok(()) but {} of {total} entries are gone. \
         First missing: {:?}. Cause: the sources share a block-cache namespace, so \
         the cache serves the first source's blocks to the later ones.",
        missing.len(),
        &missing[..missing.len().min(5)],
    );
}

/// The same loss across separate calls, which rules out an ordering bug
/// inside one call and points at the shared cache namespace.
#[test]
fn ingesting_two_external_tables_in_separate_calls_must_not_drop_the_second() {
    let missing = missing_after_ingest(512 * 1024 * 1024, &["aaa_", "bbb_"], false);
    assert!(
        missing.is_empty(),
        "two separate ingest calls each returned Ok(()) but {} entries are gone, \
         first {:?}",
        missing.len(),
        &missing[..missing.len().min(5)],
    );
}

/// The control that named the mechanism: with the block cache disabled
/// the same workload lost nothing even before the fix, which is what made
/// the two above a cache-collision diagnosis rather than a guess.
#[test]
fn with_the_block_cache_disabled_the_same_ingest_loses_nothing() {
    let prefixes = ["aaa_", "bbb_", "ccc_"];
    let missing = missing_after_ingest(0, &prefixes, true);
    assert!(
        missing.is_empty(),
        "the control itself lost {} entries, so the diagnosis is wrong",
        missing.len(),
    );
    println!(
        "control: {} entries ingested from {} files with block_cache_size = 0, none lost",
        PER_FILE as usize * prefixes.len(),
        prefixes.len(),
    );
}
