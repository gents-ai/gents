//! Adversarial byte-substitution sweep over a real WAL (the torn-tail rule).
//!
//! The shipped tests flip single bits and cut at every offset. This sets
//! every offset to `0x00` and to `0xFF`, which reaches the length field
//! in ways a single flip cannot, and asserts the whole contract at once:
//! an open either refuses, or serves a state that is a prefix of the
//! write history. Nothing may be invented and nothing may go missing out
//! of the middle.

use std::fs;
use std::path::{Path, PathBuf};

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

const KEYS: usize = 24;

fn opts() -> Options {
    Options::default()
        .write_buffer_size(8 * 1024 * 1024)
        .durability(DurabilityMode::Immediate)
}

fn wal_of(db: &Path) -> PathBuf {
    let mut wals: Vec<PathBuf> = fs::read_dir(db.join("wal"))
        .expect("wal dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    wals.sort();
    assert_eq!(
        wals.len(),
        1,
        "the fixture must hold exactly one WAL: {wals:?}"
    );
    wals.pop().expect("one wal")
}

/// One file of a planted database directory: its path relative to the
/// database root, and its bytes.
type PlantedFile = (PathBuf, Vec<u8>);

/// A pristine database directory whose WAL holds `KEYS` acknowledged
/// writes and whose MANIFEST references no SSTable.
fn fixture() -> Vec<PlantedFile> {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("db");
    {
        let d = Db::open(&db, opts()).expect("open");
        for i in 0..KEYS {
            d.put(format!("k{i:04}").as_bytes(), format!("v{i:04}").as_bytes())
                .expect("put");
        }
        // No close: closing flushes the memtable and empties the WAL.
        std::mem::forget(d);
    }
    let mut files = Vec::new();
    collect(&db, &db, &mut files);
    files
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PlantedFile>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "LOCK") {
            continue;
        }
        if p.is_dir() {
            collect(root, &p, out);
        } else {
            out.push((
                p.strip_prefix(root).expect("under root").to_path_buf(),
                fs::read(&p).expect("read"),
            ));
        }
    }
}

fn plant(files: &[PlantedFile], db: &Path) {
    let _ = fs::remove_dir_all(db);
    for (rel, bytes) in files {
        let p = db.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(&p, bytes).expect("write");
    }
}

/// A key-value pair as the scan surface hands it back.
type Pair = (Vec<u8>, Vec<u8>);

/// Every `(key, value)` the opened database serves.
fn read_all(db: &Path) -> Result<Vec<Pair>, String> {
    match Db::open(db, opts()) {
        Err(e) => Err(e.to_string()),
        Ok(d) => {
            let pairs = d.scan(None, None).expect("scan");
            drop(d);
            Ok(pairs)
        }
    }
}

/// Which prefixes of the write history the served state equals. Every
/// key is distinct and written once, so prefix `k` is exactly keys
/// `0..k`.
fn matching_prefix(state: &[Pair]) -> Option<usize> {
    let k = state.len();
    if k > KEYS {
        return None;
    }
    for (i, (key, value)) in state.iter().enumerate() {
        if key != format!("k{i:04}").as_bytes() || value != format!("v{i:04}").as_bytes() {
            return None;
        }
    }
    Some(k)
}

#[test]
fn every_single_byte_substitution_in_the_wal_refuses_or_serves_a_prefix() {
    let files = fixture();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    plant(&files, &db);
    let pristine = read_all(&db).expect("the pristine fixture must open");
    assert_eq!(
        matching_prefix(&pristine),
        Some(KEYS),
        "the pristine fixture must serve the whole history"
    );

    let wal_rel = {
        plant(&files, &db);
        let w = wal_of(&db);
        w.strip_prefix(&db).expect("under db").to_path_buf()
    };
    let original = files
        .iter()
        .find(|(rel, _)| *rel == wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");

    let mut refused = 0usize;
    let mut prefixes = std::collections::BTreeMap::<usize, usize>::new();
    let mut bad = Vec::new();

    for offset in 0..original.len() {
        for fill in [0x00u8, 0xFFu8] {
            if original[offset] == fill {
                continue;
            }
            let mut bytes = original.clone();
            bytes[offset] = fill;
            plant(&files, &db);
            fs::write(db.join(&wal_rel), &bytes).expect("write wal");
            match read_all(&db) {
                Err(_) => refused += 1,
                Ok(state) => match matching_prefix(&state) {
                    Some(k) => *prefixes.entry(k).or_default() += 1,
                    None => bad.push(format!(
                        "offset {offset} set to {fill:#04x}: opened on a state matching no \
                         prefix of the write history ({} entries: {:?})",
                        state.len(),
                        state
                            .iter()
                            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                            .collect::<Vec<_>>(),
                    )),
                },
            }
        }
    }

    println!(
        "wal byte-substitution sweep: {} bytes, {refused} refused, prefixes served {prefixes:?}, \
         {} violations",
        original.len(),
        bad.len(),
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}
