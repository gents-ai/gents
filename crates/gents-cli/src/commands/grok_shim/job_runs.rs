//! Job rows for the stock pager as `workflow_updated`.

use rapidhash::RapidHashMap;
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::tools::background::job::dispatch::split_address;
use gents::tools::background::job::view::{
    list_jobs, load_job_view, JobFilter, JobKey, StateFilter,
};
use gents_protocol::job::{
    JobChildState, JobPauseReason, JobPhaseState, JobReadError, JobState, JobView,
};
use serde_json::{json, Map, Value};

use super::observation_cursor::{
    observe_due, ObservationSet, Observed, ObservedView, Sink, ABANDONED_REVISION,
};
use super::projection::ProjectionEngine;
use super::turn::sender::PromptSender;
use crate::commands::job::next_calls;

/// How long a transient read failure of one job is retried before the pager
/// is told Gents stopped observing it.
const TRANSIENT_GRACE_SECS: i64 = 120;

/// The one Grok child-state mapping: the pager knows active, done and failed.
fn agent_state(state: JobChildState) -> &'static str {
    match state {
        JobChildState::Live => "active",
        JobChildState::Completed => "done",
        JobChildState::Failed | JobChildState::Interrupted | JobChildState::Other => "failed",
    }
}

fn pause_reason_name(reason: JobPauseReason) -> &'static str {
    match reason {
        JobPauseReason::User => "user",
        JobPauseReason::BackOff => "back_off",
        JobPauseReason::NoProgress => "no_progress",
        JobPauseReason::Verification => "verification",
        JobPauseReason::Infra => "infra",
        JobPauseReason::BudgetExceeded => "budget_exceeded",
    }
}

/// Matches `JobView.state`, never `progress.state`, which stays `running` on an
/// ended row. Grok has no verification status.
fn status(state: JobState, pause_reason: Option<JobPauseReason>) -> &'static str {
    match state {
        JobState::Running => "active",
        JobState::Completed => "complete",
        JobState::Paused => match pause_reason {
            None | Some(JobPauseReason::User | JobPauseReason::Verification) => "user_paused",
            Some(JobPauseReason::BackOff) => "back_off_paused",
            Some(JobPauseReason::NoProgress) => "no_progress_paused",
            Some(JobPauseReason::Infra) => "infra_paused",
            Some(JobPauseReason::BudgetExceeded) => "budget_limited",
        },
        JobState::AwaitingUser => "blocked",
        JobState::Cancelled => "cancelled",
        JobState::Failed | JobState::TimedOut => "failed",
    }
}

impl ObservedView for JobView {
    fn is_terminal(&self) -> Result<bool> {
        Ok(self.state != JobState::Running)
    }

    /// Progress revision plus the children and their terminal outcomes: counts
    /// that only grow over a row's life.
    fn durable_revision(&self) -> u64 {
        let terminal = self
            .children
            .iter()
            .filter(|child| child.state != JobChildState::Live)
            .count();
        1 + self
            .progress
            .as_ref()
            .map_or(0, |progress| progress.revision)
            + self.children.len() as u64
            + terminal as u64
    }

