//! Adversarial reclamation probes for the kovan-backed read view.
//!
//! Every `LiveSst` in a `Version` owns an open file descriptor, and a
//! published `ReadView` owns the `Arc<Version>`. Replacing a view now
//! retires it to kovan instead of dropping it, so these probes ask the
//! only question that matters for a storage engine: when does the
//! descriptor actually close, and does the unlinked inode's disk space
//! actually come back?
//!
//! Deterministic: every count below is a syscall-observable fact
//! (`/proc/self/fd`), never a timing loop.
//!
//! kovan reclamation is process-global: a batch is freed when every
//! reservation slot that could still reach it releases, and a thread
//! keeps its slot published after its last guard drops. So any thread
//! in the process that pinned and then idled delays this database's
//! retired views, even though the descriptor count below is scoped to
//! one temporary directory. These probes therefore run as a single
//! sequential test rather than five parallel ones: under the default
//! harness the sibling test threads are themselves idle pinners and
//! would gate what is being measured, turning a real property into a
//! reading of the harness.

// The whole file measures descriptors through `/proc/self/fd`, and its
// subject is the POSIX behaviour where an unlinked file keeps its bytes
// until the last descriptor closes. Windows has neither: there is no
// procfs to read, so every count comes back zero and the assertions
// become vacuous, and a file unlinked with a handle open stays
// delete-pending rather than becoming an unlinked-but-open inode.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use regolith::{Db, Options};
use tempfile::TempDir;

/// Descriptors this process still holds inside `root`, split into
/// (still-linked, unlinked-but-open).
fn bytes_of(entries: &[String]) -> u64 {
    entries
        .iter()
        .filter_map(|e| {
            e.rsplit_once(" [")?
                .1
                .strip_suffix(" bytes]")?
                .parse::<u64>()
                .ok()
        })
        .sum()
}

fn open_fds_under(root: &Path) -> (Vec<String>, Vec<String>) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root = root.to_string_lossy().to_string();
    let mut live = Vec::new();
    let mut deleted = Vec::new();
    let entries = match std::fs::read_dir("/proc/self/fd") {
        Ok(e) => e,
        Err(_) => return (live, deleted),
    };
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy().to_string();
        if !target.starts_with(&root) {
            continue;
        }
        let bytes = std::fs::metadata(entry.path())
            .map(|m| m.len())
            .unwrap_or(0);
        if target.ends_with(" (deleted)") {
            deleted.push(format!("{target} [{bytes} bytes]"));
        } else {
            live.push(format!("{target} [{bytes} bytes]"));
        }
    }
    (live, deleted)
}

fn fill(db: &Db, rounds: usize, per_round: usize, tag: u8) {
    for r in 0..rounds {
        for i in 0..per_round {
            let key = format!("k{:06}", i).into_bytes();
            let value = vec![tag.wrapping_add(r as u8); 256];
            regolith::through_stalls(|| db.put(&key, &value)).expect("put");
        }
    }
}

/// A `Version` replaced by a compaction owns the last `Arc<LiveSst>` of
/// every input file, and compaction has already unlinked those files.
/// If the retired view is not reclaimed, the inodes stay allocated:
/// disk space that `df` still counts and that nothing can free.
fn unlinked_sstable_inodes_are_released_after_a_compaction() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open");

    fill(&db, 8, 400, b'a');
    db.compact_range(None, None).wait().expect("compact");

    // Reclamation is RCU: the retired view is freed when the last
    // reader releases it, and a background compaction pass can still
    // be holding one the instant `compact_range` returns. Asserting
    // immediately makes the outcome depend on how loaded the machine
    // is, which is a flaky test rather than a property. A deadline
    // with bounded backoff keeps it exact: a fast machine finishes on
    // the first probe, a loaded one still gets a true answer, and a
    // real leak never clears and still fails.
    let (live, deleted) = wait_for_reclamation(dir.path());
    eprintln!(
        "after compact_range: {} live fds, {} unlinked-but-open fds holding {} bytes",
        live.len(),
        deleted.len(),
        bytes_of(&deleted),
    );
    assert!(
        deleted.is_empty(),
        "{} unlinked SSTable inodes are still pinned open {:?} after a \
         synchronous compaction returned; their disk space cannot be \
         reclaimed",
        deleted.len(),
        RECLAIM_DEADLINE,
    );
}

