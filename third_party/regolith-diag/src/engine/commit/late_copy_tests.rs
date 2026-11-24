//! A record's late copy and the sync, under the real fault shim
//! (`wal_gate.rs`, WG4): no write lands on a range of the log after a sync
//! that covers it has begun.
//!
//! An owner stops inside its record's write, or just before it, through
//! the pipeline's test hooks. Alone, it takes the fast path (`write_unit.rs`,
//! WU8): its write is a copy through the gate and it holds no claim on the
//! write unit. A helper behind it writes the same record, in a run of its
//! own that begins with it, and a sync wants the log durable. The workload runs in
//! a child process, this test binary run again with the `LD_PRELOAD` shim
//! of `tests/common/fault`, which journals every write and sync of the log
//! in the order they reached the host. The parent reads the journal: a write
//! after a sync of the log to a range written before that sync is the
//! defect, whatever bytes it carried, since a sector in flight when the
//! power goes may come back garbled.
//!
//! Threads are stopped and released through channels, and every wait has a
//! deadline; nothing is timed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::hook::{self, Stage};
use super::pipeline_tests::{durable_put, eventual_put};
use super::*;
use crate::engine::EngineOptions;

#[allow(dead_code)]
#[path = "../../../tests/common/fault/journal.rs"]
mod journal;
#[allow(dead_code)]
#[path = "../../../tests/common/fault/shim.rs"]
mod shim;

use journal::{Journal, OpKind};

const DEADLINE: Duration = Duration::from_secs(30);
/// The child's whole run: an engine opened, three commits, a close.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);
const SCENARIO: &str = "REGOLITH_LATE_COPY";
const CHILD_DIR: &str = "REGOLITH_LATE_COPY_DIR";

/// Where the owner stops, and what runs the sync.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    /// Inside its write, counted in at the gate; a durable commit behind it
    /// writes the record and syncs the log.
    InWrite,
    /// Before its write, past its claim of it; a durable commit behind it
    /// writes the record and syncs the log, and the owner goes on after.
    BeforeWrite,
    /// Inside its write; a commit behind it writes and publishes the record,
    /// and a rotation syncs the log before it seals it.
    Rotation,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::InWrite => "in-write",
            Scenario::BeforeWrite => "before-write",
            Scenario::Rotation => "rotation",
        }
    }

    fn parse(name: &str) -> Self {
        match name {
            "in-write" => Scenario::InWrite,
            "before-write" => Scenario::BeforeWrite,
            "rotation" => Scenario::Rotation,
            other => panic!("unknown scenario {other}"),
        }
    }

    fn owner_stops_at(self) -> Stage {
        match self {
            Scenario::BeforeWrite => Stage::Write,
            Scenario::InWrite | Scenario::Rotation => Stage::InWrite,
        }
    }
}

/// The child's side: returns at once unless this process is the child.
#[test]
fn late_copy_child() {
    let Ok(name) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(CHILD_DIR).expect("the child's directory"));
    run_scenario(Scenario::parse(&name), &dir);
}

fn run_scenario(scenario: Scenario, dir: &Path) {
    // No worker: nothing syncs the log but the threads below.
    let engine = RegolithEngine::open(
        dir,
        EngineOptions {
            max_background_compactions: 0,
            ..EngineOptions::default()
        },
    )
    .expect("engine open");

    let (stopped, owner_stopped) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let owner = {
        let engine = Arc::clone(&engine);
        let at = scenario.owner_stops_at();
        std::thread::spawn(move || {
            let mut fired = false;
            hook::set(move |stage| {
                if stage == at && !fired {
                    fired = true;
                    stopped.send(()).unwrap();
                    let _ = released.recv_timeout(DEADLINE);
                }
            });
            let out = engine.submit(eventual_put(b"owner", b"v"));
            hook::clear();
            out
        })
    };
    owner_stopped
        .recv_timeout(DEADLINE)
        .expect("the owner never reached its write");

    // What the sync's thread says first: that it waits for the owner's
    // write, or that its call returned.
    let (said, heard) = mpsc::channel();
    let syncer = {
        let engine = Arc::clone(&engine);
        let waits = said.clone();
        std::thread::spawn(move || {
            hook::set(move |stage| {
                if stage == Stage::SyncWait {
                    let _ = waits.send("waits");
                }
            });
            let out = match scenario {
                Scenario::InWrite | Scenario::BeforeWrite => {
                    engine.submit(durable_put(b"helper", b"v")).map(|_| ())
                }
                Scenario::Rotation => engine
                    .submit(eventual_put(b"helper", b"v"))
                    .map(|_| ())
                    .and_then(|()| engine.seal_active().map(|_| ())),
            };
            hook::clear();
            let _ = said.send("returned");
            out
        })
    };
    let first = heard
        .recv_timeout(DEADLINE)
        .expect("the sync neither waited nor returned");
    if scenario == Scenario::BeforeWrite {
        assert_eq!(
            first, "returned",
            "nothing of the owner's is in flight, so the sync does not wait"
        );
    }
    release.send(()).unwrap();
    syncer
        .join()
        .unwrap()
        .expect("the helper's commit and its sync");
    owner
        .join()
        .unwrap()
        .expect("the owner's commit, written by the helper");
    drop(engine);
}

