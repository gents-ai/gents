//! Ends a job row: maps a [`Settlement`] onto the row's terminal state and
//! canonical tool output.

mod context;
mod output;
mod rows;
mod settle;
mod step;
#[cfg(test)]
pub(crate) mod tests;

use rapidhash::{HashMapExt, RapidHashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio::sync::Semaphore;

use super::admit::{Refusal, StepRow};
use super::control::{
    kill_as, kill_decision, observe_live_children, KillCause, KillObservation, KillOutcome,
};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::PluginVerdict;
use crate::store::config_client::ConfigAccess;

pub(super) use context::step_context_for;
pub(super) use output::{guest_state_of, progress_of, scratch_of, settlement_of, writes_of};
pub(super) use rows::load_lifecycle;
#[cfg(test)]
pub(super) use rows::store_running_progress;
pub(crate) use rows::{read_step_row, read_step_row_in_txn};
pub(crate) use settle::settle_plugin_job_in_txn;
pub(super) use settle::{refusal_text, terminal_text};
pub use settle::{settle_plugin_job, Settlement};
pub use step::{run_step, StepOutcome};

/// The handles one job step needs on its own runtime. The runtime's single
/// `JobDriver` holds the one value; tests build it directly.
#[derive(Clone)]
pub struct JobRuntime {
    pub node: Arc<EmbeddedNode>,
    pub plugins: Arc<PluginExecutor>,
    /// The gents home plugins are installed under; empty when there is none.
    pub home: PathBuf,
    /// The operator tool root: step 0's working folder when the request names no workspace.
    pub tool_root: Option<PathBuf>,
}

/// A step invocation that failed: the failures that count towards
/// `plugin_job_step_failed`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepFailure {
    #[error("the guest trapped: {0}")]
    Trap(String),
    #[error("the guest ran out of its budget")]
    BudgetExhausted,
    #[error("the guest returned a malformed step output: {0}")]
    MalformedOutput(String),
    #[error("the plugin call did not run: {0}")]
    NotRun(String),
}

/// What a step failure means for the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureVerdict {
    /// The row stays running and its step runs again.
    Retry,
    /// The third consecutive failure: the row fails `plugin_job_step_failed`.
    Settle,
}

/// Lean `maxConsecutiveStepFailures`.
const MAX_CONSECUTIVE_STEP_FAILURES: u32 = 3;

/// The failure `verdict` of a plugin call is, if it is one.
fn step_failure(verdict: PluginVerdict, diagnostics: &str) -> Option<StepFailure> {
    match verdict {
        PluginVerdict::Success => None,
        PluginVerdict::OutOfFuel | PluginVerdict::OutOfMemory | PluginVerdict::Timeout => {
            Some(StepFailure::BudgetExhausted)
        }
        PluginVerdict::BadOutput => Some(StepFailure::MalformedOutput(diagnostics.to_owned())),
        PluginVerdict::Refused | PluginVerdict::Failed => {
            Some(StepFailure::Trap(diagnostics.to_owned()))
        }
    }
}

/// Lean `onInvocationFailure`: one more failed invocation of `row`. The
/// third in a row ends a running row `failed`, kills its live own children
/// as a stop does, and refuses `StepFailed`; before that nothing is killed.
pub fn step_failure_decision(
    row: &StepRow,
    observe: &dyn Fn(&str) -> KillObservation,
) -> (KillOutcome, Option<Refusal>) {
    let counted = StepRow {
        consecutive_failures: row.consecutive_failures + 1,
        ..row.clone()
    };
    if counted.consecutive_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
        let outcome = kill_decision(
            &counted,
            KillCause::FailClosed(Refusal::StepFailed),
            observe,
        );
        (outcome, Some(Refusal::StepFailed))
    } else {
        (
            KillOutcome {
                row: counted,
                won: false,
                killed: Vec::new(),
            },
            None,
        )
    }
}

/// Whether a wake came while the row's step was in flight.
#[derive(Debug, Default)]
struct Dirty(bool);

/// A row's consecutive step failures, held in memory (one active runtime per
/// node: a restart resets them), and the row and children's observations
/// the last failure read.
#[derive(Default)]
struct RowFailures {
    count: u32,
    read: Option<(StepRow, RapidHashMap<String, KillObservation>)>,
}

