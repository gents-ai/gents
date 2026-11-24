//! The machinery behind `tests/corruption_exhaustive.rs`: pristine
//! fixtures, the read-back checker that decides whether a corrupt
//! database answered honestly, the sweep tally, and the on-disk shapes
//! the sweeps aim at.
//!
//! It lives beside the test file rather than inside it so that the test
//! file reads as a list of properties. Nothing here asserts a property of
//! the engine; every assertion in this module is about the fixture being
//! the shape the tests believe it is, so a format change fails loudly
//! instead of quietly corrupting nothing.

use std::collections::HashMap;
use std::fs;
use std::panic;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use regolith::DurabilityMode;
use regolith::{Db, Options};
use tempfile::TempDir;

use crate::common::fault::History;
// Only the WAL fixtures spawn a child, and they exist only on linux.
#[cfg(target_os = "linux")]
use crate::common::fault::{ChildSpec, CrashRun, Phase, validate_prefix_of_state};

/// Seed for every sampled sweep. Fixed, so a failing offset is reported
/// once and reproduced on the next run without a `--seed` dance.
pub const SEED: u64 = 0x_C0FF_EE12_3456_789A;

/// Offsets sampled per file (truncation) and per region (bit flips) in
/// the default run. The exhaustive twins visit every offset instead.
pub const SAMPLE: usize = 24;

/// How long a single trial may make no progress before the sweep is
/// declared hung. This bounds a stall, not the total runtime, so a slow
/// machine simply takes longer rather than failing.
pub const STALL_LIMIT: Duration = Duration::from_secs(30);

// ─── fixtures ───────────────────────────────────────────────────────────

pub type State = Vec<(Vec<u8>, Vec<u8>)>;

/// A pristine database directory captured as bytes, plus everything a
/// test needs to say what the engine should have returned.
pub struct Fixture {
    /// `(relative path, contents)` for every file regolith wrote. `LOCK` is
    /// process state rather than data and is left out.
    files: Vec<(String, Vec<u8>)>,
    /// The writes that produced it, in order.
    /// The write history the fixture replayed. Read only by the WAL
    /// sweeps, which are linux-only, but carried on every platform so
    /// the fixture type does not change shape between them.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub history: History,
    /// What an uncorrupted open reads back.
    pub state: State,
    write_buffer: usize,
}

impl Fixture {
    pub fn opts(&self) -> Options {
        Options::default().write_buffer_size(self.write_buffer)
    }

    /// Lay the pristine files down at `db`, replacing whatever a previous
    /// trial left there.
    pub fn plant(&self, db: &Path) {
        let _ = fs::remove_dir_all(db);
        for (rel, bytes) in &self.files {
            let path = db.join(rel);
            fs::create_dir_all(path.parent().expect("relative path has a parent"))
                .expect("plant: create_dir_all");
            fs::write(&path, bytes).expect("plant: write");
        }
    }

    pub fn bytes(&self, rel: &str) -> &[u8] {
        self.files
            .iter()
            .find(|(name, _)| name == rel)
            .map(|(_, b)| b.as_slice())
            .unwrap_or_else(|| panic!("fixture has no {rel}"))
    }

    /// The single file with extension `ext`. Panics when there is not
    /// exactly one, so a fixture that silently changed shape fails here
    /// rather than corrupting nothing.
    pub fn only(&self, ext: &str) -> String {
        let mut hits: Vec<&String> = self
            .files
            .iter()
            .map(|(name, _)| name)
            .filter(|name| name.ends_with(ext))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one {ext} file, got {hits:?}"
        );
        hits.pop().expect("checked non-empty").clone()
    }
}

