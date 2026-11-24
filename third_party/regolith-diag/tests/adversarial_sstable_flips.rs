//! Independent bit-rot sweeps over an SSTable (the metadata checksum fix).
//!
//! `tests/corruption_exhaustive.rs` sweeps a flat-index table with no
//! range tombstones, so two of the four checksummed metadata regions -
//! the partitioned index leaves and the range-tombstone block - are
//! never reached by it. This file builds a table that has both and
//! sweeps that, so "every metadata region is covered" is measured rather
//! than argued.
//!
//! A trial is a violation only when the engine serves data that
//! disagrees with what was written: a wrong value, an invented or
//! missing key, or a forward scan, a reverse scan and a point lookup
//! that disagree with each other. A refusal is always acceptable; so is
//! serving the correct data, which happens when the flip lands somewhere
//! that cannot change an answer.
//!
//! Every region a regolith table carries is checksummed, so a flip in
//! the metadata tail must be refused or read back correctly, never
//! served as data.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regolith::{Db, Options};
use tempfile::TempDir;

/// A pristine database directory plus what an uncorrupted open serves.
struct Fixture {
    files: Vec<(String, Vec<u8>)>,
    truth: BTreeMap<Vec<u8>, Vec<u8>>,
    table: String,
    /// The options the table was written with. A trial reopens the
    /// planted directory with them, so a fixture carries its own rather
    /// than sharing one across generators.
    opts: Options,
}

impl Fixture {
    fn opts(&self) -> Options {
        self.opts.clone()
    }
}

fn copy_tree_into(from: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(from).expect("read_dir").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "LOCK" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_tree_into(&path, &format!("{prefix}{name}/"), out);
        } else {
            out.push((format!("{prefix}{name}"), fs::read(&path).expect("read")));
        }
    }
}

/// Build a single-SSTable database whose table exercises every
/// checksummed metadata region: many data blocks, a bloom filter, a
/// partitioned or flat index, and a range-tombstone block.
fn build(partitioned: bool) -> Fixture {
    let root = TempDir::new().expect("tempdir");
    let db_dir = root.path().join("db");
    let opts = Options::default()
        .write_buffer_size(4096)
        .block_size(256)
        .partitioned_index(partitioned)
        .metadata_block_size(256);
    let db = Db::open(&db_dir, opts.clone()).expect("open");

    let mut truth = BTreeMap::new();
    for i in 0..400usize {
        let k = format!("k{i:05}").into_bytes();
        let v = format!("v{i:05}_{}", "p".repeat(i % 23)).into_bytes();
        regolith::through_stalls(|| db.put(&k, &v)).expect("put");
        truth.insert(k, v);
    }
    // A range delete, so the table carries a range-tombstone block.
    regolith::through_stalls(|| db.delete_range(b"k00100", b"k00140")).expect("delete_range");
    truth.retain(|k, _| {
        !(k.as_slice() >= b"k00100".as_slice() && k.as_slice() < b"k00140".as_slice())
    });
    for i in 400..430usize {
        let k = format!("k{i:05}").into_bytes();
        let v = format!("v{i:05}").into_bytes();
        regolith::through_stalls(|| db.put(&k, &v)).expect("put");
        truth.insert(k, v);
    }
    db.compact_range(None, None).wait().expect("compact_range");
    db.close().expect("close");
    drop(db);

    let mut files = Vec::new();
    copy_tree_into(&db_dir, "", &mut files);
    files.sort();
    let tables: Vec<String> = files
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| n.ends_with(".sst"))
        .collect();
    assert_eq!(
        tables.len(),
        1,
        "the sweep needs exactly one table to corrupt, got {tables:?}",
    );
    Fixture {
        files,
        truth,
        table: tables[0].clone(),
        opts,
    }
}

fn plant(fx: &Fixture, db: &Path) {
    let _ = fs::remove_dir_all(db);
    for (rel, bytes) in &fx.files {
        let p = db.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("create_dir_all");
        fs::write(&p, bytes).expect("write");
    }
}

/// What one trial observed.
enum Outcome {
    Refused,
    /// Every surface agreed with the truth.
    Correct,
    /// The engine served something that is not what was written.
    Wrong(String),
}

fn probe(fx: &Fixture, db_dir: &Path) -> Outcome {
    let db = match Db::open(db_dir, fx.opts()) {
        Err(_) => return Outcome::Refused,
        Ok(db) => db,
    };

    let mut forward = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        let (Some(k), Some(v)) = (it.key(), it.value()) else {
            break;
        };
        forward.push((k.to_vec(), v.to_vec()));
        it.next();
    }
    if it.status().is_err() {
        return Outcome::Refused;
    }

    let mut reverse = Vec::new();
    let mut it = db.iter();
    it.seek_to_last();
    while it.valid() {
        let (Some(k), Some(v)) = (it.key(), it.value()) else {
            break;
        };
        reverse.push((k.to_vec(), v.to_vec()));
        it.prev();
    }
    if it.status().is_err() {
        return Outcome::Refused;
    }
    reverse.reverse();

    if forward != reverse {
        return Outcome::Wrong(format!(
            "forward scan ({} entries) and reverse scan ({} entries) disagree",
            forward.len(),
            reverse.len()
        ));
    }

    let want: Vec<(Vec<u8>, Vec<u8>)> = fx
        .truth
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if forward != want {
        return Outcome::Wrong(format!(
            "scan served {} entries, {} were written",
            forward.len(),
            want.len()
        ));
    }

    for (k, v) in &want {
        match db.get(k) {
            Err(_) => return Outcome::Refused,
            Ok(Some(got)) if got == *v => {}
            Ok(other) => {
                return Outcome::Wrong(format!(
                    "scan says {} -> {:?}, get says {:?}",
                    String::from_utf8_lossy(k),
                    String::from_utf8_lossy(v),
                    other.map(|b| String::from_utf8_lossy(&b).into_owned()),
                ));
            }
        }
    }
    Outcome::Correct
}

