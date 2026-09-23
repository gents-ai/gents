//! `<run dir>/progress.json`: which slots are in flight and at which stage
//! (spec 4b §6). Ephemeral and advisory: the loop and, through
//! [`StageProgress`], an executor write it; only `gents eval watch` reads it.
//! It is never evidence, never an anchor or digest input and never a
//! document. Every write replaces the file atomically, so a reader sees one
//! whole state or the previous one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROGRESS_FILE: &str = "progress.json";

/// One slot a process is running now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub cell_id: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
    /// The stage the trial is in; `None` before its first stage and between
    /// stages.
    pub stage_id: Option<String>,
    /// RFC 3339: when the slot, or its current stage, started.
    pub started_at: String,
    /// The process hosting the slot, so a reader can tell a live entry from
    /// one a stopped process left behind.
    pub pid: u32,
    /// RFC 3339 with milliseconds: when this entry was last rewritten. The
    /// loop refreshes it on its marker timer, so an entry that stops
    /// being refreshed belongs to a process that stopped.
    pub written_at: String,
}

/// The whole file: the in-flight slots, keyed by trial id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub slots: BTreeMap<String, InFlight>,
}

/// The file under `run_dir`, or `None` when it is absent or unreadable; the
/// watcher then shows finished slots only.
pub fn read_progress(run_dir: &Path) -> Option<Progress> {
    let bytes = std::fs::read(run_dir.join(PROGRESS_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether process `pid` still exists. Signal 0 delivers nothing
/// and only checks; `EPERM` means it exists under another user.
#[cfg(unix)]
pub fn host_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 sends no signal; it only checks `pid`.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a portable liveness check, age alone decides.
#[cfg(not(unix))]
pub fn host_alive(_pid: u32) -> bool {
    true
}

/// An entry a live process refreshed within `window`.
/// An unreadable `written_at` is not fresh; one from the future is.
pub fn is_fresh(slot: &InFlight, window: Duration) -> bool {
    let Ok(written) = chrono::DateTime::parse_from_rfc3339(&slot.written_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(written.with_timezone(&chrono::Utc));
    host_alive(slot.pid) && age.to_std().map_or(true, |age| age <= window)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn now_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The loop's writer: one per pass over a run, shared by its trials.
#[derive(Debug)]
pub(crate) struct ProgressWriter {
    path: PathBuf,
    state: Mutex<Progress>,
    last_heartbeat: Mutex<Option<Instant>>,
}

impl ProgressWriter {
    /// Start the pass with an empty file.
    pub(crate) fn new(run_dir: &Path) -> Arc<Self> {
        let writer = Arc::new(Self {
            path: run_dir.join(PROGRESS_FILE),
            state: Mutex::new(Progress::default()),
            last_heartbeat: Mutex::new(None),
        });
        writer.update(|_| {});
        writer
    }

    /// Record a slot this process now hosts; stamps `pid` and `written_at`.
    pub(crate) fn slot_started(&self, trial_id: &str, mut in_flight: InFlight) {
        in_flight.pid = std::process::id();
        in_flight.written_at = now_millis();
        self.update(|progress| {
            progress.slots.insert(trial_id.to_owned(), in_flight);
        });
    }

    /// Refresh every entry's `written_at`: the loop is alive and its slots
    /// are still in flight. Called on the loop's marker timer; it writes at
    /// most once per `min_gap`, so a fast timer does not rewrite the file on
    /// every tick.
    pub(crate) fn heartbeat(&self, min_gap: Duration) {
        {
            let mut last = self
                .last_heartbeat
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|at| at.elapsed() < min_gap) {
                return;
            }
            *last = Some(Instant::now());
        }
        let idle = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slots
            .is_empty();
        if idle {
            return;
        }
        self.update(|progress| {
            let written_at = now_millis();
            for slot in progress.slots.values_mut() {
                slot.written_at = written_at.clone();
            }
        });
    }

    pub(crate) fn slot_ended(&self, trial_id: &str) {
        self.update(|progress| {
            progress.slots.remove(trial_id);
        });
    }

    fn stage(&self, trial_id: &str, stage_id: Option<&str>) {
        self.update(|progress| {
            if let Some(slot) = progress.slots.get_mut(trial_id) {
                slot.stage_id = stage_id.map(str::to_owned);
                slot.started_at = now();
                slot.written_at = now_millis();
            }
        });
    }

    /// Apply `change` and rewrite the file while holding the lock, so two
    /// trials' writes never interleave.
    fn update(&self, change: impl FnOnce(&mut Progress)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut state);
        if let Err(error) = write_atomically(&self.path, &state) {
            tracing::warn!(
                error = %format!("{error:#}"),
                path = %self.path.display(),
                "eval run progress was not recorded"
            );
        }
    }
}

fn write_atomically(path: &Path, progress: &Progress) -> Result<()> {
    // Never created here: a run directory `gents eval rm --force` removed
    // under a live loop stays removed, and the write fails and is logged.
    let dir = path.parent().context("progress.json has a run directory")?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("staging {}", path.display()))?;
    serde_json::to_writer_pretty(&mut staged, progress).context("encoding progress")?;
    staged
        .persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// What an executor holds to report stage boundaries. The default reports
/// nothing, so executors and specs built outside the loop need no writer.
#[derive(Clone, Default)]
pub struct StageProgress {
    sink: Option<(Arc<ProgressWriter>, String)>,
}

impl StageProgress {
    pub(crate) fn for_trial(writer: &Arc<ProgressWriter>, trial_id: &str) -> Self {
        Self {
            sink: Some((writer.clone(), trial_id.to_owned())),
        }
    }

    pub fn stage_started(&self, stage_id: &str) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage(trial_id, Some(stage_id));
        }
    }

    pub fn stage_ended(&self, _stage_id: &str) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage(trial_id, None);
        }
    }
}