/// Runs job steps: one in flight per row, wakes during a step coalesced into
/// one re-run, and at most four steps at once.
pub struct JobDriver {
    runtime: JobRuntime,
    service_admission: crate::runtime_service::RuntimeServiceAdmission,
    this: Weak<JobDriver>,
    in_flight: Mutex<RapidHashMap<String, Dirty>>,
    permits: Semaphore,
    failures: Mutex<RapidHashMap<String, RowFailures>>,
    /// Every row ever kicked, for tests that assert a wake reached no row.
    #[cfg(test)]
    pub(crate) kicked: Mutex<Vec<String>>,
}

impl JobDriver {
    pub fn new(runtime: JobRuntime) -> Arc<Self> {
        Self::with_service_admission(runtime, Default::default())
    }

    pub(crate) fn with_service_admission(
        runtime: JobRuntime,
        service_admission: crate::runtime_service::RuntimeServiceAdmission,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            runtime,
            service_admission,
            this: this.clone(),
            in_flight: Mutex::new(RapidHashMap::new()),
            permits: Semaphore::new(4),
            failures: Mutex::new(RapidHashMap::new()),
            #[cfg(test)]
            kicked: Mutex::new(Vec::new()),
        })
    }

    /// The handles every step of this runtime runs on.
    pub fn runtime(&self) -> &JobRuntime {
        &self.runtime
    }

    pub(crate) fn in_flight_row_ids(&self) -> Vec<String> {
        self.in_flight
            .lock()
            .expect("in-flight rows lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Wakes the row: starts its step, or marks it dirty when one is in flight.
    pub fn kick(&self, row_doc_id: &str) {
        #[cfg(test)]
        self.kicked.lock().unwrap().push(row_doc_id.to_owned());
        let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
        if let Some(dirty) = in_flight.get_mut(row_doc_id) {
            dirty.0 = true;
            return;
        }
        let Ok(work) = self.service_admission.admit() else {
            return;
        };
        in_flight.insert(row_doc_id.to_owned(), Dirty::default());
        let driver = self.this.upgrade().expect("a driver is held by its Arc");
        let row_doc_id = row_doc_id.to_owned();
        tokio::spawn(async move { driver.drive(&row_doc_id, work).await });
    }

    /// Runs one step of the row now and returns what it did. `None` when a
    /// step of the row is already in flight: that step re-runs once for this
    /// call, as for a wake.
    pub async fn step_now(&self, row_doc_id: &str) -> Option<Result<StepOutcome>> {
        let work;
        {
            let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
            if let Some(dirty) = in_flight.get_mut(row_doc_id) {
                dirty.0 = true;
                return None;
            }
            work = match self.service_admission.admit() {
                Ok(work) => work,
                Err(error) => return Some(Err(error.into())),
            };
            in_flight.insert(row_doc_id.to_owned(), Dirty::default());
        }
        let (again, outcome) = self.step_once(row_doc_id).await;
        let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
        let dirty = std::mem::take(
            &mut in_flight
                .get_mut(row_doc_id)
                .expect("the row is in flight")
                .0,
        );
        if again || dirty {
            let driver = self.this.upgrade().expect("a driver is held by its Arc");
            let row_doc_id = row_doc_id.to_owned();
            tokio::spawn(async move { driver.drive(&row_doc_id, work).await });
        } else {
            in_flight.remove(row_doc_id);
        }
        Some(outcome)
    }

    /// Whether a step of the row is running or a hold is on it.
    pub fn is_stepping(&self, row_doc_id: &str) -> bool {
        self.in_flight
            .lock()
            .expect("in-flight rows lock")
            .contains_key(row_doc_id)
    }

    /// Runs `f` while no step of the row runs or can start, or returns `None`
    /// when one is in flight. A wake during `f` steps the row afterwards.
    pub async fn hold_steps<T>(
        &self,
        row_doc_id: &str,
        f: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let work;
        {
            let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
            if in_flight.contains_key(row_doc_id) {
                return None;
            }
            work = self.service_admission.admit().ok()?;
            in_flight.insert(row_doc_id.to_owned(), Dirty::default());
        }
        let out = f.await;
        let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
        if in_flight.get(row_doc_id).is_some_and(|dirty| dirty.0) {
            in_flight.insert(row_doc_id.to_owned(), Dirty::default());
            let driver = self.this.upgrade().expect("a driver is held by its Arc");
            let row_doc_id = row_doc_id.to_owned();
            tokio::spawn(async move { driver.drive(&row_doc_id, work).await });
        } else {
            in_flight.remove(row_doc_id);
        }
        Some(out)
    }

    /// Steps the row until no wake or retry is left.
    async fn drive(&self, row_doc_id: &str, _work: crate::runtime_service::RuntimeServiceWork) {
        loop {
            let (again, _) = self.step_once(row_doc_id).await;
            let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
            let dirty = std::mem::take(
                &mut in_flight
                    .get_mut(row_doc_id)
                    .expect("the row is in flight")
                    .0,
            );
            if !(again || dirty) {
                in_flight.remove(row_doc_id);
                return;
            }
        }
    }

    /// One step of the row, and whether it must run again.
    async fn step_once(&self, row_doc_id: &str) -> (bool, Result<StepOutcome>) {
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("the step semaphore is never closed");
        let outcome = run_step(&self.runtime, row_doc_id).await;
        let again = match &outcome {
            Ok(StepOutcome::Stale) => true,
            Ok(_) => {
                self.failures
                    .lock()
                    .expect("step failures lock")
                    .remove(row_doc_id);
                false
            }
            Err(error) => match error.downcast_ref::<StepFailure>() {
                None => {
                    tracing::warn!(row_doc_id, error = %format!("{error:#}"), "job step did not run");
                    false
                }
                Some(failure) => match self.step_failed(row_doc_id, failure).await {
                    Ok(verdict) => verdict == FailureVerdict::Retry,
                    Err(error) => {
                        tracing::warn!(row_doc_id, error = %format!("{error:#}"), "job step failure was not settled");
                        false
                    }
                },
            },
        };
        (again, outcome)
    }

    /// Reads the row and its children's observations, counts the failure and,
    /// on the third, fails the row closed with `plugin_job_step_failed`.
    async fn step_failed(&self, row_doc_id: &str, error: &StepFailure) -> Result<FailureVerdict> {
        let access = ConfigAccess::local(self.runtime.node.clone());
        let mut lifecycle = load_lifecycle(&access, row_doc_id).await?;
        let row = read_step_row(&access, row_doc_id).await?;
        let observed = observe_live_children(&access, &row, lifecycle.node_did()).await?;
        self.failures
            .lock()
            .expect("step failures lock")
            .entry(row_doc_id.to_owned())
            .or_default()
            .read = Some((row.clone(), observed));
        let verdict = self.on_step_failure(row_doc_id, error);
        if verdict == FailureVerdict::Settle {
            kill_as(
                &access,
                &mut lifecycle,
                KillCause::FailClosed(Refusal::StepFailed),
                Settlement::StepFailed {
                    last_error: error.to_string(),
                },
            )
            .await?;
            self.failures
                .lock()
                .expect("step failures lock")
                .remove(row_doc_id);
        }
        Ok(verdict)
    }

    /// Counts one step invocation failure of the row, whose row and children's
    /// observations `step_failed` has just read.
    pub fn on_step_failure(&self, row_doc_id: &str, error: &StepFailure) -> FailureVerdict {
        tracing::warn!(row_doc_id, %error, "job step failed");
        let mut failures = self.failures.lock().expect("step failures lock");
        let state = failures
            .get_mut(row_doc_id)
            .expect("the failing row's state is read");
        let (row, observed) = state.read.as_ref().expect("the failing row is read");
        let observe = |key: &str| {
            observed
                .get(key)
                .copied()
                .unwrap_or(KillObservation::Unresolved)
        };
        let counted = StepRow {
            consecutive_failures: state.count,
            ..row.clone()
        };
        let (outcome, refusal) = step_failure_decision(&counted, &observe);
        state.count = outcome.row.consecutive_failures;
        match refusal {
            Some(Refusal::StepFailed) => FailureVerdict::Settle,
            _ => FailureVerdict::Retry,
        }
    }
}