/// Copy every data file out of a database directory.
pub fn capture(db: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).expect("capture: read_dir").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &format!("{prefix}{name}/"), out);
            } else if name != "LOCK" {
                out.push((
                    format!("{prefix}{name}"),
                    fs::read(&path).expect("capture: read"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(db, "", &mut out);
    out.sort();
    out
}

/// Read an untouched copy back, so every expectation in this file is
/// measured from the engine rather than assumed.
pub fn measure_pristine(files: &[(String, Vec<u8>)], write_buffer: usize) -> State {
    let probe = Fixture {
        files: files.to_vec(),
        history: History::new(),
        state: Vec::new(),
        write_buffer,
    };
    let root = TempDir::new().expect("pristine: tempdir");
    let db = root.path().join("db");
    probe.plant(&db);
    match recover(&db, probe.opts()) {
        Recovered::Opened(state) => state,
        other => panic!(
            "the pristine fixture must open cleanly, got {}",
            other.why()
        ),
    }
}

/// A database killed mid-life with every write still in the WAL: a clean
/// close would flush the memtable to an SSTable and leave nothing to
/// corrupt, so the writer is a real child process that is killed.
#[cfg(target_os = "linux")]
pub fn build_wal_fixture(batch_size: usize, delete_every: usize) -> Fixture {
    const OPS: usize = 8;
    const WRITE_BUFFER: usize = 1 << 20;
    let root = TempDir::new().expect("wal fixture: tempdir");
    let db = root.path().join("db");
    let spec = ChildSpec::new(Phase::AfterNPuts, &db)
        .ops(OPS)
        .batch_size(batch_size)
        .value_len(4)
        .delete_every(delete_every)
        .durability(DurabilityMode::Immediate)
        .write_buffer_size(WRITE_BUFFER);
    let history = spec.history();
    let outcome = CrashRun::new(spec.clone()).record_io(false).run();
    outcome.assert_killed();
    assert_eq!(
        outcome.acked_count(),
        OPS,
        "the fixture child must acknowledge all {OPS} writes before it dies",
    );
    let files = capture(&db);
    let state = measure_pristine(&files, WRITE_BUFFER);
    Fixture {
        files,
        history,
        state,
        write_buffer: WRITE_BUFFER,
    }
}

#[cfg(target_os = "linux")]
pub fn wal_fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| build_wal_fixture(1, 3))
}

#[cfg(target_os = "linux")]
pub fn batch_fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| build_wal_fixture(4, 0))
}

/// A closed database whose data lives in one compacted SSTable, with a
/// MANIFEST that has real `AddFile`/`RemoveFile` history behind it.
pub fn table_fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        const KEYS: usize = 24;
        const WRITE_BUFFER: usize = 4096;
        let root = TempDir::new().expect("table fixture: tempdir");
        let db = root.path().join("db");
        let opts = Options::default().write_buffer_size(WRITE_BUFFER);
        let mut history = History::new();
        let handle = Db::open(&db, opts).expect("table fixture: open");
        for i in 0..KEYS {
            let (k, v) = (format!("key_{i:04}"), format!("value_{i:04}"));
            regolith::through_stalls(|| handle.put(k.as_bytes(), v.as_bytes()))
                .expect("fixture put");
            history.put(k.into_bytes(), v.into_bytes());
        }
        handle
            .compact_range(None, None)
            .wait()
            .expect("fixture compact");
        handle.close().expect("fixture close");
        drop(handle);
        let files = capture(&db);
        let state = measure_pristine(&files, WRITE_BUFFER);
        assert_eq!(state.len(), KEYS, "the fixture must hold every key");
        Fixture {
            files,
            history,
            state,
            write_buffer: WRITE_BUFFER,
        }
    })
}

// ─── reading a possibly-corrupt database ────────────────────────────────

/// What a corruption trial produced. Only the first three are acceptable;
/// [`Recovered::Broken`] is the engine contradicting itself and is always
/// a bug.
pub enum Recovered {
    /// `Db::open` refused, with this message.
    Refused(String),
    /// The database opened and the whole state read back.
    Opened(State),
    /// The database opened and a read reported an error. Loud, so fine.
    ReadFailed(String),
    /// The database served an inconsistent view: a scan that went
    /// backwards, a reverse scan that disagreed with the forward one, or
    /// a point lookup that disagreed with the scan.
    Broken(String),
}