    /// An agent row carries no `phase`: it names the phase active when the
    /// connection first saw the child, which a stateless projection cannot know.
    fn project(&self, revision: u64, now: DateTime<Utc>) -> Value {
        let progress = self.progress.as_ref();
        let pause_reason = progress.and_then(|progress| progress.pause_reason);
        let agents: Vec<Value> = self
            .children
            .iter()
            .map(|child| {
                json!({
                    "agent_id": child.session_id,
                    "label": child.title,
                    "state": agent_state(child.state),
                    "tokens_used": child.usage.total,
                    "duration_ms": child.duration_ms,
                })
            })
            .collect();
        let end = self.completed_at.unwrap_or(now.timestamp_millis() as u64);
        let mut update = Map::new();
        update.insert("sessionUpdate".into(), json!("workflow_updated"));
        update.insert("run_id".into(), json!(self.tool_call_id));
        update.insert("revision".into(), json!(revision));
        update.insert("name".into(), json!(self.handle));
        update.insert("status".into(), json!(status(self.state, pause_reason)));
        update.insert("foreground".into(), json!(false));
        update.insert(
            "elapsed_ms".into(),
            json!(end.saturating_sub(self.created_at)),
        );
        update.insert(
            "active_agents".into(),
            json!(agents.iter().filter(|a| a["state"] == "active").count()),
        );
        update.insert("agents".into(), Value::Array(agents));
        update.insert("phases".into(), json!([]));
        let mut details = Vec::new();
        if let Some(progress) = progress {
            update.insert("objective".into(), json!(progress.title));
            let phases: Vec<Value> = progress
                .phases
                .iter()
                .map(|phase| {
                    let state = match phase.state {
                        JobPhaseState::Pending => "pending",
                        JobPhaseState::Active => "active",
                        JobPhaseState::Done | JobPhaseState::Failed => "done",
                    };
                    json!({"title": phase.title, "state": state})
                })
                .collect();
            update.insert("phases".into(), Value::Array(phases));
            if let Some(phase) = progress
                .phases
                .iter()
                .find(|phase| phase.state == JobPhaseState::Active)
            {
                update.insert("current_phase".into(), json!(phase.title));
            }
            update.insert("agent_budget".into(), json!(progress.budget.total));
            update.insert("agents_used".into(), json!(progress.budget.spent));
            update.insert("agents_remaining".into(), json!(progress.budget.remaining));
            update.insert("agents_reserved".into(), json!(0));
            update.insert("last_event".into(), json!(progress.last_event));
            if let Some(at) = &progress.last_event_at {
                update.insert("last_event_timestamp".into(), json!(at));
            }
            let failed: Vec<&str> = progress
                .phases
                .iter()
                .filter(|phase| phase.state == JobPhaseState::Failed)
                .map(|phase| phase.title.as_str())
                .collect();
            if !failed.is_empty() {
                details.push(format!("failed phase: {}", failed.join(", ")));
            }
        }
        let next = next_calls(self.state, &self.address)
            .into_iter()
            .next()
            .unwrap_or_default();
        if let Some(outcome) = &self.outcome {
            details.push(format!(
                "{}: {} (next: {next})",
                outcome.code, outcome.message
            ));
        }
        if let Some(reason) = pause_reason {
            details.push(format!("paused: {}", pause_reason_name(reason)));
        }
        if !details.is_empty() {
            update.insert("last_event_detail".into(), json!(details.join("; ")));
        }
        let message = match self.state {
            JobState::Running | JobState::Completed => None,
            JobState::Paused => Some(match pause_reason {
                Some(reason) => format!("Paused ({}). Next: {next}", pause_reason_name(reason)),
                None => format!("Paused. Next: {next}"),
            }),
            JobState::AwaitingUser => Some(format!(
                "{} Reply in chat to answer.",
                progress
                    .and_then(|progress| progress.question.as_deref())
                    .unwrap_or("The workflow is waiting for an answer.")
            )),
            JobState::Failed => Some(format!("Failed. Next: {next}")),
            JobState::Cancelled => Some(format!("Cancelled. Next: {next}")),
            JobState::TimedOut => Some(format!("Timed out. Next: {next}")),
        };
        if let Some(message) = message {
            update.insert("pause_message".into(), json!(message));
        }
        Value::Object(update)
    }
}

/// A failed job read, naming the next call. `abandoned` prints only this
/// outermost context; the typed `JobReadError` stays reachable by downcast.
#[derive(Debug)]
struct JobUnobservable {
    address: String,
    reason: String,
    grace_expired: bool,
}

impl std::fmt::Display for JobUnobservable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; next: gents job show {}", self.reason, self.address)
    }
}

fn permanent(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<JobReadError>()
        .is_some_and(|error| !error.is_transient())
        || error
            .downcast_ref::<JobUnobservable>()
            .is_some_and(|error| error.grace_expired)
}

/// The base frame for a job that never delivered one.
fn unobserved(address: &str, job: &Observed) -> Value {
    json!({
        "sessionUpdate": "workflow_updated",
        "run_id": split_address(address).map_or_else(|| address.to_owned(), |(_, call)| call),
        "name": job.name,
        "foreground": false,
        "phases": [],
        "elapsed_ms": 0,
        "agents": [],
    })
}

/// Observes the session's job rows, keyed by job address. The session
/// observer has already authorized the attached root session.
#[derive(Default)]
pub(super) struct JobRunCursor {
    /// Row doc ids whose terminal frame this connection delivered.
    terminal_delivered: BTreeSet<String>,
    /// Tool-call ids of runs with a delivered frame that were not abandoned.
    pub(super) delivered_runs: BTreeSet<String>,
    rows: RapidHashMap<String, String>,
    first_failure: Mutex<RapidHashMap<String, DateTime<Utc>>>,
    discovery_failures: u32,
    jobs: ObservationSet,
}

