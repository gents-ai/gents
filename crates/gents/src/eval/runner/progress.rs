//! `<run dir>/progress.json`: which slots are in flight and at which stage
//! (spec 4b §6). Ephemeral and advisory: never evidence, never an anchor or
//! digest input and never a document. Every write replaces the file
//! atomically, so a reader sees one whole state or the previous one.
//!
//! An entry may carry the trial's [`LiveSnapshot`], which the executor
//! reports while the trial runs, and the case's [`GoalEntry`] list, so a
//! watcher sees what the trial's home holds without opening it: a running
//! trial's store is locked by the process hosting it. Both fields are
//! optional, so a file written by an older runner still reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::eval::runner::goal::GoalEntry;

use super::files::write_json_atomically;

pub const PROGRESS_FILE: &str = "progress.json";

/// What a trial's home held at one moment: its usage, its tool calls and how
/// many documents of each observed collection it holds. Observational only:
/// never evidence, never graded, never part of a digest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSnapshot {
    /// RFC 3339: when the home was read.
    pub observed_at: String,
    #[serde(default)]
    pub stages: BTreeMap<String, serde_json::Value>,
    /// Since the trial's first stage started.
    pub elapsed_secs: u64,
    /// Public requests in the trial's home, the stages' own and every one
    /// they caused.
    pub requests: u64,
    /// One per model turn.
    pub model_turns: u64,
    /// `None` once one call did not report.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Reported usage remains a lower bound while any call lacks usage;
    /// observers must label these bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_contexts: Option<Vec<SessionContextUsage>>,
    pub tool_calls: u64,
    /// Tool calls that ended failed or timed out.
    pub failed_tool_calls: u64,
    /// By tool name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolTally>,
    /// The tool call that started last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<LastToolCall>,
    /// Rows per collection: the configuration collections, every collection
    /// a capture reads, and every schema registered during the trial.
    #[serde(default)]
    pub documents: BTreeMap<String, u64>,
    /// Rows each documents capture's filter matches now, by capture name.
    #[serde(default)]
    pub captures: BTreeMap<String, u64>,
    /// Collections registered in the home after the pack and fixtures were
    /// installed: the schemas the subject registered.
    #[serde(default)]
    pub schemas: Vec<String>,
}

/// Last dispatched inference input, not cumulative billing or the transcript
/// after the final response. Estimates come from the owned loop's accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextUsage {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub last_prompt_tokens: Option<u64>,
    pub peak_prompt_tokens: Option<u64>,
    pub last_estimated_input_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub session_compactions: u64,
    pub provider_reductions: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastToolCall {
    pub tool_name: String,
    pub state: Option<String>,
    /// Cut to [`LastToolCall::RESULT_CHARS`].
    pub result: Option<String>,
}

impl LastToolCall {
    pub const RESULT_CHARS: usize = 160;