impl Recovered {
    pub fn why(&self) -> String {
        match self {
            Recovered::Refused(e) => format!("refused: {e}"),
            Recovered::Opened(s) => format!("opened with {} keys", s.len()),
            Recovered::ReadFailed(e) => format!("read failed: {e}"),
            Recovered::Broken(w) => format!("inconsistent: {w}"),
        }
    }
}

pub enum ReadError {
    Engine(String),
    Broken(String),
}

/// Read the entire database three ways and require the three to agree.
/// The point of reading it three ways is that an engine serving corrupt
/// data usually still serves *something*; disagreement between the
/// forward scan, the reverse scan and a point lookup is how that shows.
pub fn read_state(db: &Db) -> Result<State, ReadError> {
    let mut forward: State = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        let k = match it.key() {
            Some(k) => k.to_vec(),
            None => return Err(ReadError::Broken("valid iterator with no key".into())),
        };
        let v = match it.value() {
            Some(v) => v.to_vec(),
            None => return Err(ReadError::Broken("valid iterator with no value".into())),
        };
        if let Some((prev, _)) = forward.last()
            && prev >= &k
        {
            return Err(ReadError::Broken(format!(
                "forward scan returned {} after {}",
                show(&k),
                show(prev)
            )));
        }
        forward.push((k, v));
        it.next();
    }
    it.status()
        .map_err(|e| ReadError::Engine(format!("forward scan: {e}")))?;

    let mut backward: State = Vec::new();
    let mut rit = db.iter();
    rit.seek_to_last();
    while rit.valid() {
        match (rit.key(), rit.value()) {
            (Some(k), Some(v)) => backward.push((k.to_vec(), v.to_vec())),
            _ => {
                return Err(ReadError::Broken(
                    "valid reverse iterator with no entry".into(),
                ));
            }
        }
        rit.prev();
    }
    rit.status()
        .map_err(|e| ReadError::Engine(format!("reverse scan: {e}")))?;
    backward.reverse();
    if backward != forward {
        return Err(ReadError::Broken(format!(
            "reverse scan yielded {} entries, forward yielded {}, first difference at {:?}",
            backward.len(),
            forward.len(),
            forward
                .iter()
                .zip(backward.iter())
                .position(|(a, b)| a != b),
        )));
    }

    for (k, v) in &forward {
        match db.get(k) {
            Ok(Some(got)) if &got == v => {}
            Ok(other) => {
                return Err(ReadError::Broken(format!(
                    "scan says {} -> {}, get says {:?}",
                    show(k),
                    show(v),
                    other.as_deref().map(show),
                )));
            }
            Err(e) => return Err(ReadError::Engine(format!("get {}: {e}", show(k)))),
        }
    }
    Ok(forward)
}

pub fn recover(db: &Path, opts: Options) -> Recovered {
    match Db::open(db, opts) {
        Err(e) => Recovered::Refused(e.to_string()),
        Ok(handle) => {
            let outcome = match read_state(&handle) {
                Ok(state) => Recovered::Opened(state),
                Err(ReadError::Engine(e)) => Recovered::ReadFailed(e),
                Err(ReadError::Broken(w)) => Recovered::Broken(w),
            };
            let _ = handle.close();
            outcome
        }
    }
}

pub fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Plant the pristine tree, corrupt it, then open and read it back.
pub fn trial(fixture: &Fixture, db: &Path, mutate: impl FnOnce(&Path)) -> Recovered {
    fixture.plant(db);
    mutate(db);
    recover(db, fixture.opts())
}

// ─── expectations ───────────────────────────────────────────────────────

/// The recovered state must equal what the pristine database held.
pub fn exactly(pristine: &State) -> impl Fn(&State) -> Result<(), String> + '_ {
    move |state| {
        if state == pristine {
            Ok(())
        } else {
            Err(format!(
                "expected the {} pristine keys, got {}",
                pristine.len(),
                state.len()
            ))
        }
    }
}