impl std::fmt::Debug for StageProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StageProgress")
    }
}

/// Reporting is not part of what a trial is: two specs that differ only in
/// where they report are the same spec.
impl PartialEq for StageProgress {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for StageProgress {}
#[cfg(test)]
mod tests {
    use super::*;

    fn in_flight(stage_id: Option<&str>) -> InFlight {
        InFlight {
            cell_id: "base".into(),
            case_id: "case-a".into(),
            trial_index: 0,
            attempt: 1,
            stage_id: stage_id.map(str::to_owned),
            started_at: "2026-09-22T00:00:00Z".into(),
            pid: 0,
            written_at: String::new(),
        }
    }

    #[test]
    fn a_writer_records_slots_and_stages_and_forgets_ended_slots() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        assert_eq!(read_progress(dir.path()), Some(Progress::default()));

        writer.slot_started("t1", in_flight(None));
        let started = read_progress(dir.path()).unwrap().slots["t1"].clone();
        assert_eq!(
            started.pid,
            std::process::id(),
            "slot_started stamps the host"
        );
        assert!(!started.written_at.is_empty());
        let stages = StageProgress::for_trial(&writer, "t1");
        stages.stage_started("check");
        let during = read_progress(dir.path()).unwrap();
        assert_eq!(during.slots["t1"].stage_id.as_deref(), Some("check"));

        stages.stage_ended("check");
        assert_eq!(
            read_progress(dir.path()).unwrap().slots["t1"].stage_id,
            None
        );

        writer.slot_ended("t1");
        assert!(read_progress(dir.path()).unwrap().slots.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(PROGRESS_FILE)]);
    }

    /// `gents eval rm --force` may remove a run directory under a live
    /// loop; the loop's next write must not bring it back.
    #[test]
    fn a_write_into_a_removed_run_directory_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-1");
        std::fs::create_dir(&run_dir).unwrap();
        let writer = ProgressWriter::new(&run_dir);
        std::fs::remove_dir_all(&run_dir).unwrap();

        writer.slot_started("t1", in_flight(None));
        writer.heartbeat(Duration::ZERO);
        writer.slot_ended("t1");
        assert!(!run_dir.exists(), "the removed run directory stays removed");
        ProgressWriter::new(&dir.path().join("absent"));
        assert!(!dir.path().join("absent").exists(), "nor does a new writer");
    }

    #[test]
    fn a_default_handle_reports_nothing_and_is_equal_to_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        StageProgress::default().stage_started("check");
        assert_eq!(read_progress(dir.path()), Some(Progress::default()));
        assert_eq!(
            StageProgress::default(),
            StageProgress::for_trial(&writer, "t1")
        );
        assert_eq!(read_progress(&dir.path().join("absent")), None);
    }

    #[test]
    fn this_process_is_alive_and_an_impossible_pid_is_not() {
        assert!(host_alive(std::process::id()));
        assert!(!host_alive(0));
        assert!(!host_alive(u32::MAX), "never kill(-1, 0)");
        let mut slot = in_flight(None);
        slot.pid = std::process::id();
        slot.written_at = now_millis();
        assert!(is_fresh(&slot, Duration::from_secs(3)));
        slot.written_at = "2026-01-01T00:00:00.000Z".into();
        assert!(!is_fresh(&slot, Duration::from_secs(3)), "too old");
        slot.written_at = "not a time".into();
        assert!(!is_fresh(&slot, Duration::from_secs(3)));
    }

    #[test]
    fn a_heartbeat_writes_at_most_once_per_gap() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        writer.slot_started("t1", in_flight(None));
        let written = || {
            read_progress(dir.path()).unwrap().slots["t1"]
                .written_at
                .clone()
        };
        writer.heartbeat(Duration::from_secs(60));
        let first = written();
        std::thread::sleep(Duration::from_millis(5));
        writer.heartbeat(Duration::from_secs(60));
        assert_eq!(written(), first, "inside the gap nothing is written");
        writer.heartbeat(Duration::ZERO);
        assert!(written() > first);
    }
}
