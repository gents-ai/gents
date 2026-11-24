//! Power-loss reconstruction from a recorded syscall stream.
//!
//! # Why this is not `kill -9`
//!
//! Killing a process leaves every byte it wrote sitting in the OS page
//! cache, and the kernel goes on to write those bytes to disk. A `kill -9`
//! test therefore proves that the engine survives losing its *memory*, not
//! that it survives losing its *unsynced disk writes*. A suite that stops
//! at `kill -9` and calls durability proven is a false green.
//!
//! A power cut discards everything that was not `fsync`ed. This module
//! reproduces that: it replays the journal recorded by the `LD_PRELOAD`
//! shim, works out which byte ranges were written but never followed by a
//! successful `fsync`/`fdatasync` on that file, and rewrites the directory
//! as the filesystem would have left it. This is the ALICE model, driven
//! by the syscalls regolith actually issued rather than by an assumption about
//! what regolith issues.
//!
//! # What is modelled
//!
//! * Unsynced byte ranges, dropped or torn per [`TearMode`].
//! * Reordering headroom: a cut can be placed at any journal sequence
//!   number, not only at the end of the run.
//! * Optionally, files created but never made durable by an `fsync` of
//!   their parent directory ([`PowerLossOptions::drop_unsynced_creates`]).
//! * Renames before the cut: the file keeps its bytes and what was synced
//!   of them under its new name, which counts as created at the rename for
//!   the option above.
//!
//! # What is NOT modelled, stated so nobody assumes otherwise
//!
//! * Resurrection of an `unlink`ed file whose directory was never synced.
//!   Undoing an unlink needs the file's contents, which the journal does
//!   not carry. Not resurrecting is the *milder* outcome, so a passing
//!   test here is weaker than reality on that one axis.
//! * Cross-file write reordering below the granularity of a single
//!   `write` call. A partial write is torn at [`PowerLossOptions::
//!   sector_bytes`] granularity, not at arbitrary byte granularity.
//! * Barriers and disk-cache behaviour of a specific device.
//! * A rename after the cut point. The file under its new name is cut back
//!   like any file first touched after the cut, and its old name is not
//!   restored, for the same reason an unlink is not undone.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::bytes::garbage;
use super::journal::{Journal, OpKind};

/// Where in the recorded stream the power was cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutPoint {
    /// Everything the child managed to do before it died. The usual
    /// choice: the crash point was already chosen by the kill trigger.
    End,
    /// Immediately after the journal record with this sequence number.
    AtSeq(u64),
    /// Immediately after the `n`th successful sync, 1-based. The most
    /// meaningful place to cut, because it is exactly the boundary the
    /// durability contract is written in terms of.
    AfterNthSync(usize),
    /// Immediately before the `n`th successful sync, 1-based.
    BeforeNthSync(usize),
}

/// What the filesystem left behind where the unsynced bytes were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TearMode {
    /// The unsynced tail is gone and the file is shorter. The common
    /// outcome on a journalling filesystem with delayed allocation.
    Truncate,
    /// The file keeps its length but the unsynced bytes read back as
    /// zeros. Real on ext4 when blocks were allocated but not written.
    Zero,
    /// The unsynced bytes read back as unrelated data. The harshest and
    /// the one that actually exercises a checksum.
    Garbage,
    /// The unsynced region is dropped, except that the sector straddling
    /// the last synced byte keeps its first half and reads garbage after
    /// it. Models a device that tears at sector granularity, and is the
    /// case that produces a record with a plausible length prefix and a
    /// broken payload.
    TornSector,
}

#[derive(Clone, Debug)]
pub struct PowerLossOptions {
    pub tear: TearMode,
    /// Sector granularity for [`TearMode::TornSector`].
    pub sector_bytes: u64,
    /// Remove files that were created during the run and never made
    /// durable by an `fsync` of their parent directory. Off by default:
    /// it is a second, harsher axis of failure and a test should opt into
    /// it deliberately.
    pub drop_unsynced_creates: bool,
    /// Seed for [`TearMode::Garbage`] and [`TearMode::TornSector`], so a
    /// failure replays byte for byte.
    pub seed: u64,
}

