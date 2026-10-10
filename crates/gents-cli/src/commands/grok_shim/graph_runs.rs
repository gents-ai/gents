//! Graph-run visibility for the stock pager as `workflow_updated`.
//!
//! The run id comes from the session's durable `run_graph` tool results; the
//! run itself is the canonical `GraphRunView`. Only the last delivered
//! observation per run is connection-local. Observation never writes: a
//! failed read only defers or ends this connection's display of one run.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::graph_pipeline::{
    load_graph_run_view_with_access, run_receipt_from_tool_result, GraphRunStatus,
    GraphRunUnobservable, GraphRunView,
};
use serde_json::{json, Value};

use super::projection::tools::session_tool_results;
use super::projection::{ProjectionEngine, UpdateTimestamps};
use super::turn::{PromptSender, PromptSenderLine};

/// Run views loaded per observation tick. Each load is several queries on
/// the session observer's shared loop, so a session that resumes with many
/// runs is covered round-robin instead of delaying its token stream.
const VIEW_LOADS_PER_TICK: usize = 4;

/// The revision of the terminal frame sent when observation stops. The pager
/// keeps the highest revision it holds across leader reconnects (see
/// [`durable_revision`]), so this frame must outrank anything an earlier
/// observer delivered. Stopping is final for this actor, so no frame follows
/// it. 2^53 - 1 is the largest integer a JSON number keeps exactly as an
/// IEEE double.
const ABANDONED_REVISION: u64 = (1 << 53) - 1;

/// Upper bound on the delay before re-reading a run or `run_graph` reply
/// whose last read failed.
const MAX_RETRY_DELAY_SECS: i64 = 30;

/// The display-only failure streak of one observed subject. It defers the
/// subject's next read; it never repeats a read.
#[derive(Default)]
struct Backoff {
    failures: u32,
    retry_at: Option<DateTime<Utc>>,
}

impl Backoff {
    fn ready(&self, now: DateTime<Utc>) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    /// Defers the next read exponentially. Returns the error worth
    /// reporting: only a streak's first, so a lasting failure is not logged
    /// on every read.
    fn fail(
        &mut self,
        now: DateTime<Utc>,
        error: anyhow::Error,
        subject: &str,
    ) -> Option<anyhow::Error> {
        self.failures += 1;
        let delay = (1_i64 << (self.failures - 1).min(5)).min(MAX_RETRY_DELAY_SECS);
        self.retry_at = Some(now + Duration::seconds(delay));
        (self.failures == 1).then(|| error.context(format!("{subject}: retrying")))
    }
}

struct Delivered {
    update: Value,
    at: DateTime<Utc>,
}

struct RunObservation {
    graph_id: String,
    revision: u64,
    delivered: Option<Delivered>,
    finished: bool,
    backoff: Backoff,
}