/// How long a retired view may take to be reclaimed before the leak is
/// real rather than merely late.
const RECLAIM_DEADLINE: Duration = Duration::from_secs(30);

/// Poll until no unlinked-but-open descriptor remains, or the deadline
/// passes. Returns the last observation either way, so the caller
/// reports what it actually saw.
fn wait_for_reclamation(dir: &Path) -> (Vec<String>, Vec<String>) {
    let start = Instant::now();
    let mut backoff = Duration::from_millis(1);
    loop {
        let seen = open_fds_under(dir);
        if seen.1.is_empty() || start.elapsed() >= RECLAIM_DEADLINE {
            return seen;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(100));
    }
}

/// The database object is gone. Every descriptor it opened must be
/// gone with it. A retired view outliving the `Db` that published it
/// leaks descriptors past the lifetime of the handle the caller owns.
fn dropping_the_db_closes_every_descriptor_it_opened() {
    let dir = TempDir::new().expect("tempdir");
    {
        let db =
            Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open");
        fill(&db, 8, 400, b'b');
        db.compact_range(None, None).wait().expect("compact");
    }

    let (live, deleted) = wait_for_reclamation(dir.path());
    eprintln!(
        "after drop(db): {} live fds, {} unlinked-but-open fds",
        live.len(),
        deleted.len()
    );
    eprintln!("  bytes pinned by unlinked inodes: {}", bytes_of(&deleted));
    assert!(
        live.is_empty() && deleted.is_empty(),
        "{} descriptors under the database directory are still open \
         after the Db was dropped",
        live.len() + deleted.len(),
    );
}

/// Causation probe. If the pinned inodes above are kovan's deferred
/// reclamation and nothing else, then forcing the publishing thread to
/// drain its retired batch must release them. `compact_range` rotates
/// the memtable first, and `retire_oldest_frozen` calls
/// `kovan::flush()` on the calling thread, so a second `compact_range`
/// from this same thread drains what the first one retired.
fn a_second_publication_from_the_same_thread_releases_the_pinned_inodes() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open");

    fill(&db, 8, 400, b'c');
    db.compact_range(None, None).wait().expect("compact");
    let (_, first) = open_fds_under(dir.path());

    let mut counts = vec![first.len()];
    let drain_rounds: usize = std::env::var("REGOLITH_DRAIN_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    for round in 0..drain_rounds {
        regolith::through_stalls(|| db.put(format!("drain{round}").as_bytes(), b"x")).expect("put");
        db.compact_range(None, None).wait().expect("compact");
        let (_, d) = open_fds_under(dir.path());
        counts.push(d.len());
    }
    eprintln!("pinned unlinked inodes per round: {counts:?}");
    assert_eq!(
        counts.last().copied(),
        Some(0),
        "pinned unlinked inodes never returned to zero across {drain_rounds} \
         further publications from the same thread: {counts:?}",
    );
}

/// Decisive attribution probe. kovan reclaims a thread's retired batch
/// when that thread exits. If running the whole database lifecycle on a
/// spawned thread and joining it releases the descriptors that the same
/// lifecycle on the main thread does not, then the holder is a retired
/// `ReadView` sitting in the publishing thread's kovan batch, not any
/// structure regolith still owns.
fn descriptors_survive_the_db_but_not_the_publishing_thread() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().to_path_buf();

    let handle = std::thread::spawn(move || {
        let db = Db::open(&path, Options::default().write_buffer_size(4 * 1024)).expect("open");
        fill(&db, 8, 400, b'd');
        db.compact_range(None, None).wait().expect("compact");
        drop(db);
        let (live, deleted) = open_fds_under(&path);
        (live.len(), deleted.len())
    });
    let (live_before_join, deleted_before_join) = handle.join().expect("join");

    let (live_after, deleted_after) = wait_for_reclamation(dir.path());
    eprintln!(
        "on the publishing thread after drop(db): {live_before_join} live, \
         {deleted_before_join} unlinked-but-open"
    );
    eprintln!(
        "after that thread exited: {} live, {} unlinked-but-open",
        live_after.len(),
        deleted_after.len()
    );
    assert_eq!(
        (live_after.len(), deleted_after.len()),
        (0, 0),
        "descriptors outlive even the thread that published the views"
    );
}