impl Default for PowerLossOptions {
    fn default() -> Self {
        PowerLossOptions {
            tear: TearMode::Truncate,
            sector_bytes: 4096,
            drop_unsynced_creates: false,
            seed: 0x5EED_1A12,
        }
    }
}

impl PowerLossOptions {
    pub fn tear(mut self, tear: TearMode) -> Self {
        self.tear = tear;
        self
    }
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
    pub fn sector_bytes(mut self, n: u64) -> Self {
        self.sector_bytes = n;
        self
    }
    /// Also drop directory entries that were never made durable.
    pub fn strict_creates(mut self) -> Self {
        self.drop_unsynced_creates = true;
        self
    }
}

/// What the reconstruction actually did. Every number here is measured
/// from the rewrite, never assumed.
#[derive(Clone, Debug, Default)]
pub struct PowerLossReport {
    pub cut_seq: u64,
    pub records_replayed: usize,
    /// `(path, length before, length after)`.
    pub truncated: Vec<(PathBuf, u64, u64)>,
    /// `(path, offset, length)` regions overwritten with zeros or garbage.
    pub torn: Vec<(PathBuf, u64, u64)>,
    pub removed: Vec<PathBuf>,
    pub bytes_discarded: u64,
    pub tear: Option<TearMode>,
}

impl PowerLossReport {
    /// True when the reconstruction removed bytes that a `kill -9` would
    /// have kept. A power-loss test that never discards anything proved
    /// nothing beyond a process crash, so tests assert on this.
    pub fn discarded_anything(&self) -> bool {
        self.bytes_discarded > 0 || !self.removed.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "power loss at seq {} ({} records, {} bytes discarded)",
            self.cut_seq, self.records_replayed, self.bytes_discarded,
        );
        for (p, before, after) in &self.truncated {
            s.push_str(&format!(
                "\n  truncate {} {} -> {}",
                p.display(),
                before,
                after
            ));
        }
        for (p, off, len) in &self.torn {
            s.push_str(&format!(
                "\n  tear {} [{}, {})",
                p.display(),
                off,
                off + len
            ));
        }
        for p in &self.removed {
            s.push_str(&format!("\n  remove {}", p.display()));
        }
        s
    }
}

#[derive(Default, Clone)]
struct FileState {
    /// End of file as of the cut, from the replayed stream.
    eof: u64,
    /// Byte ranges written and not yet covered by a successful sync.
    dirty: Vec<(u64, u64)>,
    created_at: Option<u64>,
    dir_synced_after_create: bool,
    /// First sequence number that mentions this path, over the whole run
    /// rather than only up to the cut. A file first touched after the cut
    /// either did not exist yet or was still untouched, and the
    /// reconstruction has to undo it.
    first_seq: u64,
    /// Size the file had when it was first opened in this run.
    size_at_first_open: u64,
}

impl FileState {
    /// Offset of the first byte that was never made durable.
    fn safe_prefix(&self) -> u64 {
        self.dirty
            .iter()
            .map(|(s, _)| *s)
            .min()
            .unwrap_or(self.eof)
            .min(self.eof)
    }
}

/// Rewrite `dir` as a power cut would have left it, using the journal this
/// harness records beside `dir`.
///
/// Panics when no journal is found. Silently falling back to a weaker
/// model would let a downstream test claim a power-loss result it did not
/// have; see [`simulate_power_loss_modelled`] for the explicit fallback.
pub fn simulate_power_loss(dir: &Path, cut: CutPoint) -> PowerLossReport {
    let jp = super::journal::journal_path_for(dir);
    let journal = Journal::read(&jp).unwrap_or_else(|e| panic!("reading {}: {e}", jp.display()));
    assert!(
        !journal.is_empty(),
        "no I/O journal at {}.\n\
         simulate_power_loss needs the LD_PRELOAD shim's recording to know which bytes were \
         never fsynced. Run the workload through fault::run_child (or CrashRun), which sets \
         REGOLITH_FAULT_JOURNAL to that path.",
        jp.display(),
    );
    simulate_power_loss_with(dir, &journal, cut, &PowerLossOptions::default())
}