    pub fn failed(&self) -> bool {
        matches!(self.state.as_deref(), Some("failed" | "timedOut"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolTally {
    pub calls: u64,
    pub failed: u64,
}

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
    /// RFC 3339 with milliseconds: when this entry was last rewritten.
    pub written_at: String,
    /// The latest snapshot the executor reported; absent before the first
    /// and from an executor that reports none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal: Vec<GoalEntry>,
}

/// The process running the loop over a run, whether or not a slot is in
/// flight: between passes, during a backoff and before the first launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    /// RFC 3339 with milliseconds: when the loop last refreshed it.
    pub written_at: String,
}

impl Holder {
    /// Refreshed within `window` by a live process, by [`is_fresh`]'s rule.
    pub fn is_fresh(&self, window: Duration) -> bool {
        fresh(self.pid, &self.written_at, window)
    }
}

/// The whole file: the loop's holder and the in-flight slots, keyed by trial
/// id. A file written before the holder existed reads with none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<Holder>,
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
pub fn is_fresh(slot: &InFlight, window: Duration) -> bool {
    fresh(slot.pid, &slot.written_at, window)
}

fn fresh(pid: u32, written_at: &str, window: Duration) -> bool {
    let Ok(written) = chrono::DateTime::parse_from_rfc3339(written_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(written.with_timezone(&chrono::Utc));
    host_alive(pid) && age.to_std().map_or(true, |age| age <= window)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn now_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The loop's writer: one per `execute_frozen` call over a run, shared by
/// its trials.
#[derive(Debug)]
pub(crate) struct ProgressWriter {
    path: PathBuf,
    state: Mutex<Progress>,
    last_heartbeat: Mutex<Option<Instant>>,
}

impl ProgressWriter {
    /// Start the `execute_frozen` call with an empty file.
    pub(crate) fn new(run_dir: &Path) -> Arc<Self> {
        let writer = Arc::new(Self {
            path: run_dir.join(PROGRESS_FILE),
            state: Mutex::new(Progress::default()),
            last_heartbeat: Mutex::new(None),
        });
        writer.update(|_| {});
        writer
    }

    /// Mark this process as the run's holder until the guard drops. The loop
    /// takes it at entry; dropping the guard rewrites the file without it.
    pub(crate) fn hold(self: &Arc<Self>) -> HolderGuard {
        self.update(|progress| {
            progress.holder = Some(Holder {
                pid: std::process::id(),
                written_at: now_millis(),
            });
        });
        HolderGuard(self.clone())
    }

    /// Record a slot this process now hosts; stamps `pid` and `written_at`.
    pub(crate) fn slot_started(&self, trial_id: &str, mut in_flight: InFlight) {
        in_flight.pid = std::process::id();
        in_flight.written_at = now_millis();
        self.update(|progress| {
            progress.slots.insert(trial_id.to_owned(), in_flight);
        });
    }

    /// Refresh the holder's and every entry's `written_at`: the loop is
    /// alive and its slots are still in flight. Writes at most about once
    /// per `min_gap`: the throttle is three quarters of the gap, so a timer
    /// ticking once per gap is not halved.
    pub(crate) fn heartbeat(&self, min_gap: Duration) {
        let throttle = min_gap - min_gap / 4;
        {
            let mut last = self
                .last_heartbeat
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|at| at.elapsed() < throttle) {
                return;
            }
            *last = Some(Instant::now());
        }
        let idle = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.holder.is_none() && state.slots.is_empty()
        };
        if idle {
            return;
        }
        self.update(|progress| {
            let written_at = now_millis();
            if let Some(holder) = progress.holder.as_mut() {
                holder.written_at = written_at.clone();
            }
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

    fn live(&self, trial_id: &str, mut snapshot: LiveSnapshot) {
        self.update(|progress| {
            if let Some(slot) = progress.slots.get_mut(trial_id) {
                if let Some(previous) = &slot.live {
                    snapshot.stages.extend(previous.stages.clone());
                }
                slot.live = Some(snapshot);
                slot.written_at = now_millis();
            }
        });
    }

    fn stage_result(&self, trial_id: &str, stage_id: &str, result: serde_json::Value) {
        self.update(|progress| {
            if let Some(slot) = progress.slots.get_mut(trial_id) {
                slot.live
                    .get_or_insert_with(Default::default)
                    .stages
                    .insert(stage_id.into(), result);
            }
        });
    }

    /// The latest snapshot reported for a slot still in flight.
    pub(crate) fn live_of(&self, trial_id: &str) -> Option<LiveSnapshot> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.slots.get(trial_id).and_then(|slot| slot.live.clone())
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
        if let Err(error) = write_json_atomically(&self.path, &*state) {
            tracing::warn!(
                error = %format!("{error:#}"),
                path = %self.path.display(),
                "eval run progress was not recorded"
            );
        }
    }
}

/// Holds the run for the loop; dropping it clears the holder.
#[derive(Debug)]
pub(crate) struct HolderGuard(Arc<ProgressWriter>);

impl Drop for HolderGuard {
    fn drop(&mut self) {
        self.0.update(|progress| progress.holder = None);
    }
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

    pub fn stage_result(&self, stage_id: &str, result: serde_json::Value) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage_result(trial_id, stage_id, result);
        }
    }

    pub fn stage_ended(&self, _stage_id: &str) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage(trial_id, None);
        }
    }

    /// Whether anything reads what this handle reports, so an executor can
    /// skip observing a home nobody watches.
    pub fn attached(&self) -> bool {
        self.sink.is_some()
    }

    /// Replace the slot's snapshot. The last one reported before the trial
    /// ends is kept with its evidence record.
    pub fn live(&self, snapshot: LiveSnapshot) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.live(trial_id, snapshot);
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
            live: None,
            goal: Vec::new(),
        }
    }

    #[test]
    fn this_process_is_alive_and_an_impossible_pid_is_not() {
        assert!(host_alive(std::process::id()));
        assert!(!host_alive(0));
        assert!(!host_alive(u32::MAX), "never kill(-1, 0)");
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
}