/// Is the retention bounded or proportional to the workload? A bounded
/// holder (a fixed-size retired batch) plateaus; a holder that keeps
/// every obsolete reader grows with the number of files created.
fn pinned_inode_count_versus_workload_size() {
    let mut rows = Vec::new();
    for rounds in [4usize, 8, 16, 32] {
        let dir = TempDir::new().expect("tempdir");
        let db =
            Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open");
        fill(&db, rounds, 400, b'e');
        db.compact_range(None, None).wait().expect("compact");
        let (live, deleted) = open_fds_under(dir.path());
        let on_disk = std::fs::read_dir(dir.path().join("sst"))
            .map(|d| d.flatten().count())
            .unwrap_or(0);
        rows.push((rounds, live.len(), deleted.len(), on_disk));
    }
    for (rounds, live, deleted, on_disk) in &rows {
        eprintln!(
            "rounds={rounds:>3}  live_fds={live:>4}  pinned_unlinked={deleted:>4}  \
             files_still_on_disk={on_disk:>4}"
        );
    }
    let first = rows.first().expect("rows").2;
    let last = rows.last().expect("rows").2;
    assert!(
        last <= first * 2,
        "pinned unlinked inodes scale with the workload: {rows:?}",
    );
}

/// The bound on a thread that reads once and then parks forever.
///
/// Such a thread keeps its kovan reservation slot published, so it
/// gates the retired views born at or before its last pin. The bound
/// is that the hold is one-time: views published after it went idle
/// are younger than its slot's epoch, are skipped by the reclaimer's
/// scan, and are released on schedule. Without that bound one parked
/// pool thread would pin the engine's descriptors for the life of the
/// process.
fn an_idle_reader_thread_does_not_gate_descriptors_created_after_it_idled() {
    let dir = TempDir::new().expect("tempdir");
    let db = std::sync::Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open"),
    );
    regolith::through_stalls(|| db.put(b"seed", b"v")).expect("put");

    let parked = std::sync::Arc::new(std::sync::Barrier::new(2));
    let forever = std::sync::Arc::new(std::sync::Barrier::new(2));
    let idler = {
        let (db, parked, forever) = (std::sync::Arc::clone(&db), parked.clone(), forever.clone());
        std::thread::spawn(move || {
            // One read registers this thread as a kovan participant.
            let _ = db.get(b"seed").expect("get");
            parked.wait();
            forever.wait();
        })
    };
    parked.wait();

    // Everything below is born after the idler's last pin.
    fill(&db, 8, 400, b'f');
    db.compact_range(None, None).wait().expect("compact");

    let (_, deleted) = wait_for_reclamation(dir.path());
    eprintln!(
        "with one parked reader thread alive: {} unlinked-but-open fds",
        deleted.len()
    );
    let pinned = deleted.len();
    forever.wait();
    idler.join().expect("join");
    assert_eq!(
        pinned, 0,
        "a thread that read once and parked gated {pinned} descriptors \
         created after it went idle, so its retention is cumulative \
         rather than a one-time hold",
    );
}

/// Every probe above, in one thread, so the only kovan participants in
/// the process are this thread and the ones the engine itself spawns.
#[test]
fn no_obsolete_sstable_descriptor_outlives_the_publication_that_dropped_it() {
    unlinked_sstable_inodes_are_released_after_a_compaction();
    dropping_the_db_closes_every_descriptor_it_opened();
    a_second_publication_from_the_same_thread_releases_the_pinned_inodes();
    descriptors_survive_the_db_but_not_the_publishing_thread();
    pinned_inode_count_versus_workload_size();
    an_idle_reader_thread_does_not_gate_descriptors_created_after_it_idled();
}