/// Full-control form: an explicit journal and explicit options.
pub fn simulate_power_loss_with(
    dir: &Path,
    journal: &Journal,
    cut: CutPoint,
    opts: &PowerLossOptions,
) -> PowerLossReport {
    assert_eq!(
        journal.malformed,
        0,
        "journal {} has {} unparseable lines; refusing to reconstruct from a partial recording",
        journal.source.display(),
        journal.malformed,
    );
    let cut_seq = resolve_cut(journal, cut);
    let (files, replayed) = replay(journal, cut_seq);
    apply(dir, files, replayed, cut_seq, opts)
}

fn resolve_cut(journal: &Journal, cut: CutPoint) -> u64 {
    match cut {
        CutPoint::End => journal.last_seq(),
        CutPoint::AtSeq(s) => s,
        CutPoint::AfterNthSync(n) => {
            let syncs = journal.sync_seqs();
            assert!(
                n >= 1 && n <= syncs.len(),
                "CutPoint::AfterNthSync({n}) but the run recorded {} syncs",
                syncs.len(),
            );
            syncs[n - 1]
        }
        CutPoint::BeforeNthSync(n) => {
            let syncs = journal.sync_seqs();
            assert!(
                n >= 1 && n <= syncs.len(),
                "CutPoint::BeforeNthSync({n}) but the run recorded {} syncs",
                syncs.len(),
            );
            syncs[n - 1].saturating_sub(1)
        }
    }
}