/// Losing data is allowed (a discarded manifest tail drops whole tables);
/// inventing it never is. Every key served must carry the value it was
/// written with.
pub fn never_invents(pristine: &State) -> impl Fn(&State) -> Result<(), String> + '_ {
    let index: HashMap<&[u8], &[u8]> = pristine
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    move |state| {
        for (k, v) in state {
            match index.get(k.as_slice()) {
                None => return Err(format!("served key {} that was never written", show(k))),
                Some(want) if *want != v.as_slice() => {
                    return Err(format!(
                        "key {} came back as {} but was written as {}",
                        show(k),
                        show(v),
                        show(want)
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// The recovered state must be the state after some whole number of the
/// intended writes: no gap, no reordering, no half-applied `WriteBatch`.
#[cfg(target_os = "linux")]
pub fn valid_prefix(history: &History) -> impl Fn(&State) -> Result<(), String> + '_ {
    move |state| {
        validate_prefix_of_state(state, history)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

// ─── sweep bookkeeping ──────────────────────────────────────────────────

/// Counts of each acceptable outcome plus every violation found, so a
/// sweep reports what actually happened instead of only whether it
/// passed. A sweep that ran zero trials is itself a violation: a silent
/// no-op would look exactly like a pass.
#[derive(Default)]
pub struct Tally {
    pub refused: usize,
    pub read_failed: usize,
    pub matched: usize,
    violations: Vec<String>,
    extra_violations: usize,
}

impl Tally {
    pub fn record(
        &mut self,
        label: &str,
        outcome: Recovered,
        expected: &impl Fn(&State) -> Result<(), String>,
    ) {
        match outcome {
            Recovered::Refused(e) if e.trim().is_empty() => {
                self.violation(format!("{label}: refused with an empty error message"))
            }
            Recovered::Refused(_) => self.refused += 1,
            Recovered::ReadFailed(e) if e.trim().is_empty() => {
                self.violation(format!("{label}: read failed with an empty error message"))
            }
            Recovered::ReadFailed(_) => self.read_failed += 1,
            Recovered::Broken(w) => self.violation(format!("{label}: {w}")),
            Recovered::Opened(state) => match expected(&state) {
                Ok(()) => self.matched += 1,
                Err(w) => self.violation(format!("{label}: {w}")),
            },
        }
    }

    pub fn violation(&mut self, message: String) {
        if self.violations.len() < 8 {
            self.violations.push(message);
        } else {
            self.extra_violations += 1;
        }
    }

    pub fn total(&self) -> usize {
        self.refused
            + self.read_failed
            + self.matched
            + self.violations.len()
            + self.extra_violations
    }

    pub fn finish(&self, what: &str) {
        println!(
            "{what}: {} trials -> {} refused, {} read errors, {} correct",
            self.total(),
            self.refused,
            self.read_failed,
            self.matched
        );
        assert!(self.total() > 0, "{what}: the sweep ran no trials at all");
        assert!(
            self.violations.is_empty(),
            "{what}: {} violation(s){}:\n  {}",
            self.violations.len() + self.extra_violations,
            if self.extra_violations > 0 {
                format!(" ({} more not shown)", self.extra_violations)
            } else {
                String::new()
            },
            self.violations.join("\n  "),
        );
        assert_engine_never_panicked(what);
    }
}

// ─── watchdogs ──────────────────────────────────────────────────────────

static ENGINE_PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Record any panic raised outside `tests/`, including one on a
/// background compaction thread where the harness would never see it. A
/// test's own assertion failure lives in `tests/` and is left to unwind
/// normally.
pub fn watch_engine_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let from_engine = info
                .location()
                .is_some_and(|loc| !loc.file().starts_with("tests/"));
            if !from_engine {
                previous(info);
                return;
            }
            let mut seen = ENGINE_PANICS.lock().expect("panic list");
            seen.push(info.to_string());
            // Print the first few so a real crash keeps its backtrace,
            // without dumping thousands of them from a sweep.
            if seen.len() <= 3 {
                drop(seen);
                previous(info);
            }
        }));
    });
}

pub fn assert_engine_never_panicked(what: &str) {
    let seen = ENGINE_PANICS.lock().expect("panic list");
    assert!(
        seen.is_empty(),
        "{what}: the engine panicked on corrupt input ({} time(s)):\n  {}",
        seen.len(),
        seen.join("\n  "),
    );
}

/// Run `body` on a worker thread and fail if it stops making progress.
/// An engine that hangs on a corrupt file would otherwise wedge the whole
/// test binary with no output at all. The bound is per trial, so a slow
/// machine takes longer rather than failing, and the poll is a deadline
/// with bounded backoff rather than a fixed sleep.
pub fn watch(label: &'static str, body: impl FnOnce(&AtomicU64) + Send + 'static) {
    watch_engine_panics();
    let progress = Arc::new(AtomicU64::new(0));
    let worker = Arc::clone(&progress);
    let (tx, rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        body(&worker);
        let _ = tx.send(());
    });

    let mut last = 0u64;
    let mut idle_since = Instant::now();
    let mut backoff = Duration::from_micros(200);
    loop {
        match rx.recv_timeout(backoff) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = progress.load(Ordering::Relaxed);
        if now != last {
            last = now;
            idle_since = Instant::now();
            backoff = Duration::from_micros(200);
        } else {
            assert!(
                idle_since.elapsed() < STALL_LIMIT,
                "{label}: no progress past trial {now} for {STALL_LIMIT:?}: the engine is hung",
            );
            backoff = (backoff * 2).min(Duration::from_millis(20));
        }
    }
    if let Err(payload) = handle.join() {
        panic::resume_unwind(payload);
    }
}