impl RunObservation {
    fn new(graph_id: impl Into<String>) -> Self {
        Self {
            graph_id: graph_id.into(),
            revision: 0,
            delivered: None,
            finished: false,
            backoff: Backoff::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct GraphRunCursor {
    scanned: HashSet<String>,
    replies: HashMap<String, Backoff>,
    discovery_failures: u32,
    runs: BTreeMap<String, RunObservation>,
    /// The last run loaded; the next tick's loads continue after it.
    loaded_through: Option<String>,
}

enum ObservationError {
    /// Repeating the same read by the same actor fails the same way.
    Permanent(anyhow::Error),
    Transient(anyhow::Error),
}

impl GraphRunCursor {
    /// The session observer has already authorized the attached root session.
    /// Returns the failures worth reporting; none of them stops the other
    /// runs from being observed.
    pub(super) async fn refresh(
        &mut self,
        node: &Arc<EmbeddedNode>,
        principal: &str,
        session: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Vec<anyhow::Error> {
        let now = Utc::now();
        let mut reports = self.discover(node, principal, session, now).await;
        let access = &ConfigAccess::Local(node.clone());
        reports.extend(
            self.observe(session, sender, projections, now, |run_id| async move {
                load_graph_run_view_with_access(access, principal, &run_id).await
            })
            .await,
        );
        reports
    }

    /// A reply that is not yet delivered is revisited; a delivered reply
    /// without a receipt (a failed start) is final.
    async fn discover(
        &mut self,
        node: &Arc<EmbeddedNode>,
        principal: &str,
        session: &str,
        now: DateTime<Utc>,
    ) -> Vec<anyhow::Error> {
        let mut skip = self.scanned.clone();
        skip.extend(
            self.replies
                .iter()
                .filter(|(_, backoff)| !backoff.ready(now))
                .map(|(doc_id, _)| doc_id.clone()),
        );
        let calls = match session_tool_results(
            node,
            principal,
            session,
            gents::self_config::RUN_GRAPH_TOOL_NAME,
            &skip,
        )
        .await
        {
            Ok(calls) => calls,
            // New runs stay discoverable, so discovery itself is never
            // abandoned; only the first failure of a streak is reported.
            Err(error) => {
                self.discovery_failures += 1;
                return if self.discovery_failures == 1 {
                    vec![error.context("graph run discovery: retrying")]
                } else {
                    Vec::new()
                };
            }
        };
        self.discovery_failures = 0;
        let mut reports = Vec::new();
        for call in calls {
            match call.result {
                Ok(None) => {
                    self.replies.remove(&call.doc_id);
                }
                Ok(Some(result)) => {
                    if let Some(receipt) = run_receipt_from_tool_result(&result) {
                        self.runs
                            .entry(receipt.run_id)
                            .or_insert_with(|| RunObservation::new(receipt.graph_id));
                    }
                    self.replies.remove(&call.doc_id);
                    self.scanned.insert(call.doc_id);
                }
                Err(error) => {
                    let subject = format!("run_graph reply {}", call.doc_id);
                    let backoff = self.replies.entry(call.doc_id).or_default();
                    reports.extend(backoff.fail(now, error, &subject));
                }
            }
        }
        reports
    }

    async fn observe<F, Fut>(
        &mut self,
        session: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        now: DateTime<Utc>,
        mut load: F,
    ) -> Vec<anyhow::Error>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<GraphRunView>>,
    {
        let mut reports = Vec::new();
        for run_id in self.due(now) {
            let Some(run) = self.runs.get_mut(&run_id) else {
                continue;
            };
            let outcome = async {
                let view = load(run_id.clone()).await.map_err(|error| {
                    if error.downcast_ref::<GraphRunUnobservable>().is_some() {
                        ObservationError::Permanent(error)
                    } else {
                        ObservationError::Transient(error)
                    }
                })?;
                let status = view.status().map_err(ObservationError::Permanent)?;
                let revision = durable_revision(&view).max(run.revision + 1);
                let update = project(&view, status, revision, now);
                let shape = without_clock(&update);
                if run
                    .delivered
                    .as_ref()
                    .is_none_or(|delivered| without_clock(&delivered.update) != shape)
                {
                    send(session, update.clone(), sender, projections)
                        .await
                        .map_err(ObservationError::Transient)?;
                    run.revision = revision;
                    run.delivered = Some(Delivered { update, at: now });
                }
                run.finished = status.is_terminal();
                Ok(())
            }
            .await;
            let subject = format!("graph run {run_id}");
            match outcome {
                Ok(()) => run.backoff = Backoff::default(),
                Err(ObservationError::Transient(error)) => {
                    reports.extend(run.backoff.fail(now, error, &subject));
                }
                Err(ObservationError::Permanent(error)) => {
                    run.finished = true;
                    run.revision = ABANDONED_REVISION;
                    let update = abandoned(&run_id, run, &error, now);
                    reports.push(error.context(format!("{subject}: observation stopped")));
                    if let Err(error) = send(session, update, sender, projections).await {
                        reports.push(error.context(format!("{subject}: final update")));
                    }
                }
            }
        }
        reports
    }

    /// Up to [`VIEW_LOADS_PER_TICK`] runs that are neither finished nor
    /// waiting out a failure, continuing after the last run loaded.
    fn due(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let after = self.loaded_through.as_deref();
        let (later, earlier): (Vec<&String>, Vec<&String>) = self
            .runs
            .iter()
            .filter(|(_, run)| !run.finished && run.backoff.ready(now))
            .map(|(run_id, _)| run_id)
            .partition(|run_id| after.is_some_and(|after| run_id.as_str() > after));
        let due = later
            .into_iter()
            .chain(earlier)
            .take(VIEW_LOADS_PER_TICK)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(last) = due.last() {
            self.loaded_through = Some(last.clone());
        }
        due
    }
}

/// The stock pager keeps the highest revision it holds for a run, across
/// leader reconnects, and drops a lower one (probed against v1.0.46). A fresh
/// observer after resume or re-attach therefore starts from this count of
/// facts that only grow over a run's life: stage, request and plugin-call
/// totals, their terminal outcomes, and the run's own terminal status. It
/// increases with every change the projection can show, so it never falls
/// below what an earlier observer delivered for the same durable state.
fn durable_revision(view: &GraphRunView) -> u64 {
    let stages: usize = view
        .stages
        .iter()
        .map(|stage| stage.total + stage.succeeded + stage.failed)
        .sum();
    let calls: usize = [&view.requests, &view.plugin_calls]
        .into_iter()
        .map(|calls| calls.len() + calls.iter().filter(|call| call.terminal).count())
        .sum();
    1 + (stages + calls + usize::from(view.is_terminal())) as u64
}

async fn send(
    session: &str,
    update: Value,
    sender: &PromptSender,
    projections: &ProjectionEngine,
) -> Result<()> {
    projections
        .session_updates()
        .send(
            session,
            |event_id, total_tokens| {
                Ok(super::projection::session_notification_for_method(
                    "x.ai/session_notification",
                    session,
                    update,
                    super::projection::stamp_update_meta(
                        event_id,
                        total_tokens,
                        None,
                        None,
                        UpdateTimestamps::default(),
                    ),
                ))
            },
            PromptSenderLine(sender),
        )
        .await
        .map(drop)
}

/// The pager ticks a running run's clock itself; elapsed time and the
/// delivery counter alone never justify another update.
fn without_clock(update: &Value) -> Value {
    let mut shape = update.clone();
    if let Some(object) = shape.as_object_mut() {
        object.remove("revision");
        object.remove("elapsed_ms");
    }
    shape
}

fn elapsed_ms(view: &GraphRunView, now: DateTime<Utc>) -> u64 {
    let parse = |text: &str| DateTime::parse_from_rfc3339(text).ok();
    let Some(start) = view
        .started_at
        .as_deref()
        .and_then(parse)
        .or_else(|| parse(&view.created_at))
    else {
        return 0;
    };
    let end = if view.is_terminal() {
        view.completed_at.as_deref().and_then(parse)
    } else {
        Some(now.fixed_offset())
    };
    end.map_or(0, |end| (end - start).num_milliseconds().max(0) as u64)
}

fn objective(graph_id: &str) -> String {
    format!("Gents graph run: {graph_id}")
}

/// Stages are phases and model requests are agents. The pager's phase
/// vocabulary is pending/active/done: a failed or cancelled run says so in
/// `status` and `pause_message`, and a failed request in its agent state.
/// An agent label names its own request, so a sibling that appears later
/// never renames a row the pager already shows.
fn project(
    view: &GraphRunView,
    status: GraphRunStatus,
    revision: u64,
    now: DateTime<Utc>,
) -> Value {
    let phases = view
        .stages
        .iter()
        .map(|stage| {
            let state = if stage.active > 0 {
                "active"
            } else if stage.total > 0 {
                "done"
            } else {
                "pending"
            };
            json!({"title": stage.node_id, "state": state})
        })
        .collect::<Vec<_>>();
    let agents = view
        .requests
        .iter()
        .filter_map(|request| {
            let node_id = request.node_id.as_deref()?;
            let state = match (request.terminal, request.succeeded) {
                (false, _) => "active",
                (true, true) => "done",
                (true, false) => "failed",
            };
            let prefix = request.request_id.chars().take(8).collect::<String>();
            Some(json!({
                "agent_id": request.request_id,
                "label": format!("{node_id}-{prefix}"),
                "phase": node_id,
                "state": state,
            }))
        })
        .collect::<Vec<_>>();
    let mut update = json!({
        "sessionUpdate": "workflow_updated",
        "run_id": view.run_id,
        "revision": revision,
        "name": view.graph_id,
        "objective": objective(&view.graph_id),
        "status": match status {
            GraphRunStatus::Running => "active",
            GraphRunStatus::Succeeded => "complete",
            GraphRunStatus::Failed | GraphRunStatus::Cancelled => "failed",
        },
        "foreground": false,
        "phases": phases,
        "elapsed_ms": elapsed_ms(view, now),
        "active_agents": view.requests.iter().filter(|request| !request.terminal).count(),
        "agents": agents,
    });
    let failed_stages = view
        .stages
        .iter()
        .filter(|stage| stage.failed > 0)
        .map(|stage| stage.node_id.as_str())
        .collect::<Vec<_>>();
    let message = match status {
        GraphRunStatus::Failed if failed_stages.is_empty() => Some("Graph run failed".to_owned()),
        GraphRunStatus::Failed => Some(format!("Graph run failed: {}", failed_stages.join(", "))),
        GraphRunStatus::Cancelled => Some(match view.cancellation_reason.as_deref() {
            Some(reason) if !reason.is_empty() => format!("Graph run cancelled: {reason}"),
            _ => "Graph run cancelled".to_owned(),
        }),
        GraphRunStatus::Running | GraphRunStatus::Succeeded => None,
    };
    if let Some(message) = message {
        update["pause_message"] = json!(message);
    }
    update
}

/// The one terminal update for a run this connection stops observing, so
/// the pager does not keep ticking a run nothing reports on. It continues
/// the last delivered update when there is one.
fn abandoned(
    run_id: &str,
    run: &RunObservation,
    error: &anyhow::Error,
    now: DateTime<Utc>,
) -> Value {
    let mut update = match &run.delivered {
        Some(delivered) => {
            let mut update = delivered.update.clone();
            let shown = update["elapsed_ms"].as_u64().unwrap_or_default();
            let since = (now - delivered.at).num_milliseconds().max(0) as u64;
            update["elapsed_ms"] = json!(shown + since);
            update
        }
        None => json!({
            "sessionUpdate": "workflow_updated",
            "run_id": run_id,
            "name": run.graph_id,
            "objective": objective(&run.graph_id),
            "foreground": false,
            "phases": [],
            "elapsed_ms": 0,
            "agents": [],
        }),
    };
    update["revision"] = json!(run.revision);
    update["status"] = json!("failed");
    update["active_agents"] = json!(0);
    update["pause_message"] = json!(format!("Gents stopped observing this graph run: {error}"));
    update
}

#[cfg(test)]
mod tests {
    use super::super::projection::BoundModelContext;
    use super::super::test_fixtures::seed_canonical_tool_call;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NOW: &str = "2026-10-02T05:30:20Z";
    const PLAN: &str = "7c1e9a40-1f2e-4d3c-8b4a-000000000001";
    const RESEARCH_1: &str = "2b8d4f11-1f2e-4d3c-8b4a-000000000002";
    const RESEARCH_2: &str = "9e3a6c22-1f2e-4d3c-8b4a-000000000003";
    const SYNTHESIS: &str = "04d7b5e3-1f2e-4d3c-8b4a-000000000004";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn request(id: &str, node: &str, terminal: bool, succeeded: bool) -> Value {
        json!({"request_id": id, "session_id": format!("child-{id}"), "node_id": node,
            "agent_id": "worker", "lifecycle_state": null, "failure_reason": null,
            "terminal": terminal, "succeeded": succeeded})
    }

    fn stage(node: &str, total: usize, active: usize, succeeded: usize, failed: usize) -> Value {
        json!({"node_id": node, "total": total, "active": active,
            "succeeded": succeeded, "failed": failed})
    }

    fn view(
        status: &str,
        stages: Vec<Value>,
        requests: Vec<Value>,
        completed_at: Option<&str>,
    ) -> GraphRunView {
        serde_json::from_value(json!({
            "view_version": 1, "run_id": "run-1", "graph_id": "research-graph",
            "revision_digest": "sha256:d", "owner_did": "did:test:grok-shim",
            "caller_did": "did:test:grok-shim", "entry_name": "entry", "correlation": "corr",
            "status": status, "input": {}, "created_at": "2026-10-02T05:30:00Z",
            "started_at": "2026-10-02T05:30:00Z", "completed_at": completed_at,
            "update_generation": 1, "requests": requests, "stages": stages,
            "groups": [], "results": [], "persisted_result_refs": [],
            "active_request_count": 0, "terminal_request_count": 0,
            "result_contract_satisfied": false, "failure_evidence": null,
        }))
        .unwrap()
    }

    fn running_view() -> GraphRunView {
        view(
            "running",
            vec![
                stage("plan", 1, 0, 1, 0),
                stage("research", 2, 1, 1, 0),
                stage("synthesis", 0, 0, 0, 0),
            ],
            vec![
                request(PLAN, "plan", true, true),
                request(RESEARCH_1, "research", true, true),
                request(RESEARCH_2, "research", false, false),
            ],
            None,
        )
    }

    fn project_view(view: &GraphRunView, revision: u64, at: DateTime<Utc>) -> Value {
        project(view, view.status().unwrap(), revision, at)
    }

    fn assert_emitted(name: &str, actual: &Value, expected: Value) {
        assert_eq!(actual, &expected, "{name}");
    }

    #[test]
    fn running_run_projects_pending_active_and_done_stages() {
        assert_emitted(
            "workflow-updated-running",
            &project_view(&running_view(), 1, now()),
            json!({
                "sessionUpdate": "workflow_updated", "run_id": "run-1", "revision": 1,
                "name": "research-graph", "objective": "Gents graph run: research-graph",
                "status": "active", "foreground": false,
                "phases": [
                    {"title": "plan", "state": "done"},
                    {"title": "research", "state": "active"},
                    {"title": "synthesis", "state": "pending"},
                ],
                "elapsed_ms": 20000, "active_agents": 1,
                "agents": [
                    {"agent_id": PLAN, "label": "plan-7c1e9a40", "phase": "plan", "state": "done"},
                    {"agent_id": RESEARCH_1, "label": "research-2b8d4f11", "phase": "research", "state": "done"},
                    {"agent_id": RESEARCH_2, "label": "research-9e3a6c22", "phase": "research", "state": "active"},
                ],
            }),
        );
    }

    /// The view orders requests by id, which is not creation order, so a new
    /// sibling can sort before the rows the pager already shows.
    #[test]
    fn a_new_sibling_never_renames_shown_agents() {
        let labels = |update: &Value| {
            update["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|agent| {
                    (
                        agent["agent_id"].as_str().unwrap().to_owned(),
                        agent["label"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let before = labels(&project_view(&running_view(), 1, now()));
        let earlier = "00000000-1f2e-4d3c-8b4a-000000000005";
        let mut grown = running_view();
        grown.stages[1] = serde_json::from_value(stage("research", 3, 2, 1, 0)).unwrap();
        grown.requests.insert(
            0,
            serde_json::from_value(request(earlier, "research", false, false)).unwrap(),
        );
        let after = labels(&project_view(&grown, 2, now()));
        for (agent_id, label) in &before {
            assert_eq!(&after[agent_id], label, "{agent_id}");
        }
        assert_eq!(after[earlier], "research-00000000");
    }

    #[test]
    fn succeeded_run_is_complete_with_a_frozen_clock() {
        let done = view(
            "succeeded",
            vec![stage("plan", 1, 0, 1, 0)],
            vec![request(PLAN, "plan", true, true)],
            Some("2026-10-02T05:30:07Z"),
        );
        let later = now() + Duration::seconds(60);
        assert_emitted(
            "workflow-updated-complete",
            &project_view(&done, 4, later),
            json!({
                "sessionUpdate": "workflow_updated", "run_id": "run-1", "revision": 4,
                "name": "research-graph", "objective": "Gents graph run: research-graph",
                "status": "complete", "foreground": false,
                "phases": [{"title": "plan", "state": "done"}],
                "elapsed_ms": 7000, "active_agents": 0,
                "agents": [{"agent_id": PLAN, "label": "plan-7c1e9a40", "phase": "plan", "state": "done"}],
            }),
        );
    }

    #[test]
    fn failed_and_cancelled_runs_say_so_in_status_and_message() {
        let mut failed = view(
            "failed",
            vec![
                stage("research", 2, 0, 1, 1),
                stage("synthesis", 0, 0, 0, 0),
            ],
            vec![
                request(RESEARCH_1, "research", true, false),
                request(RESEARCH_2, "research", true, true),
            ],
            Some("2026-10-02T05:30:09Z"),
        );
        assert_emitted(
            "workflow-updated-failed",
            &project_view(&failed, 3, now()),
            json!({
                "sessionUpdate": "workflow_updated", "run_id": "run-1", "revision": 3,
                "name": "research-graph", "objective": "Gents graph run: research-graph",
                "status": "failed", "foreground": false,
                "phases": [
                    {"title": "research", "state": "done"},
                    {"title": "synthesis", "state": "pending"},
                ],
                "elapsed_ms": 9000, "active_agents": 0,
                "agents": [
                    {"agent_id": RESEARCH_1, "label": "research-2b8d4f11", "phase": "research", "state": "failed"},
                    {"agent_id": RESEARCH_2, "label": "research-9e3a6c22", "phase": "research", "state": "done"},
                ],
                "pause_message": "Graph run failed: research",
            }),
        );
        failed.status = "cancelled".into();
        failed.cancellation_reason = Some("operator stop".into());
        let cancelled = project_view(&failed, 3, now());
        assert_emitted(
            "workflow-updated-cancelled-pause-message",
            &json!({"status": cancelled["status"], "pause_message": cancelled["pause_message"]}),
            json!({"status": "failed", "pause_message": "Graph run cancelled: operator stop"}),
        );
    }

    async fn harness() -> (
        tempfile::TempDir,
        Arc<EmbeddedNode>,
        ProjectionEngine,
        Arc<tokio::sync::Mutex<Vec<String>>>,
        PromptSender,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        gents::schema::ensure_runtime_schemas(&node).await.unwrap();
        let projections = ProjectionEngine::new(
            node.clone(),
            BoundModelContext::new("model".into(), "Model".into(), 1000),
        );
        let buffer = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        (directory, node, projections, buffer, sender)
    }

    async fn observe_view(
        cursor: &mut GraphRunCursor,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        loads: &AtomicUsize,
        view: GraphRunView,
        at: DateTime<Utc>,
    ) {
        let reports = cursor
            .observe("session", sender, projections, at, |_| {
                loads.fetch_add(1, Ordering::SeqCst);
                let view = view.clone();
                async move { Ok(view) }
            })
            .await;
        assert!(reports.is_empty(), "{reports:?}");
    }

    async fn updates(buffer: &tokio::sync::Mutex<Vec<String>>) -> Vec<Value> {
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

    fn changed_view() -> GraphRunView {
        let mut current = running_view();
        current.stages[2] = serde_json::from_value(stage("synthesis", 1, 1, 0, 0)).unwrap();
        current.requests[2] =
            serde_json::from_value(request(RESEARCH_2, "research", true, true)).unwrap();
        current.stages[1] = serde_json::from_value(stage("research", 2, 0, 2, 0)).unwrap();
        current
            .requests
            .push(serde_json::from_value(request(SYNTHESIS, "synthesis", false, false)).unwrap());
        current
    }

    fn completed_view() -> GraphRunView {
        let mut current = changed_view();
        current.status = "succeeded".into();
        current.completed_at = Some("2026-10-02T05:30:25Z".into());
        current.stages[2] = serde_json::from_value(stage("synthesis", 1, 0, 1, 0)).unwrap();
        current.requests[3] =
            serde_json::from_value(request(SYNTHESIS, "synthesis", true, true)).unwrap();
        current
    }

    #[tokio::test]
    async fn observation_emits_only_changes_and_stops_at_a_terminal_view() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let mut cursor = GraphRunCursor::default();
        cursor
            .runs
            .insert("run-1".into(), RunObservation::new("research-graph"));
        let loads = AtomicUsize::new(0);
        macro_rules! step {
            ($view:expr, $at:expr) => {
                observe_view(&mut cursor, &sender, &projections, &loads, $view, $at).await
            };
        }
        step!(running_view(), now());
        // Only the clock moved.
        step!(running_view(), now() + Duration::seconds(1));
        let first = updates(&buffer).await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["revision"], 11);

        step!(changed_view(), now() + Duration::seconds(2));
        let second = updates(&buffer).await;
        assert_eq!(second.len(), 2);
        assert_emitted(
            "workflow-updated-changed",
            &second[1],
            json!({
                "sessionUpdate": "workflow_updated", "run_id": "run-1", "revision": 15,
                "name": "research-graph", "objective": "Gents graph run: research-graph",
                "status": "active", "foreground": false,
                "phases": [
                    {"title": "plan", "state": "done"},
                    {"title": "research", "state": "done"},
                    {"title": "synthesis", "state": "active"},
                ],
                "elapsed_ms": 22000, "active_agents": 1,
                "agents": [
                    {"agent_id": PLAN, "label": "plan-7c1e9a40", "phase": "plan", "state": "done"},
                    {"agent_id": RESEARCH_1, "label": "research-2b8d4f11", "phase": "research", "state": "done"},
                    {"agent_id": RESEARCH_2, "label": "research-9e3a6c22", "phase": "research", "state": "done"},
                    {"agent_id": SYNTHESIS, "label": "synthesis-04d7b5e3", "phase": "synthesis", "state": "active"},
                ],
            }),
        );

        step!(completed_view(), now() + Duration::seconds(3));
        let third = updates(&buffer).await;
        assert_eq!(third.len(), 3);
        assert_eq!(third[2]["revision"], 18);
        assert_eq!(third[2]["status"], "complete");
        assert_eq!(third[2]["elapsed_ms"], 25000);

        let loads_at_terminal = loads.load(Ordering::SeqCst);
        step!(completed_view(), now() + Duration::seconds(30));
        assert_eq!(loads.load(Ordering::SeqCst), loads_at_terminal);
        assert_eq!(updates(&buffer).await.len(), 3);
    }

    /// The stock pager drops a revision lower than the one it holds, also
    /// after the leader reconnects, so an observer created on resume or
    /// re-attach must continue from durable state rather than from 1.
    #[tokio::test]
    async fn a_fresh_observer_continues_the_revision_from_durable_state() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let loads = AtomicUsize::new(0);
        let mut first = GraphRunCursor::default();
        first
            .runs
            .insert("run-1".into(), RunObservation::new("research-graph"));
        observe_view(
            &mut first,
            &sender,
            &projections,
            &loads,
            running_view(),
            now(),
        )
        .await;
        observe_view(
            &mut first,
            &sender,
            &projections,
            &loads,
            changed_view(),
            now(),
        )
        .await;
        for view in [changed_view(), completed_view()] {
            let mut fresh = GraphRunCursor::default();
            fresh
                .runs
                .insert("run-1".into(), RunObservation::new("research-graph"));
            observe_view(&mut fresh, &sender, &projections, &loads, view, now()).await;
        }
        let revisions = updates(&buffer)
            .await
            .iter()
            .map(|update| update["revision"].as_u64().unwrap())
            .collect::<Vec<_>>();
        // A fresh observer of unchanged state repeats the held revision; of a
        // later state, it exceeds it.
        assert_eq!(revisions, [11, 15, 15, 18]);
    }

    /// A run that cannot be read for a while is read again later, ever more
    /// rarely, and is never given up while the session is attached.
    #[tokio::test]
    async fn a_transient_failure_backs_off_and_is_never_abandoned() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let mut cursor = GraphRunCursor::default();
        cursor
            .runs
            .insert("run-1".into(), RunObservation::new("research-graph"));
        let loads = AtomicUsize::new(0);
        let mut reports = Vec::new();
        let mut delays = Vec::new();
        let mut at = now();
        for _ in 0..31 {
            let failing = |_| {
                loads.fetch_add(1, Ordering::SeqCst);
                async { Err::<GraphRunView, _>(anyhow::anyhow!("storage unavailable")) }
            };
            reports.extend(
                cursor
                    .observe("session", &sender, &projections, at, failing)
                    .await,
            );
            let retry_at = cursor.runs["run-1"].backoff.retry_at.unwrap();
            delays.push((retry_at - at).num_seconds());
            // Not read again before its delay has passed.
            let reads = loads.load(Ordering::SeqCst);
            let early = retry_at - Duration::milliseconds(1);
            assert!(cursor
                .observe("session", &sender, &projections, early, failing)
                .await
                .is_empty());
            assert_eq!(loads.load(Ordering::SeqCst), reads);
            at = retry_at;
        }
        assert_eq!(loads.load(Ordering::SeqCst), 31);
        assert_eq!(delays[..7], [1, 2, 4, 8, 16, 30, 30]);
        assert!(delays.iter().all(|delay| *delay <= MAX_RETRY_DELAY_SECS));
        let reports = reports
            .iter()
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        assert_eq!(reports, ["graph run run-1: retrying: storage unavailable"]);
        assert!(!cursor.runs["run-1"].finished);
        assert!(buffer.lock().await.is_empty());

        let reports = cursor
            .observe("session", &sender, &projections, at, |_| async {
                Ok(running_view())
            })
            .await;
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(cursor.runs["run-1"].backoff.failures, 0);
        let sent = updates(&buffer).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["revision"], 11);
        assert_eq!(sent[0]["status"], "active");
    }

    /// A missing or denied run, or a status outside the vocabulary, cannot
    /// recover: observation ends at once with one terminal update, and other
    /// runs are unaffected.
    #[tokio::test]
    async fn a_permanent_failure_ends_observation_with_one_terminal_update() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let mut cursor = GraphRunCursor::default();
        for run in [
            "missing-run",
            "run-1",
            "shown-then-denied",
            "unknown-status",
        ] {
            cursor
                .runs
                .insert(run.into(), RunObservation::new("research-graph"));
        }
        let loads = std::sync::Mutex::new(BTreeMap::<String, u32>::new());
        let mut reports = Vec::new();
        for tick in 0..5 {
            let at = now() + Duration::seconds(tick);
            reports.extend(
                cursor
                    .observe("session", &sender, &projections, at, |run_id| {
                        let reads = {
                            let mut loads = loads.lock().unwrap();
                            let reads = loads.entry(run_id.clone()).or_default();
                            *reads += 1;
                            *reads
                        };
                        async move {
                            let mut view = running_view();
                            view.run_id = run_id.clone();
                            match run_id.as_str() {
                                "missing-run" => {
                                    return Err(anyhow::Error::from(
                                        GraphRunUnobservable::Missing {
                                            run_id: run_id.clone(),
                                        },
                                    ))
                                }
                                "shown-then-denied" if reads > 1 => {
                                    return Err(anyhow::Error::from(
                                        GraphRunUnobservable::Unauthorized,
                                    ))
                                }
                                "unknown-status" => view.status = "paused".into(),
                                _ => {}
                            }
                            Ok(view)
                        }
                    })
                    .await,
            );
        }
        let loads = loads.into_inner().unwrap();
        assert_eq!(loads["missing-run"], 1);
        assert_eq!(loads["unknown-status"], 1);
        assert_eq!(loads["shown-then-denied"], 2);
        assert_eq!(loads["run-1"], 5);
        let reports = reports
            .iter()
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        assert_eq!(
            reports,
            [
                r#"graph run missing-run: observation stopped: GraphRun "missing-run" does not exist"#,
                r#"graph run unknown-status: observation stopped: unrecognized persisted graph run status "paused""#,
                "graph run shown-then-denied: observation stopped: actor is not authorized to observe this graph run",
            ]
        );
        assert!(["missing-run", "shown-then-denied", "unknown-status"]
            .iter()
            .all(|run| cursor.runs[*run].finished));
        assert!(!cursor.runs["run-1"].finished);

        let sent = updates(&buffer).await;
        let for_run = |run: &str| {
            sent.iter()
                .filter(|update| update["run_id"] == run)
                .collect::<Vec<_>>()
        };
        assert_eq!(for_run("run-1").len(), 1);
        assert_eq!(for_run("unknown-status").len(), 1);
        assert_emitted(
            "workflow-updated-unobservable",
            for_run("missing-run")[0],
            json!({
                "sessionUpdate": "workflow_updated", "run_id": "missing-run",
                "revision": ABANDONED_REVISION,
                "name": "research-graph", "objective": "Gents graph run: research-graph",
                "status": "failed", "foreground": false, "phases": [],
                "elapsed_ms": 0, "active_agents": 0, "agents": [],
                "pause_message": r#"Gents stopped observing this graph run: GraphRun "missing-run" does not exist"#,
            }),
        );
        let denied = for_run("shown-then-denied");
        assert_eq!(denied.len(), 2);
        let mut expected = denied[0].clone();
        expected["revision"] = json!(ABANDONED_REVISION);
        expected["elapsed_ms"] = json!(21000);
        expected["status"] = json!("failed");
        expected["active_agents"] = json!(0);
        expected["pause_message"] = json!(
            "Gents stopped observing this graph run: actor is not authorized to observe this graph run"
        );
        assert_emitted("workflow-updated-stopped", denied[1], expected);
    }

    #[tokio::test]
    async fn view_loads_per_tick_are_capped_and_round_robin() {
        let (_directory, _node, projections, _buffer, sender) = harness().await;
        let mut cursor = GraphRunCursor::default();
        let runs = (0..10)
            .map(|run| format!("run-{run:02}"))
            .collect::<Vec<_>>();
        for run in &runs {
            cursor
                .runs
                .insert(run.clone(), RunObservation::new("research-graph"));
        }
        let mut ticks = Vec::new();
        for _ in 0..3 {
            let loaded = std::sync::Mutex::new(Vec::new());
            let reports = cursor
                .observe("session", &sender, &projections, now(), |run_id| {
                    loaded.lock().unwrap().push(run_id.clone());
                    async move {
                        let mut view = running_view();
                        view.run_id = run_id;
                        Ok(view)
                    }
                })
                .await;
            assert!(reports.is_empty(), "{reports:?}");
            ticks.push(loaded.into_inner().unwrap());
        }
        let expected = [[0, 1, 2, 3], [4, 5, 6, 7], [8, 9, 0, 1]]
            .map(|tick| tick.map(|run| runs[run].clone()).to_vec())
            .to_vec();
        assert_eq!(ticks, expected);
        // 10 runs at 4 loads per tick are all covered within 3 ticks.
        assert_eq!(
            ticks.concat().into_iter().collect::<HashSet<_>>().len(),
            runs.len()
        );
    }

    async fn seed_request(
        node: &EmbeddedNode,
        id: &str,
        session: &str,
        principal: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let principal = gents::graphql::escape_graphql_string(principal);
        let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", request_id: "{id}", session_id: "{session}", node_did: "{principal}", requester_did: "{principal}", agent_id: "test", content: "test", lifecycle_state: "pending"}}) {{ _docID }} }}"#)).await;
        gents::graphql::ensure_no_errors(&response, "seed request").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data": response.data}),
            "AgentRequest",
        )
        .unwrap();
        serde_json::from_value(
            json!({"_docID": doc, "request_id": id, "session_id": session,
            "node_did": principal, "requester_did": principal}),
        )
        .unwrap()
    }

    fn run_graph_result(run_id: &str) -> String {
        let mut observed = running_view();
        observed.run_id = run_id.into();
        let receipt = gents::graph_pipeline::GraphRunReceipt {
            run_id: run_id.into(),
            graph_id: observed.graph_id.clone(),
            revision_digest: observed.revision_digest.clone(),
            entry_name: observed.entry_name.clone(),
            correlation: observed.correlation.clone(),
            seed_doc_id: "seed".into(),
        };
        gents::graph_pipeline::run_graph_tool_result(
            "did:test:grok-shim",
            &receipt,
            &observed,
            json!({}),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_session_without_a_linked_run_emits_nothing() {
        let (_directory, node, projections, buffer, sender) = harness().await;
        let principal = "did:test:grok-shim";
        let mine = seed_request(&node, "req-mine", "session", principal).await;
        let other = seed_request(&node, "req-other", "other-session", principal).await;
        for (request, call, tool, result) in [
            (&mine, "call-bash", "bash", Some("ok".to_owned())),
            (
                &mine,
                "call-failed-start",
                "run_graph",
                Some("graph run failed to start".to_owned()),
            ),
            (&mine, "call-pending", "run_graph", None),
            (
                &other,
                "call-foreign",
                "run_graph",
                Some(run_graph_result("foreign-run")),
            ),
        ] {
            let lifecycle = if result.is_some() {
                "completed"
            } else {
                "running"
            };
            seed_canonical_tool_call(
                &node,
                request,
                call,
                tool,
                lifecycle,
                "{}",
                result.as_deref(),
                None,
                None,
                None,
            )
            .await;
        }
        let mut cursor = GraphRunCursor::default();
        let reports = cursor.discover(&node, principal, "session", now()).await;
        assert!(reports.is_empty(), "{reports:?}");
        assert!(cursor.runs.is_empty());
        let loads = AtomicUsize::new(0);
        let reports = cursor
            .observe("session", &sender, &projections, now(), |_| {
                loads.fetch_add(1, Ordering::SeqCst);
                async { Ok(running_view()) }
            })
            .await;
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(loads.load(Ordering::SeqCst), 0);
        assert!(buffer.lock().await.is_empty());
    }

    #[tokio::test]
    async fn discovery_links_runs_from_delivered_run_graph_results() {
        let (_directory, node, _projections, _buffer, _sender) = harness().await;
        let principal = "did:test:grok-shim";
        let request = seed_request(&node, "req-mine", "session", principal).await;
        let mut cursor = GraphRunCursor::default();
        seed_canonical_tool_call(
            &node,
            &request,
            "call-late",
            "run_graph",
            "running",
            "{}",
            None,
            None,
            None,
            None,
        )
        .await;
        assert!(cursor
            .discover(&node, principal, "session", now())
            .await
            .is_empty());
        assert!(cursor.runs.is_empty() && cursor.scanned.is_empty());
        let delivered = run_graph_result("run-late");
        seed_canonical_tool_call(
            &node,
            &request,
            "call-run",
            "run_graph",
            "completed",
            "{}",
            Some(&delivered),
            None,
            None,
            None,
        )
        .await;
        assert!(cursor
            .discover(&node, principal, "session", now())
            .await
            .is_empty());
        assert_eq!(cursor.runs.keys().collect::<Vec<_>>(), ["run-late"]);
        assert_eq!(cursor.runs["run-late"].graph_id, "research-graph");
        assert_eq!(cursor.scanned.len(), 1);
    }

    fn key_identity() -> gents::KeyIdentity {
        let directory = tempfile::tempdir().unwrap();
        gents::KeyIdentity::load_or_create(directory.path().join("owner.key"), None).unwrap()
    }

    /// Publishes and starts a one-stage graph through the public graph
    /// owners, as the graph_pipeline runtime tests do.
    async fn start_real_run(
        node: &Arc<EmbeddedNode>,
        owner: &str,
    ) -> gents::graph_pipeline::GraphRunReceipt {
        use gents::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use gents::graph_pipeline::*;
        use gents::Collection;
        for schema in [
            "type PipelineInput { graph_run_id: String @index(unique: true) payload: String }",
            "type PipelineResult { graph_run_id: String @index report: String }",
        ] {
            node.add_schema(schema).await.unwrap();
        }
        gents::ensure_node(node, owner).await.unwrap();
        let plan = DesiredStateApplyPlan::new(
            [
                (Collection::Agent, json!({"node_did": owner, "agent_id": "worker", "context_id": "worker:context", "inference_profile_id": "worker:inference"})),
                (Collection::AgentContext, json!({"node_did": owner, "context_id": "worker:context", "tools_id": "worker:tools"})),
                (Collection::Tools, json!({"node_did": owner, "tools_id": "worker:tools"})),
                (Collection::InferenceProfile, json!({"node_did": owner, "profile_id": "worker:inference", "backend_id": "worker:backend", "model_name": "test-model"})),
                (Collection::InferenceBackend, json!({"node_did": owner, "backend_id": "worker:backend", "name": "Test inference", "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1", "auth": {"kind": "unauthenticated"}})),
                (Collection::Task, json!({"node_did": owner, "task_id": "worker-task", "agent_id": "worker", "prompt_template": "operator approved prompt", "enabled": true})),
            ]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
        )
        .unwrap();
        ConfigAccess::transact_local(node, None, "grok.graph.fixture", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(drop) })
        })
        .await
        .unwrap();
        gents::backend_registry::set_backend_probe_status(node, owner, "worker:backend", "healthy")
            .await
            .unwrap();
        let port = |name: &str, collection: &str, required: bool| PortSpec {
            name: name.to_owned(),
            collection: collection.to_owned(),
            schema: format!("{collection}/v1"),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required,
        };
        let (input, output) = (
            port("input", "PipelineInput", true),
            port("result", "PipelineResult", false),
        );
        let worker = |port: &PortSpec| PortRef {
            node_id: "worker".to_owned(),
            port: port.name.clone(),
        };
        let plan = compile_graph(
            &GraphIntent {
                node_did: owner.to_owned(),
                tags: vec![],
                graph_id: "pipeline".to_owned(),
                nodes: vec![GraphNode {
                    session: None,
                    node_id: "worker".to_owned(),
                    capability_id: "worker".to_owned(),
                    capability_revision: "v1".to_owned(),
                }],
                edges: vec![],
                entries: vec![EntryBinding {
                    name: "input".to_owned(),
                    collection: input.collection.clone(),
                    schema: input.schema.clone(),
                    input_contract: None,
                    input_schema: None,
                    prepare: None,
                    to: worker(&input),
                }],
                results: vec![ResultContract {
                    name: "result".to_owned(),
                    from: worker(&output),
                    cardinality: ResultCardinality::Exactly { count: 1 },
                    terminal: true,
                }],
                limits: GraphLimits {
                    max_nodes: 2,
                    max_edges: 2,
                    max_depth: 2,
                    max_fan_out: 2,
                    max_total_invocations: 2,
                    max_runtime_secs: 60,
                },
            },
            &[StageCapability {
                node_did: owner.to_owned(),
                tags: vec![],
                workspace_authority: None,
                capability_id: "worker".to_owned(),
                revision: "v1".to_owned(),
                target: StageTarget::Task {
                    task_id: "worker-task".to_owned(),
                },
                input_ports: vec![input.clone()],
                output_ports: vec![output.clone()],
                allowed_callers: vec![owner.to_owned()],
            }],
            owner,
            &CompilerPolicy::default(),
        )
        .unwrap();
        materialize_graph_revision(node, None, owner, &plan)
            .await
            .unwrap();
        activate_graph_revision(node, None, owner, "pipeline", &plan.digest, None)
            .await
            .unwrap();
        start_graph_run(
            node,
            None,
            owner,
            "pipeline",
            None,
            "input",
            json!({"payload": "review"}),
            EntryInputOrigin::Operator,
        )
        .await
        .unwrap()
    }

    /// A stage request the run's entry trigger caused, signed by the owner
    /// as the trigger engine signs it, and pending.
    async fn seed_stage_request(
        node: &Arc<EmbeddedNode>,
        owner: &gents::KeyIdentity,
        receipt: &gents::graph_pipeline::GraphRunReceipt,
        request_id: &str,
    ) -> gents::RequestLifecycle {
        use gents::NodeIdentity;
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        let did = owner.did();
        let triggers = node
            .execute(&format!(
                r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID trigger_id }} }}"#,
                gents::graphql::escape_graphql_string(did),
            ))
            .await;
        gents::graphql::ensure_no_errors(&triggers, "graph trigger").unwrap();
        let triggers = triggers.data.unwrap()["Trigger"].clone();
        let [trigger] = triggers.as_array().unwrap().as_slice() else {
            panic!("one graph entry trigger: {triggers}");
        };
        let trigger_id = trigger["trigger_id"].as_str().unwrap();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            did,
            did,
            "worker",
            format!("session-{request_id}"),
            "Execute the pinned graph stage",
            "scheduled",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::runtime_automated_trigger(did, trigger_id),
        );
        create.caused_by_trigger_doc_id = trigger["_docID"].as_str().map(str::to_owned);
        create.caused_by_trigger_id = Some(trigger_id.to_owned());
        create.caused_by_trigger_kind = Some("event".into());
        create.caused_by_correlation = Some(receipt.correlation.clone());
        create.caused_by_source_doc_id = Some(receipt.seed_doc_id.clone());
        gents::sign_agent_request_create(owner, &mut create)
            .await
            .unwrap();
        let created = node.execute(&create.graphql_mutation().unwrap()).await;
        gents::graphql::ensure_no_errors(&created, "stage request").unwrap();
        let row = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                gents::graphql::escape_graphql_string(request_id),
                gents::SIGNED_REQUEST_FIELDS,
            ))
            .await;
        gents::graphql::ensure_no_errors(&row, "stage request row").unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row.data.unwrap()["AgentRequest"][0].clone()).unwrap();
        gents::RequestLifecycle::new_with_node_did(
            node.clone(),
            "worker",
            did,
            gents::AgentRequest::try_from(row).unwrap(),
            300,
        )
    }

    /// The real `run_graph` reply writer, discovery, the canonical run
    /// loader and the projection, end to end; a poison reply beside it is
    /// reported once and deferred without hiding the run.
    #[tokio::test]
    async fn a_real_run_graph_reply_is_observed_through_the_canonical_loader() {
        let (_directory, node, projections, buffer, sender) = harness().await;
        let owner = gents::NodeIdentity::did(&key_identity()).to_owned();
        let receipt = start_real_run(&node, &owner).await;
        let access = ConfigAccess::Local(node.clone());
        let observed = load_graph_run_view_with_access(&access, &owner, &receipt.run_id)
            .await
            .unwrap();
        let reply =
            gents::graph_pipeline::run_graph_tool_result(&owner, &receipt, &observed, json!({}))
                .unwrap();
        let request = seed_request(&node, "req-run", "session", &owner).await;
        let poison = seed_canonical_tool_call(
            &node,
            &request,
            "call-poison",
            "run_graph",
            "bogus",
            "{}",
            None,
            None,
            None,
            None,
        )
        .await;
        seed_canonical_tool_call(
            &node,
            &request,
            "call-run",
            "run_graph",
            "completed",
            "{}",
            Some(&reply),
            None,
            None,
            None,
        )
        .await;

        let mut cursor = GraphRunCursor::default();
        let mut reports = Vec::new();
        for _ in 0..3 {
            reports.extend(
                cursor
                    .refresh(&node, &owner, "session", &sender, &projections)
                    .await
                    .iter()
                    .map(|error| format!("{error:#}")),
            );
        }
        let subject = format!("run_graph reply {poison}");
        assert_eq!(reports.len(), 1, "{reports:#?}");
        assert!(reports[0].starts_with(&format!("{subject}: retrying")));
        assert!(!cursor.scanned.contains(&poison));
        assert!(cursor.replies[&poison].failures >= 1);

        let mut sent = updates(&buffer).await;
        assert_eq!(sent.len(), 1, "{sent:#?}");
        sent[0].as_object_mut().unwrap().remove("elapsed_ms");
        assert_emitted(
            "workflow-updated-real-run",
            &sent[0],
            json!({
                "sessionUpdate": "workflow_updated", "run_id": receipt.run_id,
                "revision": 1, "name": "pipeline", "objective": "Gents graph run: pipeline",
                "status": "active", "foreground": false,
                "phases": [{"title": "worker", "state": "pending"}],
                "active_agents": 0, "agents": [],
            }),
        );

        // Another principal is denied by the loader, and an unknown run id
        // has no row: each is read once and ends with one terminal update.
        let mut intruder = GraphRunCursor::default();
        for run in [receipt.run_id.as_str(), "missing-run"] {
            intruder
                .runs
                .insert(run.into(), RunObservation::new("pipeline"));
        }
        let loads = AtomicUsize::new(0);
        let mut denied = Vec::new();
        for _ in 0..3 {
            denied.extend(
                intruder
                    .observe("session", &sender, &projections, now(), |run_id| {
                        loads.fetch_add(1, Ordering::SeqCst);
                        let access = &access;
                        async move {
                            load_graph_run_view_with_access(access, "did:test:intruder", &run_id)
                                .await
                        }
                    })
                    .await
                    .iter()
                    .map(|error| format!("{error:#}")),
            );
        }
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert_eq!(denied.len(), 2, "{denied:#?}");
        assert!(denied
            .iter()
            .any(|report| report.ends_with("not authorized to observe this graph run")));
        assert!(denied
            .iter()
            .any(|report| report.ends_with(r#"GraphRun "missing-run" does not exist"#)));
        assert!(intruder.runs.values().all(|run| run.finished));
        let sent = updates(&buffer).await;
        assert_eq!(sent.len(), 3);
        // A fresh cursor's stop frame must outrank what the first delivered.
        let delivered = sent[0]["revision"].as_u64().unwrap();
        assert!(sent[1..].iter().all(|update| update["status"] == "failed"
            && update["pause_message"].is_string()
            && update["revision"].as_u64().unwrap() > delivered));
    }

    /// Stage requests driven by the lifecycle owners through pending,
    /// claimed, processing and terminal (one failed, one interrupted), then
    /// the run reconciled to its terminal status: the durable revision never
    /// falls, and a fresh observer never starts below a running one.
    #[tokio::test]
    async fn a_real_runs_revision_never_decreases_across_its_lifecycle() {
        use gents::lifecycle::{ClaimOutcome, RequestTerminalOutcome};
        use gents_protocol::output::TerminalOutput;
        let (_directory, node, projections, buffer, sender) = harness().await;
        let identity = key_identity();
        let owner = gents::NodeIdentity::did(&identity).to_owned();
        let receipt = start_real_run(&node, &owner).await;
        let access = ConfigAccess::Local(node.clone());
        let mut cursor = GraphRunCursor::default();
        cursor
            .runs
            .insert(receipt.run_id.clone(), RunObservation::new("pipeline"));
        let mut steps = Vec::<(&str, u64, u64, u64)>::new();
        macro_rules! step {
            ($name:expr) => {{
                let view = load_graph_run_view_with_access(&access, &owner, &receipt.run_id)
                    .await
                    .unwrap();
                let reports = cursor
                    .observe("session", &sender, &projections, Utc::now(), |_| {
                        let view = view.clone();
                        async move { Ok(view) }
                    })
                    .await;
                assert!(reports.is_empty(), "{reports:?}");
                let mut fresh = GraphRunCursor::default();
                fresh
                    .runs
                    .insert(receipt.run_id.clone(), RunObservation::new("pipeline"));
                let silent = PromptSender::Buffer {
                    buffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                };
                let reports = fresh
                    .observe("session", &silent, &projections, Utc::now(), |_| {
                        let view = view.clone();
                        async move { Ok(view) }
                    })
                    .await;
                assert!(reports.is_empty(), "{reports:?}");
                steps.push((
                    $name,
                    durable_revision(&view),
                    cursor.runs[&receipt.run_id].revision,
                    fresh.runs[&receipt.run_id].revision,
                ));
                view
            }};
        }
        step!("started");
        let writer = gents::DefraStreamWriter::new(node.clone(), &owner, std::time::Duration::ZERO);
        for (request_id, outcome) in [
            ("stage-failed", RequestTerminalOutcome::Failed),
            ("stage-interrupted", RequestTerminalOutcome::Interrupted),
        ] {
            let mut lifecycle = seed_stage_request(&node, &identity, &receipt, request_id).await;
            step!("pending");
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            step!("claimed");
            lifecycle.begin_owned_execution(&writer).await.unwrap();
            step!("processing");
            if matches!(outcome, RequestTerminalOutcome::Interrupted) {
                gents::interrupt_request(&node, request_id).await.unwrap();
            }
            lifecycle
                .terminalize_owned(outcome, TerminalOutput::NoMessage, Some("stage ended"))
                .await
                .unwrap();
            let view = step!("terminal");
            assert!(view
                .requests
                .iter()
                .any(|request| request.request_id == request_id
                    && request.terminal
                    && !request.succeeded));
        }
        gents::graph_pipeline::reconcile_graph_run_with_access(&access, &owner, &receipt.run_id)
            .await
            .unwrap();
        let view = step!("run terminal");
        assert_eq!(view.status().unwrap(), GraphRunStatus::Failed);

        for pair in steps.windows(2) {
            let ((before, durable_before, _, _), (after, durable_after, _, _)) = (pair[0], pair[1]);
            assert!(
                durable_after >= durable_before,
                "{before} -> {after}: {steps:?}"
            );
        }
        for (name, _, running, fresh) in &steps {
            assert!(fresh >= running, "{name}: {steps:?}");
        }
        let revisions = updates(&buffer)
            .await
            .iter()
            .map(|update| update["revision"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert!(
            revisions.windows(2).all(|pair| pair[0] < pair[1]),
            "{revisions:?}"
        );
        assert_eq!(updates(&buffer).await.last().unwrap()["status"], "failed");
    }
}