/// Replay the whole recorded stream, but freeze each file's byte state at
/// `cut_seq`. Records past the cut are still scanned, because a file that
/// only appears after the cut is one the power cut must undo: it either did
/// not exist yet, or had not been written to yet.
fn replay(journal: &Journal, cut_seq: u64) -> (HashMap<PathBuf, FileState>, usize) {
    let mut files: HashMap<PathBuf, FileState> = HashMap::new();
    let mut replayed = 0usize;
    // The source of each thread's rename whose `RenameTo` record is still to
    // come, and whether this run had opened it. The shim writes a rename's two
    // records one after the other on the thread that renamed, but another
    // thread's records can come between them (`A.from, B.from, A.to, B.to`),
    // so a pair is matched by its thread id, never by what came last.
    let mut renaming: HashMap<i64, (PathBuf, bool)> = HashMap::new();

    for r in journal.records.iter() {
        if r.path.as_os_str().is_empty() || !r.succeeded() {
            continue;
        }
        let seen_before = files.contains_key(&r.path);
        if let Some(st) = files.get_mut(&r.path) {
            st.first_seq = st.first_seq.min(r.seq);
        } else {
            let mut st = FileState {
                first_seq: r.seq,
                ..Default::default()
            };
            if r.kind == OpKind::Open {
                st.size_at_first_open = r.a.max(0) as u64;
                if r.opened_creating() && r.a == 0 {
                    st.created_at = Some(r.seq);
                }
            }
            files.insert(r.path.clone(), st);
        }
        if r.seq > cut_seq {
            continue;
        }
        replayed += 1;
        match r.kind {
            OpKind::Open => {
                let created = r.opened_creating() && r.a == 0;
                let st = files.entry(r.path.clone()).or_default();
                if r.opened_truncating() {
                    st.eof = 0;
                    st.dirty.clear();
                } else {
                    st.eof = st.eof.max(r.a.max(0) as u64);
                }
                if created && st.created_at.is_none() {
                    st.created_at = Some(r.seq);
                }
            }
            OpKind::Write => {
                let n = r.written();
                if n == 0 {
                    continue;
                }
                let st = files.entry(r.path.clone()).or_default();
                // The shim resolves the offset with a side-effect-free
                // `SEEK_CUR` query, which is exact for a plain descriptor
                // and, as the kernel repositions an `O_APPEND` descriptor
                // to the end before each write, exact there too. `-1`
                // means the query failed, so fall back to the running end
                // of file rebuilt from the stream.
                let off = if r.a >= 0 { r.a as u64 } else { st.eof };
                st.dirty.push((off, n));
                st.eof = st.eof.max(off + n);
            }
            OpKind::Sync => {
                if r.path.is_dir() {
                    for (p, st) in files.iter_mut() {
                        if p.parent() == Some(r.path.as_path())
                            && let Some(at) = st.created_at
                            && at < r.seq
                        {
                            st.dir_synced_after_create = true;
                        }
                    }
                } else if let Some(st) = files.get_mut(&r.path) {
                    st.dirty.clear();
                }
            }
            OpKind::Truncate => {
                let len = r.a.max(0) as u64;
                let st = files.entry(r.path.clone()).or_default();
                st.eof = len;
                st.dirty.retain(|(s, _)| *s < len);
                for d in st.dirty.iter_mut() {
                    if d.0 + d.1 > len {
                        d.1 = len - d.0;
                    }
                }
            }
            OpKind::Close => {}
            OpKind::Unlink => {
                files.remove(&r.path);
            }
            OpKind::RenameFrom => {
                renaming.insert(r.tid, (r.path.clone(), seen_before));
            }
            // A rename carries the file's bytes, and what was synced of
            // them, to its new name, which names nothing else afterwards.
            // Left unmodelled, the new name kept the empty state the loop
            // gives a path it has not seen, and the reconstruction cut a
            // file written to a staging name, synced and renamed, the way
            // `atomic_write` makes every backup's metadata, down to nothing.
            // A file this run never opened is left as the loop found it:
            // the journal does not say how long it was.
            OpKind::RenameTo => {
                if let Some((from, true)) = renaming.remove(&r.tid)
                    && let Some(mut moved) = files.remove(&from)
                {
                    // The new name appears now: only a sync of its
                    // directory after this makes it durable.
                    moved.created_at = Some(r.seq);
                    moved.dir_synced_after_create = false;
                    files.insert(r.path.clone(), moved);
                }
            }
        }
    }
    (files, replayed)
}