/// Run `scenario` in a child under the shim, and return its journal.
fn journal_of(scenario: Scenario) -> Journal {
    let lib = shim::require();
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    let journal_path = tmp.path().join("journal");
    let out = tmp.path().join("child.out");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "engine::commit::late_copy_tests::late_copy_child",
        ])
        .env(SCENARIO, scenario.name())
        .env(CHILD_DIR, &db)
        .env("LD_PRELOAD", shim::preload_value(&lib))
        .env("REGOLITH_FAULT_JOURNAL", &journal_path)
        .env("REGOLITH_FAULT_ROOT", format!("{}/", db.display()))
        .env_remove("REGOLITH_FAULT_DIE_KIND")
        .env_remove("REGOLITH_FAULT_DIE_PATH")
        .env_remove("REGOLITH_FAULT_DIE_NTH")
        .env_remove("REGOLITH_FAULT_DIE_WHEN")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&out).unwrap()))
        .stderr(Stdio::from(
            std::fs::File::create(tmp.path().join("child.err")).unwrap(),
        ))
        .spawn()
        .expect("spawn the child");
    let deadline = Instant::now() + CHILD_DEADLINE;
    let mut pause = Duration::from_micros(200);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the {scenario:?} child did not finish within {CHILD_DEADLINE:?}");
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    };
    let said = std::fs::read_to_string(&out).unwrap_or_default()
        + &std::fs::read_to_string(tmp.path().join("child.err")).unwrap_or_default();
    assert!(status.success(), "the {scenario:?} child failed:\n{said}");
    assert!(
        said.contains("1 passed"),
        "the {scenario:?} child ran no scenario:\n{said}"
    );
    let journal = Journal::read(&journal_path).unwrap();
    assert_eq!(
        journal.malformed, 0,
        "a journal line the parser cannot read"
    );
    journal
}

/// A range of a log, `(log, offset, length)`, and how many writes wrote it.
type Copies = Vec<((PathBuf, u64), usize)>;

/// What a journal shows of one log so far.
#[derive(Default)]
struct LogSeen {
    /// Ranges written since the last sync.
    pending: Vec<(u64, u64)>,
    /// Ranges written before a sync.
    synced: Vec<(u64, u64)>,
    last_sync: Option<u64>,
}

/// Every write of a log the journal shows landing after a sync of the log
/// on a range written before that sync; and the ranges written more than
/// once, each with how often.
fn late_writes(journal: &Journal) -> (Vec<String>, Copies) {
    let overlaps = |ranges: &[(u64, u64)], offset: u64, len: u64| {
        ranges
            .iter()
            .any(|&(s, n)| s < offset + len && offset < s + n)
    };
    let mut late = Vec::new();
    let mut copies = std::collections::BTreeMap::new();
    let mut logs = std::collections::BTreeMap::<PathBuf, LogSeen>::new();
    for r in &journal.records {
        if !r.succeeded() || !r.path.to_string_lossy().ends_with(".log") {
            continue;
        }
        let log = logs.entry(r.path.clone()).or_default();
        match r.kind {
            OpKind::Write if r.a >= 0 => {
                let (offset, len) = (r.a as u64, r.written());
                // Keyed by where a write starts: a run that begins with a
                // record writes that record again, whatever follows it.
                *copies.entry((r.path.clone(), offset)).or_insert(0usize) += 1;
                if let Some(sync_seq) = log.last_sync
                    && overlaps(&log.synced, offset, len)
                {
                    late.push(format!(
                        "{}: {len} bytes at {offset}, written at {}, after the sync at {sync_seq}",
                        r.path.display(),
                        r.seq
                    ));
                }
                log.pending.push((offset, len));
            }
            OpKind::Sync => {
                let pending = std::mem::take(&mut log.pending);
                log.synced.extend(pending);
                log.last_sync = Some(r.seq);
            }
            OpKind::Truncate => {
                let end = r.a.max(0) as u64;
                log.synced.retain(|&(s, n)| s + n <= end);
                log.pending.retain(|&(s, n)| s + n <= end);
            }
            _ => {}
        }
    }
    let copies = copies.into_iter().filter(|&(_, n)| n > 1).collect();
    (late, copies)
}

fn check(scenario: Scenario) -> Copies {
    let journal = journal_of(scenario);
    let (late, copies) = late_writes(&journal);
    assert!(
        late.is_empty(),
        "{scenario:?}: a write landed on a range of the log after a sync that covered it began: \
         {late:?}\n{journal}"
    );
    copies
}

#[test]
fn a_late_copy_from_inside_the_write_returns_before_the_sync() {
    let copies = check(Scenario::InWrite);
    assert_eq!(
        copies.len(),
        1,
        "the helper wrote a copy of the owner's record: {copies:?}"
    );
}

#[test]
fn an_owner_that_finds_its_record_written_writes_nothing() {
    let copies = check(Scenario::BeforeWrite);
    assert!(copies.is_empty(), "no record is written twice: {copies:?}");
}

#[test]
fn a_rotation_syncs_its_log_only_after_a_late_copy_returned() {
    let copies = check(Scenario::Rotation);
    assert_eq!(
        copies.len(),
        1,
        "the helper wrote a copy of the owner's record: {copies:?}"
    );
}
