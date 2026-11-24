//! Adversarial probes for the WAL torn-tail rule (the torn-tail rule).
//!
//! The rule under test: a damaged record that nothing after it proves
//! synced is the ordinary shape of a crash and must be discarded, keeping
//! every whole record before it, and reported; damage a later record
//! proves synced is corruption and must be an error rather than a silent
//! truncation.
//!
//! The fixture is richer than the one in `tests/corruption_exhaustive`:
//! it mixes single puts, deletes, range deletes, merges and multi-op
//! batches, with keys and values of several lengths, so a length field
//! damaged into another plausible length has somewhere to land.
//!
//! Every trial asserts on the recovered *state*, not on the absence of a
//! panic. The two acceptable outcomes are a loud refusal or a state
//! equal to the state after some whole number of the writes; a state
//! that is neither is silent corruption and is reported as such.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use regolith::{Db, Options, WriteBatch, WriteOptions};
use tempfile::TempDir;

mod common;

use common::wal_format;

/// One write the fixture performed, in order.
#[derive(Clone, Debug)]
enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    Batch(Vec<(Vec<u8>, Vec<u8>)>),
}

/// The key-value state a reader sees.
type State = Vec<(Vec<u8>, Vec<u8>)>;

/// The prefix states: `states[k]` is the state after the first `k` ops.
struct Fixture {
    files: Vec<(String, Vec<u8>)>,
    states: Vec<State>,
    wal_rel: String,
}

fn opts() -> Options {
    Options::default()
        // Large enough that nothing flushes: every write stays in the
        // WAL, which is what the tests corrupt.
        .write_buffer_size(1 << 22)
}

fn ops() -> Vec<Op> {
    let mut out = Vec::new();
    for i in 0..6usize {
        let k = format!("key{i:02}").into_bytes();
        let v = vec![b'a' + (i as u8 % 26); 1 + i * 5];
        out.push(Op::Put(k, v));
    }
    out.push(Op::Delete(b"key02".to_vec()));
    out.push(Op::Batch(vec![
        (b"bat0".to_vec(), vec![1u8; 3]),
        (b"bat1".to_vec(), vec![2u8; 17]),
        (b"bat2".to_vec(), vec![3u8; 40]),
    ]));
    out.push(Op::Put(b"key07".to_vec(), vec![0xffu8; 64]));
    out.push(Op::Batch(vec![
        (b"bat3".to_vec(), vec![0u8; 1]),
        (b"bat4".to_vec(), vec![0u8; 1]),
    ]));
    out.push(Op::Put(b"zz".to_vec(), b"last".to_vec()));
    out
}

fn apply(db: &Db, op: &Op) {
    let w = WriteOptions {
        sync: true,
        ..WriteOptions::default()
    };
    match op {
        Op::Put(k, v) => db.put_opt(&w, k, v).expect("put"),
        Op::Delete(k) => db.delete_opt(&w, k).expect("delete"),
        Op::Batch(pairs) => {
            let mut b = WriteBatch::new();
            for (k, v) in pairs {
                b.put(k, v);
            }
            db.write_opt(&w, b).expect("write batch");
        }
    }
}

fn drain(db: &Db) -> State {
    let mut it = db.iter();
    it.seek_to_first();
    let mut out = State::new();
    while it.valid() {
        out.push((
            it.key().expect("valid iter has a key").to_vec(),
            it.value().expect("valid iter has a value").to_vec(),
        ));
        it.next();
    }
    it.status().expect("iterator error");
    out
}

fn capture(db: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).expect("read_dir").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &format!("{prefix}{name}/"), out);
            } else if name != "LOCK" {
                out.push((format!("{prefix}{name}"), fs::read(&path).expect("read")));
            }
        }
    }
    let mut out = Vec::new();
    walk(db, "", &mut out);
    out.sort();
    out
}

