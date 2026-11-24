//! Independent adversarial review of the SSTable metadata checksums.
//!
//! Is every bit flip in the footer, the index block and the bloom region
//! either caught or harmless? The oracle is not "an error was returned":
//! it is that `get` and a full scan agree with each other and with the
//! truth. A flip that makes the two surfaces disagree is silently wrong
//! data, which is the defect this file names.
//!
//! The footer layout is parsed here from the on-disk format rather than
//! imported, so the sweep cannot inherit a wrong region boundary from the
//! code it is checking:
//!
//! ```text
//! [rt_off u64][rt_size u64][bloom_off u64][bloom_size u64]
//! [index_off u64][index_size u64][num_entries u64]
//! [footer checksum u64][magic u64]
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

const KEYS: u32 = 600;

fn opts() -> Options {
    Options::default()
        .write_buffer_size(64 * 1024 * 1024)
        .durability(DurabilityMode::Eventual)
}

fn key(i: u32) -> Vec<u8> {
    format!("key_{i:06}").into_bytes()
}

fn val(i: u32) -> Vec<u8> {
    format!("value_{i:06}").into_bytes()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create_dir_all");
    for entry in fs::read_dir(from).expect("read_dir").flatten() {
        let name = entry.file_name();
        if name == "LOCK" {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// A closed database holding `KEYS` keys in exactly one SSTable.
fn one_table_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    for i in 0..KEYS {
        db.put(&key(i), &val(i)).expect("put");
    }
    db.compact_range(None, None).wait().expect("compact");
    db.close().expect("close");
    drop(db);
    let mut ssts: Vec<PathBuf> = fs::read_dir(dir.path().join("sst"))
        .expect("sst dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sst"))
        .collect();
    ssts.sort();
    assert_eq!(ssts.len(), 1, "the fixture must hold one table: {ssts:?}");
    (dir, ssts.pop().expect("one table"))
}

/// The regions of an SSTable, parsed from its own footer.
struct Regions {
    footer: (usize, usize),
    index: (usize, usize),
    bloom: (usize, usize),
}

fn regions(bytes: &[u8]) -> Regions {
    let n = bytes.len();
    let magic = u64::from_le_bytes(bytes[n - 8..].try_into().expect("8"));
    // v5 and v6 are the REGOSST flat and partitioned footers. They share
    // a 72-byte layout, so the field offsets below hold for both.
    let footer_size = match magic & 0xff {
        0x01 | 0x02 => 64usize,
        0x03..=0x06 => 72,
        v => panic!("unknown format version {v:#04x} (magic {magic:#018x})"),
    };
    let f = n - footer_size;
    let u64_at = |i: usize| u64::from_le_bytes(bytes[f + i..f + i + 8].try_into().expect("8"));
    Regions {
        footer: (f, footer_size),
        bloom: (u64_at(16) as usize, u64_at(24) as usize),
        index: (u64_at(32) as usize, u64_at(40) as usize),
    }
}

/// What a database served after a flip.
#[derive(PartialEq, Eq, Debug)]
enum Served {
    /// The damage was reported: open, a point read or a scan errored.
    Caught,
    /// Every surface agreed with the truth.
    Harmless,
    /// `get` and the scan disagreed, or one of them served wrong data.
    Wrong(String),
}

/// Open the planted directory and cross-check `get` against a full scan.
fn probe(db_dir: &Path) -> Served {
    let db = match Db::open(db_dir, opts()) {
        Ok(db) => db,
        Err(_) => return Served::Caught,
    };
    let scanned: BTreeMap<Vec<u8>, Vec<u8>> = match db.scan(None, None) {
        Ok(pairs) => pairs.into_iter().collect(),
        Err(_) => return Served::Caught,
    };
    for i in 0..KEYS {
        let k = key(i);
        let got = match db.get(&k) {
            Ok(v) => v,
            Err(_) => return Served::Caught,
        };
        let from_scan = scanned.get(&k).cloned();
        if got != from_scan {
            return Served::Wrong(format!(
                "get and scan disagree on {}: get={:?} scan={:?}",
                String::from_utf8_lossy(&k),
                got.as_deref().map(String::from_utf8_lossy),
                from_scan.as_deref().map(String::from_utf8_lossy),
            ));
        }
        match got {
            Some(v) if v == val(i) => {}
            Some(v) => {
                return Served::Wrong(format!(
                    "{} served {:?}, truth is {:?}",
                    String::from_utf8_lossy(&k),
                    String::from_utf8_lossy(&v),
                    String::from_utf8_lossy(&val(i)),
                ));
            }
            None => {
                return Served::Wrong(format!(
                    "{} vanished but no error was reported",
                    String::from_utf8_lossy(&k),
                ));
            }
        }
    }
    drop(db);
    Served::Harmless
}

/// Flip every bit of `region`, up to `cap` evenly spread positions, and
/// classify each outcome.
fn sweep(label: &str, region: (usize, usize), cap: usize) {
    let (src, table) = one_table_db();
    let original = fs::read(&table).expect("read table");
    let rel = table
        .strip_prefix(src.path())
        .expect("under root")
        .to_path_buf();

    let (start, len) = region;
    assert!(len > 0, "[{label}] the region must be non-empty");
    assert!(
        start + len <= original.len(),
        "[{label}] region {start}+{len} runs past the {} byte file",
        original.len(),
    );

    let step = (len / cap).max(1);
    let work = TempDir::new().expect("tempdir");
    let db_dir = work.path().join("db");

    let (mut caught, mut harmless, mut trials) = (0usize, 0usize, 0usize);
    let mut wrong = Vec::new();
    for off in (start..start + len).step_by(step) {
        for bit in 0..8u8 {
            let mut damaged = original.clone();
            damaged[off] ^= 1 << bit;
            let _ = fs::remove_dir_all(&db_dir);
            copy_tree(src.path(), &db_dir);
            fs::write(db_dir.join(&rel), &damaged).expect("plant");
            trials += 1;
            match probe(&db_dir) {
                Served::Caught => caught += 1,
                Served::Harmless => harmless += 1,
                Served::Wrong(why) => wrong.push(format!("byte {off} bit {bit}: {why}")),
            }
        }
    }
    println!(
        "[{label}] {trials} flips over {len} bytes: {caught} caught, {harmless} harmless, \
         {} silently wrong",
        wrong.len(),
    );
    assert!(
        wrong.is_empty(),
        "[{label}] {} flip(s) served silently wrong data:\n  {}",
        wrong.len(),
        wrong
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}

#[test]
fn every_probed_bit_flip_in_the_footer_is_caught_or_harmless() {
    let (src, table) = one_table_db();
    let bytes = fs::read(&table).expect("read");
    let r = regions(&bytes);
    drop(src);
    sweep("footer", r.footer, 72);
}

#[test]
fn every_probed_bit_flip_in_the_index_block_is_caught_or_harmless() {
    let (src, table) = one_table_db();
    let bytes = fs::read(&table).expect("read");
    let r = regions(&bytes);
    drop(src);
    sweep("index", r.index, 120);
}

#[test]
fn every_probed_bit_flip_in_the_bloom_region_is_caught_or_harmless() {
    let (src, table) = one_table_db();
    let bytes = fs::read(&table).expect("read");
    let r = regions(&bytes);
    drop(src);
    sweep("bloom", r.bloom, 120);
}

/// The whole metadata tail in one sweep, so a region the three probes
/// above do not name (the range-tombstone block, any padding between
/// regions) is covered too.
#[test]
fn every_probed_bit_flip_in_the_whole_metadata_tail_is_caught_or_harmless() {
    let (src, table) = one_table_db();
    let bytes = fs::read(&table).expect("read");
    let r = regions(&bytes);
    let tail_start = r.bloom.0.min(r.index.0);
    drop(src);
    sweep("metadata tail", (tail_start, bytes.len() - tail_start), 200);
}