struct Tally {
    refused: usize,
    correct: usize,
    /// Every trial that served something other than what was written.
    /// Counted apart from `violations`, which keeps only the first few
    /// messages: a capped length reported as the total would understate
    /// the damage.
    wrong: usize,
    violations: Vec<String>,
}

/// Flip every bit of `offsets` in the table and classify each trial.
fn sweep(fx: &Fixture, offsets: impl Iterator<Item = usize>) -> Tally {
    let root = TempDir::new().expect("tempdir");
    let db_dir = root.path().join("db");
    let table: PathBuf = PathBuf::from(&fx.table);
    let mut tally = Tally {
        refused: 0,
        correct: 0,
        wrong: 0,
        violations: Vec::new(),
    };
    for offset in offsets {
        for bit in 0..8u8 {
            plant(fx, &db_dir);
            let p = db_dir.join(&table);
            let mut bytes = fs::read(&p).expect("read");
            bytes[offset] ^= 1 << bit;
            fs::write(&p, &bytes).expect("write");
            match probe(fx, &db_dir) {
                Outcome::Refused => tally.refused += 1,
                Outcome::Correct => tally.correct += 1,
                Outcome::Wrong(why) => {
                    tally.wrong += 1;
                    if tally.violations.len() < 12 {
                        tally
                            .violations
                            .push(format!("byte {offset} bit {bit}: {why}"));
                    }
                }
            }
        }
    }
    tally
}

fn table_len(fx: &Fixture) -> usize {
    fx.files
        .iter()
        .find(|(n, _)| *n == fx.table)
        .map(|(_, b)| b.len())
        .expect("table bytes")
}

/// Every bit of the last 512 bytes of a partitioned table: the footer,
/// the top-level index and the index leaves nearest it. Exhaustive
/// rather than sampled, because this is the region the format change
/// added checksums to.
#[test]
fn every_flip_in_the_tail_of_a_partitioned_table_is_caught_or_harmless() {
    let fx = build(true);
    let len = table_len(&fx);
    let from = len.saturating_sub(512);
    let tally = sweep(&fx, from..len);
    println!(
        "partitioned tail sweep: {} trials -> {} refused, {} correct, {} violations",
        (len - from) * 8,
        tally.refused,
        tally.correct,
        tally.wrong,
    );
    assert!(
        tally.violations.is_empty(),
        "{}",
        tally.violations.join("\n  ")
    );
}

/// A strided sweep across the whole partitioned table, so the data
/// blocks, the bloom region and the range-tombstone block are all
/// visited too.
#[test]
fn a_strided_flip_across_a_whole_partitioned_table_is_caught_or_harmless() {
    let fx = build(true);
    let len = table_len(&fx);
    let stride = (len / 400).max(1);
    let offsets: Vec<usize> = (0..len).step_by(stride).collect();
    let tally = sweep(&fx, offsets.iter().copied());
    println!(
        "partitioned strided sweep: {} trials over {len} bytes -> {} refused, {} correct, {} \
         violations",
        offsets.len() * 8,
        tally.refused,
        tally.correct,
        tally.wrong,
    );
    assert!(
        tally.violations.is_empty(),
        "{}",
        tally.violations.join("\n  ")
    );
}

/// The same for a flat-index table that carries a range-tombstone block,
/// which the existing corpus fixture does not have.
#[test]
fn a_strided_flip_across_a_flat_table_with_range_tombstones_is_caught_or_harmless() {
    let fx = build(false);
    let len = table_len(&fx);
    let stride = (len / 400).max(1);
    let offsets: Vec<usize> = (0..len).step_by(stride).collect();
    let tally = sweep(&fx, offsets.iter().copied());
    println!(
        "flat strided sweep: {} trials over {len} bytes -> {} refused, {} correct, {} violations",
        offsets.len() * 8,
        tally.refused,
        tally.correct,
        tally.wrong,
    );
    assert!(
        tally.violations.is_empty(),
        "{}",
        tally.violations.join("\n  ")
    );
}

/// No bit flip in a table's metadata tail may be served as data.
///
/// Every region a regolith table carries is checksummed, so a flip in
/// the footer, index, bloom or range-tombstone block has to be refused
/// or read back correctly. Being served damaged metadata as if it were
/// data is the failure this exists to make impossible.
#[test]
fn no_flip_in_the_metadata_tail_is_ever_served_as_data() {
    let modern = build(false);
    let modern_len = table_len(&modern);
    let modern_from = modern_len.saturating_sub(512);
    let modern_trials = (modern_len - modern_from) * 8;
    let modern_tally = sweep(&modern, modern_from..modern_len);

    println!(
        "metadata tail sweep, last 512 bytes: {modern_trials} trials -> {} refused, {} correct, \
         {} served as data",
        modern_tally.refused, modern_tally.correct, modern_tally.wrong,
    );

    assert_eq!(
        modern_tally.refused + modern_tally.correct + modern_tally.wrong,
        modern_trials,
        "every trial must be classified",
    );
    assert!(modern_trials > 0, "the sweep ran no trials at all");
    assert!(
        modern_tally.violations.is_empty(),
        "a checksummed table served damaged metadata as data:\n  {}",
        modern_tally.violations.join("\n  "),
    );
    assert_eq!(
        modern_tally.wrong, 0,
        "the checksummed format must serve no damaged metadata as data",
    );
}