/// Build the fixture: apply every op with `Immediate` durability, then
/// capture the directory while the database is still open, so the WAL
/// still holds every record. The prefix states are measured by replaying
/// each prefix into its own database rather than being derived by hand.
fn fixture() -> &'static Fixture {
    static F: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let all = ops();
        let mut states = Vec::with_capacity(all.len() + 1);
        for k in 0..=all.len() {
            let root = TempDir::new().expect("tempdir");
            let path = root.path().join("db");
            let db = Db::open(&path, opts()).expect("open");
            for op in all.iter().take(k) {
                apply(&db, op);
            }
            states.push(drain(&db));
        }

        let root = TempDir::new().expect("tempdir");
        let path = root.path().join("db");
        let db = Db::open(&path, opts()).expect("open");
        for op in &all {
            apply(&db, op);
        }
        let files = capture(&path);
        drop(db);

        let wal_rel = files
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| n.ends_with(".log"))
            .expect("fixture has a WAL");
        Fixture {
            files,
            states,
            wal_rel,
        }
    })
}

fn plant(fx: &Fixture, db: &Path) {
    let _ = fs::remove_dir_all(db);
    for (rel, bytes) in &fx.files {
        let p = db.join(rel);
        fs::create_dir_all(p.parent().expect("has parent")).expect("create_dir_all");
        fs::write(&p, bytes).expect("write");
    }
}

/// What one trial recovered.
enum Recovered {
    /// Open succeeded and the whole keyspace read back as this state.
    Opened(State),
    /// Open refused, or a read through the opened handle failed.
    Refused(String),
}

fn recover(db: &Path) -> Recovered {
    match Db::open(db, opts()) {
        Err(e) => Recovered::Refused(format!("open refused: {e}")),
        Ok(handle) => {
            let mut it = handle.iter();
            it.seek_to_first();
            let mut out = State::new();
            while it.valid() {
                let (Some(k), Some(v)) = (it.key(), it.value()) else {
                    break;
                };
                out.push((k.to_vec(), v.to_vec()));
                it.next();
            }
            match it.status() {
                Err(e) => Recovered::Refused(format!("read failed: {e}")),
                Ok(()) => Recovered::Opened(out),
            }
        }
    }
}

fn trial(fx: &Fixture, db: &Path, mutate: impl FnOnce(&Path)) -> Recovered {
    plant(fx, db);
    mutate(db);
    recover(db)
}

/// Which prefixes of the write history a recovered state matches.
fn matching_prefixes(fx: &Fixture, state: &State) -> Vec<usize> {
    fx.states
        .iter()
        .enumerate()
        .filter(|(_, want)| *want == state)
        .map(|(k, _)| k)
        .collect()
}

fn wal_path(db: &Path, fx: &Fixture) -> PathBuf {
    db.join(&fx.wal_rel)
}

fn truncate_to(path: &Path, len: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).expect("open");
    f.set_len(len).expect("set_len");
    f.sync_all().expect("sync");
}

fn flip(path: &Path, offset: usize, bit: u8) {
    let mut bytes = fs::read(path).expect("read");
    bytes[offset] ^= 1 << bit;
    fs::write(path, &bytes).expect("write");
}

fn overwrite(path: &Path, offset: usize, patch: &[u8]) {
    let mut bytes = fs::read(path).expect("read");
    bytes[offset..offset + patch.len()].copy_from_slice(patch);
    fs::write(path, &bytes).expect("write");
}

/// Record boundaries in a WAL, derived from the on-disk framing as
/// `common::wal_format` reads it.
fn frames(bytes: &[u8]) -> Vec<(usize, usize)> {
    wal_format::record_bounds(bytes)
        .windows(2)
        .map(|w| (w[0], w[1]))
        .collect()
}

// --- the attacks -------------------------------------------------------