// ─── on-disk shapes ─────────────────────────────────────────────────────

/// One framed record. The WAL frames are format 2 group records, read with
/// `common::wal_format`; the MANIFEST writes `[len: u32 LE][edits]
/// [crc: u32 LE]` (`src/engine/manifest.rs`). Knowing the frames is what
/// lets these tests say *where* a cut landed instead of only that it did.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub start: u64,
    pub end: u64,
    /// Record type for a WAL frame, first edit's tag for a MANIFEST frame.
    pub kind: u8,
}

/// The MANIFEST opens with a 12-byte format stamp, so records start there
/// rather than at byte zero. Matches `MANIFEST_STAMP_LEN`.
pub const STAMP: usize = 12;

fn frames(bytes: &[u8], header: usize) -> Vec<Frame> {
    // Only the MANIFEST is framed this way now.
    debug_assert_eq!(header, 4);
    let mut out = Vec::new();
    let mut pos = STAMP;
    while pos + header < bytes.len() {
        let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4 bytes")) as usize;
        let end = match pos
            .checked_add(header)
            .and_then(|p| p.checked_add(len))
            .and_then(|p| p.checked_add(4))
        {
            Some(end) if end <= bytes.len() => end,
            _ => break,
        };
        out.push(Frame {
            start: pos as u64,
            end: end as u64,
            // Byte 4 is the WAL's record type or the first MANIFEST edit's tag.
            kind: bytes[pos + 4],
        });
        pos = end;
    }
    out
}

/// WAL frames: format 2 records, one per commit group.
#[cfg(target_os = "linux")]
pub fn wal_frames(bytes: &[u8]) -> Vec<Frame> {
    let f: Vec<Frame> = crate::common::wal_format::record_bounds(bytes)
        .windows(2)
        .map(|w| Frame {
            start: w[0] as u64,
            end: w[1] as u64,
            kind: crate::common::wal_format::kind_at(bytes, w[0]),
        })
        .collect();
    assert!(!f.is_empty(), "the WAL fixture parsed as zero records");
    f
}

