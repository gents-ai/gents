//! Adversarial probe for the symptom an unchecksummed footer produces: `scan` and `get`
//! disagreeing after a corrupted SSTable is served.
//!
//! The shipped sweeps flip single bits and assert "refused or correct".
//! This does three things they do not:
//!
//! 1. Substitutes whole bytes (`0x00`, `0xFF`, and the neighbouring
//!    format-version values), which reaches states no single flip can.
//! 2. Cross-checks four read surfaces against each other on every
//!    surviving open, so a corruption that makes `get` and `scan`
//!    disagree is a violation even when neither errors.
//! 3. Rewrites the magic's version byte to every other known version,
//!    which is the one corruption that makes a reader parse the footer
//!    at the wrong length.

use std::fs;
use std::path::{Path, PathBuf};

use regolith::{Db, Options};
use tempfile::TempDir;

const KEYS: usize = 400;

fn key(i: usize) -> Vec<u8> {
    format!("k_{i:06}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    format!("v_{i:06}_{}", "y".repeat(i % 29)).into_bytes()
}

fn build_opts(partitioned: bool) -> Options {
    Options::default()
        .write_buffer_size(1 << 20)
        .partitioned_index(partitioned)
        .metadata_block_size(if partitioned { 128 } else { 4096 })
        .block_size(256)
}

/// One file of a planted database directory: its path relative to the
/// database root, and its bytes.
type PlantedFile = (PathBuf, Vec<u8>);

/// A key-value pair as every read surface hands it back.
type Pair = (Vec<u8>, Vec<u8>);

/// A closed database whose single SSTable holds every key plus a range
/// tombstone, returned as (relative path, bytes) pairs.
fn fixture(partitioned: bool) -> Vec<PlantedFile> {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("db");
    {
        let db = Db::open(&db_path, build_opts(partitioned)).expect("open");
        for i in 0..KEYS {
            db.put(&key(i), &value(i)).expect("put");
        }
        db.delete_range(b"k_000050", b"k_000060").expect("range");
        db.compact_range(None, None).wait().expect("compact");
        db.close().expect("close");
    }
    let mut out = Vec::new();
    collect(&db_path, &db_path, &mut out);
    out
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

fn sst_rel(files: &[PlantedFile]) -> PathBuf {
    let mut ssts: Vec<&PathBuf> = files
        .iter()
        .map(|(p, _)| p)
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sst"))
        .collect();
    assert_eq!(ssts.len(), 1, "the fixture must hold one SSTable: {ssts:?}");
    ssts.pop().expect("one").clone()
}

/// The truth the fixture encodes.
fn expected() -> Vec<Pair> {
    (0..KEYS)
        .filter(|i| !(50..60).contains(i))
        .map(|i| (key(i), value(i)))
        .collect()
}

/// One verdict for one corrupted directory.
enum Verdict {
    /// The open, or every read surface, failed loudly.
    Refused,
    /// Every surface agreed and matched the truth.
    Correct,
    /// Something disagreed or was silently wrong.
    Violation(String),
}

fn probe(db: &Path, label: &str) -> Verdict {
    let db = match Db::open(db, build_opts(false)) {
        Err(_) => return Verdict::Refused,
        Ok(d) => d,
    };

    let want = expected();

    // Forward scan.
    let forward = db.scan(None, None).ok();

    // Reverse walk.
    let mut it = db.iter();
    it.seek_to_last();
    let mut back = Vec::new();
    while it.valid() {
        back.push((
            it.key().expect("key").to_vec(),
            it.value().expect("value").to_vec(),
        ));
        it.prev();
    }
    let back = match it.status() {
        Err(_) => None,
        Ok(()) => {
            back.reverse();
            Some(back)
        }
    };
    drop(it);

    // Point reads and one batched read over every key, including the
    // range-deleted ones.
    let all_keys: Vec<Vec<u8>> = (0..KEYS).map(key).collect();
    let refs: Vec<&[u8]> = all_keys.iter().map(|k| k.as_slice()).collect();
    let mut points = Some(Vec::new());
    for k in &refs {
        match db.get(k) {
            Err(_) => {
                points = None;
                break;
            }
            Ok(v) => points.as_mut().expect("some").push(v),
        }
    }
    let batched = db.multi_get(&refs).ok();

    if forward.is_none() && back.is_none() && points.is_none() && batched.is_none() {
        return Verdict::Refused;
    }

    let point_pairs = |p: &Vec<Option<Vec<u8>>>| -> Vec<Pair> {
        p.iter()
            .enumerate()
            .filter_map(|(i, v)| v.clone().map(|v| (key(i), v)))
            .collect()
    };

    let mut views: Vec<(&str, Vec<Pair>)> = Vec::new();
    if let Some(f) = &forward {
        views.push(("scan", f.clone()));
    }
    if let Some(b) = &back {
        views.push(("reverse", b.clone()));
    }
    if let Some(p) = &points {
        views.push(("get", point_pairs(p)));
    }
    if let Some(m) = &batched {
        views.push(("multi_get", point_pairs(m)));
    }

    for w in views.windows(2) {
        if w[0].1 != w[1].1 {
            return Verdict::Violation(format!(
                "{label}: {} and {} disagree ({} vs {} entries); first divergence {:?}",
                w[0].0,
                w[1].0,
                w[0].1.len(),
                w[1].1.len(),
                w[0].1
                    .iter()
                    .zip(w[1].1.iter())
                    .find(|(a, b)| a != b)
                    .map(|(a, b)| (
                        String::from_utf8_lossy(&a.0).into_owned(),
                        String::from_utf8_lossy(&b.0).into_owned()
                    )),
            ));
        }
    }

    // Every surviving surface agreed; it must also be the truth. A
    // surface that answered at all and answered something the workload
    // never wrote is the silent-wrong-data case the metadata checksum fix is about.
    for (name, got) in &views {
        if *got != want {
            return Verdict::Violation(format!(
                "{label}: {name} answered without error but served {} entries instead of {}; \
                 the corruption was served as data",
                got.len(),
                want.len(),
            ));
        }
    }
    Verdict::Correct
}

fn sweep(partitioned: bool, stride: usize, label: &str) {
    let files = fixture(partitioned);
    let rel = sst_rel(&files);
    let original = files
        .iter()
        .find(|(p, _)| *p == rel)
        .map(|(_, b)| b.clone())
        .expect("sst bytes");

    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    plant(&files, &db);
    assert!(
        matches!(probe(&db, "pristine"), Verdict::Correct),
        "the pristine fixture must read back correctly"
    );

    // Every offset in the last 256 bytes (footer plus the metadata that
    // precedes it), then a stride across the rest.
    let mut offsets: Vec<usize> = (original.len().saturating_sub(256)..original.len()).collect();
    offsets.extend((0..original.len()).step_by(stride));
    offsets.sort_unstable();
    offsets.dedup();

    let mut refused = 0usize;
    let mut correct = 0usize;
    let mut bad = Vec::new();
    for &off in &offsets {
        for fill in [0x00u8, 0xFFu8, 0x01u8, 0x02u8, 0x03u8, 0x04u8] {
            if original[off] == fill {
                continue;
            }
            let mut bytes = original.clone();
            bytes[off] = fill;
            plant(&files, &db);
            fs::write(db.join(&rel), &bytes).expect("write sst");
            match probe(&db, &format!("{label} offset {off} -> {fill:#04x}")) {
                Verdict::Refused => refused += 1,
                Verdict::Correct => correct += 1,
                Verdict::Violation(m) => bad.push(m),
            }
        }
    }
    println!(
        "{label}: {} offsets, {} trials -> {refused} refused, {correct} correct, {} violations",
        offsets.len(),
        refused + correct + bad.len(),
        bad.len(),
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

#[test]
fn a_byte_substitution_in_a_flat_table_never_makes_two_read_surfaces_disagree() {
    sweep(false, 31, "flat");
}

#[test]
fn a_byte_substitution_in_a_partitioned_table_never_makes_two_read_surfaces_disagree() {
    sweep(true, 31, "partitioned");
}

/// Rewriting the magic's version byte to another *known* version is the
/// corruption that makes a reader parse the footer at the wrong length:
/// a V3 footer read as V1 takes its seven fields from the wrong 56
/// bytes, with no metadata checksum to stop it. Every such relabelling
/// must be refused, never served.
#[test]
fn relabelling_a_tables_format_version_is_never_served_as_data() {
    for partitioned in [false, true] {
        let files = fixture(partitioned);
        let rel = sst_rel(&files);
        let original = files
            .iter()
            .find(|(p, _)| *p == rel)
            .map(|(_, b)| b.clone())
            .expect("sst bytes");
        let version_byte = original.len() - 8;
        let current = original[version_byte];
        assert!(
            (5..=6).contains(&current),
            "the fixture's version byte is {current:#04x}, not a known version. \
             v5 and v6 are the REGOSST flat and partitioned footers.",
        );

        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut bad = Vec::new();
        let mut refused = 0usize;
        for v in [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06] {
            if v == current {
                continue;
            }
            let mut bytes = original.clone();
            bytes[version_byte] = v;
            plant(&files, &db);
            fs::write(db.join(&rel), &bytes).expect("write sst");
            match probe(&db, &format!("version {current:#04x} relabelled {v:#04x}")) {
                Verdict::Refused => refused += 1,
                Verdict::Correct => bad.push(format!(
                    "partitioned={partitioned}: relabelling version {current:#04x} as {v:#04x} \
                     still read back as correct, so the version byte carries no meaning"
                )),
                Verdict::Violation(m) => bad.push(m),
            }
        }
        println!(
            "partitioned={partitioned}: version {current:#04x} relabelled 5 ways, \
             {refused} refused"
        );
        assert!(bad.is_empty(), "{}", bad.join("\n  "));
    }
}