impl JobRunCursor {
    /// Returns the failures worth reporting; none of them stops the other
    /// jobs from being observed.
    pub(super) async fn refresh(
        &mut self,
        node: &Arc<EmbeddedNode>,
        principal: &str,
        session: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Vec<anyhow::Error> {
        let now = Utc::now();
        let access = &ConfigAccess::local(node.clone());
        let mut reports = self.discover(access, principal, session).await;
        reports.extend(
            self.observe(session, sender, projections, now, |key| {
                load_job_view(access, None, key)
            })
            .await,
        );
        reports
    }

    fn discovery_filter(&self, session: &str, principal: &str) -> JobFilter {
        JobFilter {
            session_id: Some(session.to_owned()),
            requester: Some(principal.to_owned()),
            state: StateFilter::NotTerminalOrUndelivered {
                delivered: self.terminal_delivered.clone(),
            },
            plugin: None,
        }
    }

    fn adopt(&mut self, view: JobView) {
        self.rows.insert(view.address.clone(), view.row_doc_id);
        self.jobs.insert(view.address, Observed::new(view.handle));
    }

    async fn discover(
        &mut self,
        access: &ConfigAccess,
        principal: &str,
        session: &str,
    ) -> Vec<anyhow::Error> {
        match list_jobs(access, &self.discovery_filter(session, principal)).await {
            Ok(views) => {
                self.discovery_failures = 0;
                views.into_iter().for_each(|view| self.adopt(view));
                Vec::new()
            }
            // New jobs stay discoverable, so discovery itself is never
            // abandoned; only the first failure of a streak is reported.
            Err(error) => {
                self.discovery_failures += 1;
                if self.discovery_failures == 1 {
                    vec![anyhow::Error::new(error).context("job discovery: retrying")]
                } else {
                    Vec::new()
                }
            }
        }
    }

    async fn observe<R, Fut>(
        &mut self,
        session: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        now: DateTime<Utc>,
        read: R,
    ) -> Vec<anyhow::Error>
    where
        R: Fn(JobKey) -> Fut,
        Fut: Future<Output = Result<JobView, JobReadError>>,
    {
        let sink = Sink {
            session,
            sender,
            projections,
        };
        let first_failure = &self.first_failure;
        let read = &read;
        let reports = observe_due(
            &mut self.jobs,
            &sink,
            now,
            |address: String| async move {
                let loaded = match split_address(&address) {
                    Some((session_id, tool_call_id)) => {
                        read(JobKey::Address {
                            session_id,
                            tool_call_id,
                        })
                        .await
                    }
                    None => Err(JobReadError::Malformed {
                        message: format!("`{address}` is not a job address"),
                    }),
                };
                let mut first_failure = first_failure.lock().unwrap();
                match loaded {
                    Ok(view) => {
                        first_failure.remove(&address);
                        Ok(view)
                    }
                    Err(error) => {
                        let first = *first_failure.entry(address.clone()).or_insert(now);
                        let grace_expired = error.is_transient()
                            && now - first >= Duration::seconds(TRANSIENT_GRACE_SECS);
                        let reason = error.to_string();
                        Err(anyhow::Error::new(error).context(JobUnobservable {
                            address,
                            reason,
                            grace_expired,
                        }))
                    }
                }
            },
            permanent,
            unobserved,
            "job",
        )
        .await;
        self.settle();
        projections.record_job_runs(session, &self.delivered_runs);
        reports
    }

    /// Records what this tick delivered, and drops the failure clock of a job
    /// that is no longer observed.
    fn settle(&mut self) {
        let first_failure = self.first_failure.get_mut().unwrap();
        for (address, job) in &self.jobs.subjects {
            if let Some((_, call)) = split_address(address) {
                if job.revision == ABANDONED_REVISION {
                    self.delivered_runs.remove(&call);
                } else if job.delivered.is_some() {
                    self.delivered_runs.insert(call);
                }
            }
            if job.finished {
                first_failure.remove(address);
                if let Some(row) = self.rows.get(address) {
                    self.terminal_delivered.insert(row.clone());
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "job_runs_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "job_runs_replay_tests.rs"]
mod replay_tests;