/// The whole file is cut at every single byte offset. A cut leaves every
/// record before it exactly as written, so open must serve one of the
/// prefix states, never an invented one.
#[test]
fn a_cut_at_every_byte_offset_lands_on_a_whole_prefix() {
    let fx = fixture();
    let bytes = fx
        .files
        .iter()
        .find(|(n, _)| *n == fx.wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");
    let fr = frames(&bytes);
    assert_eq!(
        fr.len(),
        fx.states.len() - 1,
        "this sweep maps one WAL record to one write; the fixture no longer does",
    );
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    let mut refused = 0usize;
    for cut in 0..=bytes.len() {
        let outcome = trial(fx, &db, |d| truncate_to(&wal_path(d, fx), cut as u64));
        // Every record whose last byte is at or before the cut is intact,
        // so exactly that many writes must come back: not fewer, which
        // would drop an intact record, and not more, which would replay a
        // record the file no longer holds whole.
        let survived = fr.iter().filter(|(_, end)| *end <= cut).count();
        match outcome {
            Recovered::Refused(_) => refused += 1,
            Recovered::Opened(state) => {
                let ks = matching_prefixes(fx, &state);
                if !ks.contains(&survived) {
                    bad.push(format!(
                        "WAL cut to {cut}: {survived} whole record(s) survived, so the state \
                         must be prefix {survived}, but it matches {ks:?}",
                    ));
                }
            }
        }
    }
    println!(
        "cut sweep: {} offsets, {refused} refused, {} violations",
        bytes.len() + 1,
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// Damage anywhere but the *last* record's length field must be caught.
/// Every bit of every byte of every record before the last one is
/// flipped; whole records follow every one of those flips, so replay
/// has the evidence it needs and a silent truncation there is data loss.
#[test]
fn a_bit_flip_in_any_record_but_the_last_is_refused() {
    let fx = fixture();
    let bytes = fx
        .files
        .iter()
        .find(|(n, _)| *n == fx.wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");
    let fr = frames(&bytes);
    assert!(
        fr.len() >= 4,
        "fixture needs several records, got {}",
        fr.len()
    );
    let last_start = fr.last().expect("non-empty").0;
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    let mut refused = 0usize;
    let mut trials = 0usize;
    for offset in 0..last_start {
        for bit in 0..8u8 {
            trials += 1;
            let outcome = trial(fx, &db, |d| flip(&wal_path(d, fx), offset, bit));
            match outcome {
                Recovered::Refused(_) => refused += 1,
                Recovered::Opened(state) => {
                    let ks = matching_prefixes(fx, &state);
                    // Opening is only defensible if the flip changed
                    // nothing an observer can see: the full history.
                    if ks != vec![fx.states.len() - 1] {
                        bad.push(format!(
                            "byte {offset} bit {bit}: a flip in a NON-final record opened on \
                             prefix(es) {ks:?} instead of being refused; whole records follow \
                             it, so this is silent data loss",
                        ));
                    }
                }
            }
        }
    }
    println!(
        "mid-log flip sweep: {trials} trials, {refused} refused, {} violations",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A length field in the middle of the log inflated past the end of the
/// file is not a torn write: whole records follow it. Replay must refuse
/// rather than discard them.
#[test]
fn a_mid_log_length_inflated_past_the_file_is_refused() {
    let fx = fixture();
    let bytes = fx
        .files
        .iter()
        .find(|(n, _)| *n == fx.wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");
    let fr = frames(&bytes);
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    let mut trials = 0usize;
    for &(start, end) in &fr[..fr.len() - 1] {
        let real = (end - start - wal_format::HEADER_LEN) as u32;
        for extra in [1u32, 3, 9, 64, 4096, 1 << 20, u32::MAX - real] {
            trials += 1;
            let claimed = real + extra;
            let outcome = trial(fx, &db, |d| {
                overwrite(&wal_path(d, fx), start, &claimed.to_le_bytes())
            });
            if let Recovered::Opened(state) = outcome {
                bad.push(format!(
                    "record at {start} claiming {claimed} bytes instead of {real}: opened on \
                     prefix(es) {:?} instead of refusing",
                    matching_prefixes(fx, &state)
                ));
            }
        }
    }
    println!(
        "mid-log inflated length: {trials} trials, {} violations",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A flip anywhere in the *last* record, with nothing after it to prove
/// that record was synced, cannot be told from the record a crash tore:
/// the open keeps every record before it and reports the drop. Under the
/// fixture's `Immediate` durability this is the residual case the format
/// accepts rather than padding every synced group to whole sectors, and
/// it is never silent.
#[test]
fn a_flip_in_the_last_record_drops_only_it_and_is_reported() {
    let fx = fixture();
    let bytes = fx
        .files
        .iter()
        .find(|(n, _)| *n == fx.wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");
    let (start, end) = *frames(&bytes).last().expect("non-empty");
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    let mut trials = 0usize;
    for offset in (start..end).step_by(3) {
        for bit in [0u8, 5] {
            trials += 1;
            plant(fx, &db);
            flip(&wal_path(&db, fx), offset, bit);
            let reports = wal_format::TailReports::new();
            let opened = Db::open(
                &db,
                opts().listeners(vec![reports.clone() as Arc<dyn regolith::EventListener>]),
            );
            let Ok(handle) = opened else {
                bad.push(format!(
                    "byte {offset} bit {bit}: refused, expected a reported drop"
                ));
                continue;
            };
            let ks = matching_prefixes(fx, &drain(&handle));
            if ks != vec![fx.states.len() - 2] || reports.taken().len() != 1 {
                bad.push(format!(
                    "byte {offset} bit {bit}: opened on prefix(es) {ks:?} with {} report(s)",
                    reports.taken().len()
                ));
            }
        }
    }
    println!(
        "last-record flips: {trials} trials, {} violations",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// Garbage appended after a whole log is never replayed as data.
#[test]
fn garbage_appended_after_the_log_is_never_served_as_data() {
    let fx = fixture();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    let shapes: Vec<Vec<u8>> = vec![
        vec![0u8; 1],
        vec![0u8; 4],
        vec![0u8; 5],
        vec![0u8; 9],
        vec![0u8; 4096],
        vec![0xffu8; 5],
        vec![0xffu8; 64],
        vec![0x01u8; 4096],
        (0..=255u8).collect(),
        b"not a wal record at all".to_vec(),
    ];
    for (i, tail) in shapes.iter().enumerate() {
        let outcome = trial(fx, &db, |d| {
            let p = wal_path(d, fx);
            let mut bytes = fs::read(&p).expect("read");
            bytes.extend_from_slice(tail);
            fs::write(&p, &bytes).expect("write");
        });
        if let Recovered::Opened(state) = outcome {
            let ks = matching_prefixes(fx, &state);
            if ks != vec![fx.states.len() - 1] {
                bad.push(format!(
                    "garbage shape {i} ({} bytes): opened on prefix(es) {ks:?}, not the full \
                     history",
                    tail.len()
                ));
            }
        }
    }
    println!(
        "appended garbage: {} shapes, {} violations",
        shapes.len(),
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A zero-length WAL, a one-byte WAL and a header-only WAL are all
/// ordinary crash artifacts: open must succeed and serve the state with
/// no records replayed.
#[test]
fn a_degenerate_wal_opens_with_nothing_replayed() {
    let fx = fixture();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut bad = Vec::new();
    for (label, bytes) in [
        ("zero-length", Vec::new()),
        ("one byte", vec![0u8]),
        ("four bytes", vec![0u8; 4]),
        ("header only, zero length", vec![0, 0, 0, 0, 1]),
        ("header only, huge length", vec![0xff, 0xff, 0xff, 0xff, 1]),
    ] {
        let outcome = trial(fx, &db, |d| {
            fs::write(wal_path(d, fx), &bytes).expect("write");
        });
        match outcome {
            Recovered::Refused(_) => {}
            Recovered::Opened(state) => {
                let ks = matching_prefixes(fx, &state);
                if !ks.contains(&0) {
                    bad.push(format!(
                        "{label}: opened on prefix(es) {ks:?}; no record is whole, so only the \
                         empty state is defensible",
                    ));
                }
            }
        }
    }
    println!("degenerate WALs: 5 shapes, {} violations", bad.len());
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// The id embedded in a WAL filename, e.g. `wal/wal_000002.log` -> 2.
fn wal_id_of(rel: &str) -> u64 {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("wal_"))
        .and_then(|s| s.parse().ok())
        .expect("wal filename carries an id")
}

/// Lay the fixture down with its single WAL split into two files at a
/// record boundary, and optionally cut the *earlier* file short.
///
/// Splitting at a boundary changes nothing an observer can see: the same
/// records, in the same order, with the same sequence numbers, across two
/// files instead of one. That makes the split itself a control, and the
/// cut the only variable.
fn plant_split(fx: &Fixture, db: &Path, split_at: usize, cut_first_to: Option<usize>) {
    plant(fx, db);
    let bytes = fs::read(wal_path(db, fx)).expect("read wal");
    let id = wal_id_of(&fx.wal_rel);
    let dir = wal_path(db, fx).parent().expect("wal dir").to_path_buf();
    fs::remove_file(wal_path(db, fx)).expect("remove original wal");
    let (head, tail) = wal_format::split_at(&bytes, split_at);
    let head = match cut_first_to {
        Some(n) => &bytes[..n],
        None => &head[..],
    };
    fs::write(dir.join(format!("wal_{id:06}.log")), head).expect("write first wal");
    fs::write(dir.join(format!("wal_{:06}.log", id + 1)), &tail).expect("write second wal");
}

/// A torn record at the end of a WAL file that is **not** the last WAL
/// file is not a crash artifact: whole records demonstrably follow it in
/// the next file. Discarding it there loses acknowledged, fsynced writes
/// out of the middle of the history and serves a state that never
/// existed.
///
/// `Wal::replay` used to judge each file on its own bytes, applying the
/// torn-tail rule, which is only sound for the newest WAL file, to
/// every one of them. Replay now takes a `WalPosition`, and
/// `RegolithEngine::open` passes it from the id order it already walks, so
/// damage in a file a rotation already closed is refused instead of
/// being discarded as a tail.
///
/// The second file is the tail of the fixture's log under the same stamp,
/// its records signed again for their new offsets
/// (`wal_format::split_at`), so the split itself is invisible: the
/// control, the same split with no cut, opens on the full history, and
/// the cut is the only variable. Every cut must refuse, which a replay
/// that treated the earlier file's damage as its tail would not.
///
/// Reaching it needs two WAL files at open, which is the window between
/// a rotation and the flush that removes the old file, plus damage to
/// the earlier file's length field. Under `DurabilityMode::Immediate`
/// every record in the earlier file was fsynced whole, so that damage is
/// media rot rather than a torn write: exactly the class the
/// benign/malignant split exists to refuse.
#[test]
fn a_torn_tail_in_an_earlier_wal_file_is_not_the_end_of_the_log() {
    let fx = fixture();
    let bytes = fx
        .files
        .iter()
        .find(|(n, _)| *n == fx.wal_rel)
        .map(|(_, b)| b.clone())
        .expect("wal bytes");
    let fr = frames(&bytes);
    assert!(fr.len() >= 6, "need several records, got {}", fr.len());
    let split = fr[fr.len() / 2].0;
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    // Control: the split alone must be invisible.
    plant_split(fx, &db, split, None);
    match recover(&db) {
        Recovered::Opened(state) => assert_eq!(
            matching_prefixes(fx, &state),
            vec![fx.states.len() - 1],
            "the split alone changed what recovery serves; the attack below would be \
             measuring the split, not the cut",
        ),
        Recovered::Refused(why) => panic!("the split alone must open cleanly: {why}"),
    }

    // Attack: cut the earlier file inside its own last record.
    let last_in_head = fr[..fr.len() / 2].last().expect("non-empty");
    let mut bad = Vec::new();
    for cut in last_in_head.0 + 1..last_in_head.1 {
        plant_split(fx, &db, split, Some(cut));
        if let Recovered::Opened(state) = recover(&db) {
            bad.push(format!(
                "earlier WAL cut to {cut}: opened on prefix(es) {:?} ({} entries) instead of \
                 refusing; the earlier log was complete when the newer one began",
                matching_prefixes(fx, &state),
                state.len(),
            ));
        }
    }
    println!(
        "earlier-WAL torn tail: {} cuts, {} violations",
        last_in_head.1 - last_in_head.0 - 1,
        bad.len(),
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}