fn apply(
    dir: &Path,
    files: HashMap<PathBuf, FileState>,
    replayed: usize,
    cut_seq: u64,
    opts: &PowerLossOptions,
) -> PowerLossReport {
    let mut report = PowerLossReport {
        cut_seq,
        records_replayed: replayed,
        tear: Some(opts.tear),
        ..Default::default()
    };

    let mut paths: Vec<(&PathBuf, &FileState)> = files
        .iter()
        .filter(|(p, _)| p.starts_with(dir) && p.is_file())
        .collect();
    paths.sort_by_key(|(p, _)| (*p).clone());

    for (path, st) in paths {
        // Created after the cut: at the moment the power went, this file
        // did not exist.
        if st.first_seq > cut_seq && st.created_at.is_some() {
            let len = super::bytes::file_len(path);
            std::fs::remove_file(path)
                .unwrap_or_else(|e| panic!("power loss: remove {}: {e}", path.display()));
            report.bytes_discarded += len;
            report.removed.push(path.clone());
            continue;
        }
        // Pre-existed the run but was not touched until after the cut: put
        // it back to the size it had before anything was appended.
        if st.first_seq > cut_seq {
            let actual = super::bytes::file_len(path);
            if st.size_at_first_open < actual {
                super::bytes::truncate_at(path, st.size_at_first_open);
                report.bytes_discarded += actual - st.size_at_first_open;
                report
                    .truncated
                    .push((path.clone(), actual, st.size_at_first_open));
            }
            continue;
        }
        if opts.drop_unsynced_creates && st.created_at.is_some() && !st.dir_synced_after_create {
            let len = super::bytes::file_len(path);
            std::fs::remove_file(path)
                .unwrap_or_else(|e| panic!("power loss: remove {}: {e}", path.display()));
            report.bytes_discarded += len;
            report.removed.push(path.clone());
            continue;
        }
        if st.dirty.is_empty() && super::bytes::file_len(path) <= st.eof {
            continue;
        }

        let actual = super::bytes::file_len(path);
        let target = actual.min(st.eof);
        let safe = st.safe_prefix().min(target);

        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|e| panic!("power loss: open {}: {e}", path.display()));

        match opts.tear {
            TearMode::Truncate => {
                if safe < actual {
                    f.set_len(safe).expect("power loss: set_len");
                    report.bytes_discarded += actual - safe;
                    report.truncated.push((path.clone(), actual, safe));
                }
            }
            TearMode::Zero | TearMode::Garbage => {
                if target < actual {
                    f.set_len(target).expect("power loss: set_len");
                    report.bytes_discarded += actual - target;
                    report.truncated.push((path.clone(), actual, target));
                }
                if safe < target {
                    let len = (target - safe) as usize;
                    let fill = if opts.tear == TearMode::Zero {
                        vec![0u8; len]
                    } else {
                        garbage(opts.seed ^ safe, len)
                    };
                    f.seek(SeekFrom::Start(safe)).expect("power loss: seek");
                    f.write_all(&fill).expect("power loss: write");
                    report.bytes_discarded += len as u64;
                    report.torn.push((path.clone(), safe, len as u64));
                }
            }
            TearMode::TornSector => {
                let sector = opts.sector_bytes.max(1);
                let sector_end = safe.div_ceil(sector) * sector;
                let keep = sector_end.min(target);
                if keep < actual {
                    f.set_len(keep).expect("power loss: set_len");
                    report.bytes_discarded += actual - keep;
                    report.truncated.push((path.clone(), actual, keep));
                }
                if safe < keep {
                    let len = (keep - safe) as usize;
                    let fill = garbage(opts.seed ^ safe, len);
                    f.seek(SeekFrom::Start(safe)).expect("power loss: seek");
                    f.write_all(&fill).expect("power loss: write");
                    report.bytes_discarded += len as u64;
                    report.torn.push((path.clone(), safe, len as u64));
                }
            }
        }
        f.sync_all().expect("power loss: sync");
    }

    report
}

/// The last-resort model, for a platform where the `LD_PRELOAD` shim
/// cannot run.
///
/// # This is WEAKER than [`simulate_power_loss`] and downstream tests must
/// say so
///
/// It does not observe what regolith actually synced. It encodes an assumption
/// about regolith's behaviour into the test: that under
/// `DurabilityMode::Eventual` the WAL is only made durable on rotation and
/// on close, so the tail of the newest WAL past `synced_len` is unsynced.
/// If regolith's sync policy changes, this model silently keeps passing while
/// testing the wrong thing. Use it only when
/// [`super::shim::available`] is false, and print the warning it returns.
pub fn simulate_power_loss_modelled(dir: &Path, unsynced_tail_bytes: u64) -> PowerLossReport {
    let mut report = PowerLossReport {
        tear: Some(TearMode::Truncate),
        ..Default::default()
    };
    eprintln!(
        "WARNING: simulate_power_loss_modelled is the assumption-encoding fallback. \
         It does not observe fsync boundaries. Results are weaker than a shim-recorded run."
    );
    for wal in super::bytes::find_wals(dir) {
        let len = super::bytes::file_len(&wal);
        let keep = len.saturating_sub(unsynced_tail_bytes);
        if keep < len {
            super::bytes::truncate_at(&wal, keep);
            report.bytes_discarded += len - keep;
            report.truncated.push((wal, len, keep));
        }
    }
    report
}