/// MANIFEST frames, whose header is the 4-byte length alone.
pub fn manifest_frames(bytes: &[u8]) -> Vec<Frame> {
    let f = frames(bytes, 4);
    assert!(!f.is_empty(), "the MANIFEST fixture parsed as zero records");
    assert_eq!(
        f.last().expect("non-empty").end,
        bytes.len() as u64,
        "MANIFEST frames must tile the file exactly; the on-disk format changed",
    );
    f
}

pub const TAG_ADD_FILE: u8 = 1;

/// The SSTable trailer ends with the 8-byte magic, whose low byte is the
/// format version. Versions 1 and 2 carry a 64-byte footer, versions 3
/// and 4 a 72-byte one holding an extra metadata checksum ahead of the
/// magic. Both layouts open with the same seven little-endian u64s:
/// range-tombstone offset and size, bloom offset and size, index offset
/// and size, entry count. See `src/engine/sstable.rs`.
fn footer_size(bytes: &[u8]) -> u64 {
    assert!(bytes.len() >= 8, "the SSTable fixture has no magic number");
    let magic = u64::from_le_bytes(
        bytes[bytes.len() - 8..]
            .try_into()
            .expect("the last 8 bytes"),
    );
    // v5 and v6 are the stamped REGOSST flat and partitioned footers.
    // They share the 72-byte layout v3 introduced, so the field
    // offsets this harness uses hold for them too.
    match magic & 0xFF {
        1 | 2 => 64,
        3..=6 => 72,
        other => panic!(
            "the SSTable fixture carries format version {other}, which this harness does not know"
        ),
    }
}

pub const DATA: &str = "data blocks";
pub const BLOOM: &str = "bloom region";
pub const INDEX: &str = "index block";
pub const FOOTER: &str = "footer";

#[derive(Clone, Copy)]
pub struct Region {
    pub name: &'static str,
    pub start: u64,
    pub end: u64,
}

pub fn sst_regions(bytes: &[u8]) -> Vec<Region> {
    let len = bytes.len() as u64;
    let footer_size = footer_size(bytes);
    assert!(len > footer_size, "the SSTable fixture is all footer");
    let f = &bytes[bytes.len() - footer_size as usize..];
    let word = |i: usize| u64::from_le_bytes(f[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    let (rt_offset, rt_size) = (word(0), word(1));
    let (bloom_offset, bloom_size) = (word(2), word(3));
    let (index_offset, index_size) = (word(4), word(5));
    let mut regions = vec![Region {
        name: DATA,
        start: 0,
        end: rt_offset,
    }];
    if rt_size > 0 {
        regions.push(Region {
            name: "range tombstones",
            start: rt_offset,
            end: rt_offset + rt_size,
        });
    }
    regions.push(Region {
        name: BLOOM,
        start: bloom_offset,
        end: bloom_offset + bloom_size,
    });
    regions.push(Region {
        name: INDEX,
        start: index_offset,
        end: index_offset + index_size,
    });
    regions.push(Region {
        name: FOOTER,
        start: len - footer_size,
        end: len,
    });
    for r in &regions {
        assert!(
            r.start < r.end && r.end <= len,
            "{} is not inside the file",
            r.name
        );
    }
    regions
}

pub fn region(bytes: &[u8], name: &str) -> Region {
    sst_regions(bytes)
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("the SSTable fixture has no {name}"))
}

// ─── offset selection ───────────────────────────────────────────────────

/// A deterministic, evenly spread sample of `count` offsets in
/// `start..end`, always including both ends.
pub fn sample(start: u64, end: u64, count: usize, seed: u64) -> Vec<u64> {
    let len = end.saturating_sub(start);
    if len == 0 {
        return Vec::new();
    }
    if len <= count as u64 {
        return (start..end).collect();
    }
    let stride = len / count as u64;
    let mut s = seed | 1;
    let mut out = vec![start, end - 1];
    for i in 0..count as u64 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.push(start + i * stride + s % stride);
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub fn every(start: u64, end: u64) -> Vec<u64> {
    (start..end).collect()
}
