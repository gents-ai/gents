//! Job rows for the stock pager as `workflow_updated`.

use rapidhash::RapidHashMap;
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::plugin_job::dispatch::split_address;
use gents::plugin_job::view::{list_jobs, load_job_view, JobFilter, JobKey, StateFilter};
use gents_protocol::job::{
    JobChildState, JobPauseReason, JobPhaseState, JobReadError, JobState, JobView,
};
use serde_json::{json, Map, Value};

use super::observation_cursor::{
    observe_due, ObservationSet, Observed, ObservedView, Sink, ABANDONED_REVISION,
};
use super::projection::ProjectionEngine;
use super::turn::PromptSender;
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
        let next = next_calls(
            self.state,
            pause_reason,
            self.outcome.as_ref().map(|outcome| outcome.code.as_str()),
            &self.address,
            &self.handle,
        )
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
mod tests {
    use gents_protocol::job::{
        JobBudget, JobChildRow, JobOrigin, JobOutcome, JobPhase, JobPinSummary, JobProgress,
        JobScope, JobUsage,
    };

    use super::*;

    const CREATED: u64 = 1_790_000_000_000;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp_millis((CREATED + 5_000) as i64).unwrap()
    }

    pub(super) fn view(state: JobState) -> JobView {
        JobView {
            address: "session-1:call-1".to_owned(),
            row_doc_id: "row-1".to_owned(),
            session_id: "session-1".to_owned(),
            tool_call_id: "call-1".to_owned(),
            scope: JobScope {
                session_id: "session-1".to_owned(),
                request_id: "request-1".to_owned(),
                node_did: "did:key:node".to_owned(),
                requester_did: None,
            },
            handle: "research".to_owned(),
            state,
            outcome: None,
            pin: JobPinSummary {
                script_id: "script".to_owned(),
                origin: JobOrigin::Installed,
                engine_digest: "digest".to_owned(),
                plugin_ref: "ns/name".to_owned(),
                agent_budget: 8,
                cwd: "/work".to_owned(),
                chain_root_doc_id: "root".to_owned(),
                resume_of: None,
            },
            progress: None,
            children: Vec::new(),
            created_at: CREATED,
            adopted_at: CREATED + 100,
            completed_at: None,
        }
    }

    pub(super) fn progress(phases: Vec<(&str, JobPhaseState)>) -> JobProgress {
        JobProgress {
            title: "Survey the field".to_owned(),
            state: JobState::Running,
            pause_reason: None,
            phases: phases
                .into_iter()
                .map(|(title, state)| JobPhase {
                    title: title.to_owned(),
                    state,
                })
                .collect(),
            active: Vec::new(),
            spawned: 3,
            budget: JobBudget {
                total: 8,
                spent: 3,
                remaining: 5,
            },
            last_event: "phase started".to_owned(),
            last_event_at: Some("2026-10-02T05:30:00Z".to_owned()),
            question: None,
            observed_terminal_children: 0,
            revision: 4,
        }
    }

    pub(super) fn child(session: &str, state: JobChildState) -> JobChildRow {
        JobChildRow {
            slot: format!("slot-{session}"),
            attempt: 1,
            session_id: session.to_owned(),
            request_id: format!("request-{session}"),
            state,
            title: format!("title-{session}"),
            usage: JobUsage {
                input: 3,
                output: 4,
                total: 7,
            },
            duration_ms: 1_500,
            target: "agent-a".to_owned(),
        }
    }

    fn status_of(state: JobState, reason: Option<JobPauseReason>) -> Value {
        let mut view = view(state);
        let mut progress = progress(Vec::new());
        progress.pause_reason = reason;
        view.progress = Some(progress);
        view.project(1, now())["status"].clone()
    }

    #[test]
    fn status_maps_running_to_active() {
        assert_eq!(status_of(JobState::Running, None), "active");
        assert_eq!(
            view(JobState::Running).project(1, now())["status"],
            "active"
        );
    }

    #[test]
    fn status_maps_completed_to_complete() {
        assert_eq!(status_of(JobState::Completed, None), "complete");
    }

    #[test]
    fn status_maps_paused_without_reason_or_for_user_to_user_paused() {
        assert_eq!(status_of(JobState::Paused, None), "user_paused");
        assert_eq!(
            status_of(JobState::Paused, Some(JobPauseReason::User)),
            "user_paused"
        );
        assert_eq!(
            view(JobState::Paused).project(1, now())["status"],
            "user_paused"
        );
    }

    #[test]
    fn status_maps_each_automatic_pause_reason() {
        let cases = [
            (JobPauseReason::BackOff, "back_off_paused"),
            (JobPauseReason::NoProgress, "no_progress_paused"),
            (JobPauseReason::Infra, "infra_paused"),
            (JobPauseReason::BudgetExceeded, "budget_limited"),
        ];
        for (reason, expected) in cases {
            assert_eq!(status_of(JobState::Paused, Some(reason)), expected);
        }
    }

    #[test]
    fn status_maps_verification_to_user_paused_and_names_it() {
        let mut view = view(JobState::Paused);
        let mut progress = progress(Vec::new());
        progress.pause_reason = Some(JobPauseReason::Verification);
        view.progress = Some(progress);
        let frame = view.project(1, now());
        assert_eq!(frame["status"], "user_paused");
        let message = frame["pause_message"].as_str().unwrap();
        assert!(message.contains("verification"));
        assert!(message.contains("workflow resume session-1:call-1"));
    }

    #[test]
    fn status_maps_awaiting_user_to_blocked() {
        assert_eq!(status_of(JobState::AwaitingUser, None), "blocked");
    }

    #[test]
    fn status_maps_cancelled_from_the_view_not_the_progress_state() {
        let mut view = view(JobState::Cancelled);
        view.progress = Some(progress(Vec::new()));
        assert_eq!(view.progress.as_ref().unwrap().state, JobState::Running);
        assert_eq!(view.project(1, now())["status"], "cancelled");
    }

    #[test]
    fn status_maps_failed_and_timed_out_to_failed() {
        assert_eq!(status_of(JobState::Failed, None), "failed");
        assert_eq!(status_of(JobState::TimedOut, None), "failed");
    }

    #[test]
    fn agent_state_maps_every_child_state() {
        assert_eq!(agent_state(JobChildState::Live), "active");
        assert_eq!(agent_state(JobChildState::Completed), "done");
        assert_eq!(agent_state(JobChildState::Failed), "failed");
        assert_eq!(agent_state(JobChildState::Interrupted), "failed");
        assert_eq!(agent_state(JobChildState::Other), "failed");
    }

    #[test]
    fn active_agents_counts_active_children() {
        let states = [
            JobChildState::Live,
            JobChildState::Completed,
            JobChildState::Failed,
            JobChildState::Interrupted,
            JobChildState::Other,
        ];
        for mask in 0..states.len().pow(4) {
            let picked: Vec<JobChildState> = (0..4)
                .map(|slot| states[(mask / states.len().pow(slot)) % states.len()])
                .collect();
            let mut view = view(JobState::Running);
            view.children = picked
                .iter()
                .enumerate()
                .map(|(index, state)| child(&index.to_string(), *state))
                .collect();
            let frame = view.project(1, now());
            let agents = frame["agents"].as_array().unwrap();
            let active = agents.iter().filter(|a| a["state"] == "active").count();
            assert_eq!(frame["active_agents"], active);
            assert_eq!(
                active,
                picked.iter().filter(|s| **s == JobChildState::Live).count()
            );
        }
    }

    #[test]
    fn agents_carry_the_child_session_title_tokens_and_duration() {
        let mut view = view(JobState::Running);
        view.children = vec![child("c1", JobChildState::Completed)];
        let agent = &view.project(1, now())["agents"][0];
        assert_eq!(agent["agent_id"], "c1");
        assert_eq!(agent["label"], "title-c1");
        assert_eq!(agent["state"], "done");
        assert_eq!(agent["tokens_used"], 7);
        assert_eq!(agent["duration_ms"], 1_500);
        assert!(agent.get("phase").is_none());
    }

    #[test]
    fn durable_revision_counts_progress_children_and_terminal_children() {
        let mut view = view(JobState::Running);
        assert_eq!(view.durable_revision(), 1);
        view.progress = Some(progress(Vec::new()));
        view.children = vec![
            child("a", JobChildState::Live),
            child("b", JobChildState::Completed),
            child("c", JobChildState::Failed),
        ];
        assert_eq!(view.durable_revision(), 1 + 4 + 3 + 2);
    }

    #[test]
    fn is_terminal_is_anything_but_running() {
        assert!(!view(JobState::Running).is_terminal().unwrap());
        for state in [
            JobState::Paused,
            JobState::AwaitingUser,
            JobState::Completed,
            JobState::Failed,
            JobState::Cancelled,
            JobState::TimedOut,
        ] {
            assert!(view(state).is_terminal().unwrap());
        }
    }

    #[test]
    fn failed_phase_renders_done_and_is_named_in_detail() {
        let mut view = view(JobState::Failed);
        view.progress = Some(progress(vec![
            ("plan", JobPhaseState::Done),
            ("build", JobPhaseState::Failed),
            ("ship", JobPhaseState::Pending),
        ]));
        let frame = view.project(1, now());
        let states: Vec<&str> = frame["phases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|phase| phase["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["done", "done", "pending"]);
        assert!(frame["last_event_detail"]
            .as_str()
            .unwrap()
            .contains("build"));
        assert!(frame.get("current_phase").is_none());
    }

    #[test]
    fn current_phase_is_the_first_active_phase() {
        let mut view = view(JobState::Running);
        view.progress = Some(progress(vec![
            ("plan", JobPhaseState::Done),
            ("build", JobPhaseState::Active),
            ("test", JobPhaseState::Active),
        ]));
        assert_eq!(view.project(1, now())["current_phase"], "build");
    }

    #[test]
    fn outcome_code_message_and_next_call_reach_the_detail() {
        let mut view = view(JobState::Failed);
        view.outcome = Some(JobOutcome {
            code: "plugin_job_step_failed".to_owned(),
            message: "step three failed".to_owned(),
        });
        let frame = view.project(1, now());
        let detail = frame["last_event_detail"].as_str().unwrap();
        assert!(detail.contains("plugin_job_step_failed"));
        assert!(detail.contains("step three failed"));
        assert!(detail.contains("workflow status session-1:call-1"));
    }

    #[test]
    fn pause_message_names_the_next_call() {
        let mut view = view(JobState::Paused);
        let mut progress = progress(Vec::new());
        progress.pause_reason = Some(JobPauseReason::BudgetExceeded);
        view.progress = Some(progress);
        let frame = view.project(1, now());
        assert!(frame["pause_message"]
            .as_str()
            .unwrap()
            .contains("workflow resume session-1:call-1 --budget N"));
        assert!(frame["last_event_detail"]
            .as_str()
            .unwrap()
            .contains("budget_exceeded"));
    }

    #[test]
    fn awaiting_user_pause_message_carries_the_question_and_a_reply_hint() {
        let mut view = view(JobState::AwaitingUser);
        let mut progress = progress(Vec::new());
        progress.question = Some("Which branch?".to_owned());
        view.progress = Some(progress);
        let message = view.project(1, now())["pause_message"].clone();
        assert!(message.as_str().unwrap().contains("Which branch?"));
        assert!(message.as_str().unwrap().contains("Reply in chat"));
    }

    #[test]
    fn running_and_completed_rows_carry_no_pause_message() {
        for state in [JobState::Running, JobState::Completed] {
            assert!(view(state).project(1, now()).get("pause_message").is_none());
        }
    }

    #[test]
    fn elapsed_ms_runs_from_created_at() {
        let mut view = view(JobState::Running);
        assert_eq!(view.project(1, now())["elapsed_ms"], 5_000);
        view.state = JobState::Completed;
        view.completed_at = Some(CREATED + 2_000);
        assert_eq!(view.project(1, now())["elapsed_ms"], 2_000);
    }

    #[test]
    fn absent_progress_projects_an_empty_frame() {
        let frame = view(JobState::Running).project(7, now());
        assert_eq!(frame["sessionUpdate"], "workflow_updated");
        assert_eq!(frame["run_id"], "call-1");
        assert_eq!(frame["revision"], 7);
        assert_eq!(frame["name"], "research");
        assert_eq!(frame["foreground"], false);
        assert_eq!(frame["phases"], json!([]));
        assert_eq!(frame["agents"], json!([]));
        assert_eq!(frame["active_agents"], 0);
        for key in [
            "objective",
            "agent_budget",
            "agents_used",
            "agents_remaining",
            "agents_reserved",
            "current_phase",
            "last_event",
            "last_event_timestamp",
        ] {
            assert!(frame.get(key).is_none(), "{key} must be omitted");
        }
    }

    #[test]
    fn present_progress_projects_title_budget_and_last_event() {
        let mut view = view(JobState::Running);
        view.progress = Some(progress(vec![("plan", JobPhaseState::Active)]));
        let frame = view.project(1, now());
        assert_eq!(frame["objective"], "Survey the field");
        assert_eq!(frame["agent_budget"], 8);
        assert_eq!(frame["agents_used"], 3);
        assert_eq!(frame["agents_remaining"], 5);
        assert_eq!(frame["last_event"], "phase started");
        assert_eq!(frame["last_event_timestamp"], "2026-10-02T05:30:00Z");
    }

    #[test]
    fn omits_result_summary_and_agent_usage_incomplete() {
        let mut view = view(JobState::Completed);
        view.progress = Some(progress(Vec::new()));
        let frame = view.project(1, now());
        assert!(frame.get("result_summary").is_none());
        assert!(frame.get("agent_usage_incomplete").is_none());
        assert_eq!(frame["agents_reserved"], 0);
    }

    use std::collections::BTreeSet;
    use std::sync::Arc;

    use chrono::Duration;
    use gents::plugin_job::view::{JobFilter, StateFilter};
    use gents_protocol::job::JobReadError;

    use super::super::observation_cursor::{ABANDONED_REVISION, MAX_RETRY_DELAY_SECS};
    use super::super::projection::{BoundModelContext, ProjectionEngine};
    use super::super::turn::PromptSender;

    pub(super) fn at(seconds: i64) -> DateTime<Utc> {
        now() + Duration::seconds(seconds)
    }

    pub(super) async fn harness() -> (
        tempfile::TempDir,
        ProjectionEngine,
        Arc<tokio::sync::Mutex<Vec<String>>>,
        PromptSender,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(directory.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        gents::schema::ensure_runtime_schemas(&node).await.unwrap();
        let projections = ProjectionEngine::new(
            node,
            BoundModelContext::new("model".into(), "Model".into(), 1000),
        );
        let buffer = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        (directory, projections, buffer, sender)
    }

    pub(super) async fn updates(buffer: &tokio::sync::Mutex<Vec<String>>) -> Vec<Value> {
        buffer
            .lock()
            .await
            .iter()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                assert_eq!(line["method"], "_x.ai/session_notification");
                line["params"]["update"].clone()
            })
            .collect()
    }

    pub(super) async fn tick(
        cursor: &mut JobRunCursor,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        at: DateTime<Utc>,
        result: Result<JobView, JobReadError>,
    ) -> Vec<anyhow::Error> {
        cursor
            .observe("session-1", sender, projections, at, |_| {
                let result = result.clone();
                async move { result }
            })
            .await
    }

    pub(super) fn adopted(view: &JobView) -> JobRunCursor {
        let mut cursor = JobRunCursor::default();
        cursor.adopt(view.clone());
        cursor
    }

    #[test]
    fn discovery_filter_names_session_requester_and_delivered_row_ids() {
        let mut cursor = JobRunCursor::default();
        cursor.terminal_delivered.insert("row-9".to_owned());
        assert_eq!(
            cursor.discovery_filter("session-1", "did:key:node"),
            JobFilter {
                session_id: Some("session-1".to_owned()),
                requester: Some("did:key:node".to_owned()),
                state: StateFilter::NotTerminalOrUndelivered {
                    delivered: BTreeSet::from(["row-9".to_owned()]),
                },
                plugin: None,
            }
        );
    }

    #[tokio::test]
    async fn transient_error_abandons_only_after_120s() {
        let (_directory, projections, buffer, sender) = harness().await;
        let mut cursor = adopted(&view(JobState::Running));
        for seconds in [0, 119] {
            tick(
                &mut cursor,
                &sender,
                &projections,
                at(seconds),
                Err(JobReadError::NotYetObservable),
            )
            .await;
            assert!(updates(&buffer).await.is_empty(), "at {seconds}s");
        }
        let reports = tick(
            &mut cursor,
            &sender,
            &projections,
            at(121),
            Err(JobReadError::NotYetObservable),
        )
        .await;
        assert!(!reports.is_empty());
        let frames = updates(&buffer).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["status"], "failed");
        assert_eq!(frames[0]["revision"], ABANDONED_REVISION);
        assert!(cursor.jobs.subjects["session-1:call-1"].finished);
        assert!(!cursor.delivered_runs.contains("call-1"));
    }

    #[tokio::test]
    async fn run_abandoned_after_a_delivered_frame_is_no_longer_delivered() {
        let (_directory, projections, _buffer, sender) = harness().await;
        let running = view(JobState::Running);
        let mut cursor = adopted(&running);
        tick(&mut cursor, &sender, &projections, at(0), Ok(running)).await;
        assert!(projections.delivered_job_runs().contains("call-1"));
        tick(
            &mut cursor,
            &sender,
            &projections,
            at(MAX_RETRY_DELAY_SECS),
            Err(JobReadError::NotAJob),
        )
        .await;
        assert!(cursor.delivered_runs.is_empty());
        assert!(projections.delivered_job_runs().is_empty());
    }

    #[tokio::test]
    async fn permanent_error_abandons_at_once() {
        let (_directory, projections, buffer, sender) = harness().await;
        let mut cursor = adopted(&view(JobState::Running));
        tick(
            &mut cursor,
            &sender,
            &projections,
            at(0),
            Err(JobReadError::NotAJob),
        )
        .await;
        let frames = updates(&buffer).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["status"], "failed");
        assert_eq!(frames[0]["revision"], ABANDONED_REVISION);
        assert_eq!(frames[0]["run_id"], "call-1");
        assert_eq!(frames[0]["name"], "research");
        assert!(cursor.terminal_delivered.contains("row-1"));
    }

    #[tokio::test]
    async fn abandoned_frame_names_gents_job_show() {
        let (_directory, projections, buffer, sender) = harness().await;
        let mut cursor = adopted(&view(JobState::Running));
        tick(
            &mut cursor,
            &sender,
            &projections,
            at(0),
            Err(JobReadError::NotAuthorized),
        )
        .await;
        let frames = updates(&buffer).await;
        let message = frames[0]["pause_message"].as_str().unwrap();
        assert!(message.starts_with("Gents stopped observing this job: "));
        assert!(message.contains(&JobReadError::NotAuthorized.to_string()));
        assert!(message.ends_with("next: gents job show session-1:call-1"));
    }

    #[tokio::test]
    async fn first_failure_entry_is_dropped_when_observation_ends() {
        let (_directory, projections, _buffer, sender) = harness().await;
        let address = "session-1:call-1";
        let first_failures = |cursor: &mut JobRunCursor| {
            cursor
                .first_failure
                .get_mut()
                .unwrap()
                .contains_key(address)
        };

        let mut recovered = adopted(&view(JobState::Running));
        tick(
            &mut recovered,
            &sender,
            &projections,
            at(0),
            Err(JobReadError::NotYetObservable),
        )
        .await;
        assert!(first_failures(&mut recovered));
        tick(
            &mut recovered,
            &sender,
            &projections,
            at(MAX_RETRY_DELAY_SECS),
            Ok(view(JobState::Running)),
        )
        .await;
        assert!(!first_failures(&mut recovered));

        let mut finished = adopted(&view(JobState::Running));
        tick(
            &mut finished,
            &sender,
            &projections,
            at(0),
            Err(JobReadError::NotYetObservable),
        )
        .await;
        assert!(first_failures(&mut finished));
        tick(
            &mut finished,
            &sender,
            &projections,
            at(MAX_RETRY_DELAY_SECS),
            Ok(view(JobState::Completed)),
        )
        .await;
        assert!(!first_failures(&mut finished));

        let mut abandoned = adopted(&view(JobState::Running));
        tick(
            &mut abandoned,
            &sender,
            &projections,
            at(0),
            Err(JobReadError::NotYetObservable),
        )
        .await;
        tick(
            &mut abandoned,
            &sender,
            &projections,
            at(MAX_RETRY_DELAY_SECS),
            Err(JobReadError::Malformed {
                message: "bad".to_owned(),
            }),
        )
        .await;
        assert!(!first_failures(&mut abandoned));
    }

    #[tokio::test]
    async fn already_terminal_row_yields_one_terminal_frame() {
        let (_directory, projections, buffer, sender) = harness().await;
        let terminal = view(JobState::Completed);
        let mut cursor = adopted(&terminal);
        assert!(cursor.terminal_delivered.is_empty());
        for seconds in [0, 1, 2] {
            tick(
                &mut cursor,
                &sender,
                &projections,
                at(seconds),
                Ok(terminal.clone()),
            )
            .await;
        }
        let frames = updates(&buffer).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["status"], "complete");
        assert!(cursor.terminal_delivered.contains("row-1"));
        assert!(cursor.delivered_runs.contains("call-1"));
    }

    #[tokio::test]
    async fn delivered_runs_reach_the_projection_engine() {
        let (_directory, projections, _buffer, sender) = harness().await;
        let running = view(JobState::Running);
        let mut cursor = adopted(&running);
        assert!(projections.delivered_job_runs().is_empty());
        tick(&mut cursor, &sender, &projections, at(0), Ok(running)).await;
        assert_eq!(
            projections.delivered_job_runs(),
            BTreeSet::from(["call-1".to_owned()])
        );
    }

    #[tokio::test]
    async fn revisions_are_monotone() {
        let (_directory, projections, buffer, sender) = harness().await;
        let mut running = view(JobState::Running);
        let mut high = progress(Vec::new());
        high.revision = 10;
        running.progress = Some(high);
        let mut lower = running.clone();
        let mut behind = progress(Vec::new());
        behind.revision = 3;
        behind.last_event = "a later event".to_owned();
        lower.progress = Some(behind);
        let mut cursor = adopted(&running);
        for (seconds, next) in [
            (0, running.clone()),
            (1, lower),
            (2, view(JobState::Completed)),
        ] {
            tick(&mut cursor, &sender, &projections, at(seconds), Ok(next)).await;
        }
        let revisions: Vec<u64> = updates(&buffer)
            .await
            .iter()
            .map(|frame| frame["revision"].as_u64().unwrap())
            .collect();
        assert_eq!(revisions.len(), 3);
        assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(revisions[0], 11);
    }
}

#[cfg(test)]
#[path = "job_runs_replay_tests.rs"]
mod replay_tests;
