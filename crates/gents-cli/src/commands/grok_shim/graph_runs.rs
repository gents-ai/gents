use rapidhash::{RapidHashMap, RapidHashSet};
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::graph_pipeline::{
    load_graph_run_view_with_access, run_receipt_from_tool_result, GraphRunStatus,
    GraphRunUnobservable, GraphRunView,
};
use serde_json::{json, Value};

use super::observation_cursor::{
    observe_due, Backoff, ObservationSet, Observed, ObservedView, Sink,
};
use super::projection::tools::session_tool_results;
use super::projection::ProjectionEngine;
use super::turn::PromptSender;

#[derive(Default)]
pub(super) struct GraphRunCursor {
    scanned: RapidHashSet<String>,
    replies: RapidHashMap<String, Backoff>,
    discovery_failures: u32,
    runs: ObservationSet,
}

impl GraphRunCursor {
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
        let access = &ConfigAccess::local(node.clone());
        reports.extend(
            self.observe(session, sender, projections, now, |run_id| async move {
                Ok(load_graph_run_view_with_access(access, principal, &run_id).await?)
            })
            .await,
        );
        reports
    }

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
                            .insert(receipt.run_id, Observed::new(receipt.graph_id));
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
        load: F,
    ) -> Vec<anyhow::Error>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<GraphRunView>>,
    {
        let sink = Sink {
            session,
            sender,
            projections,
        };
        observe_due(
            &mut self.runs,
            &sink,
            now,
            load,
            |error| error.downcast_ref::<GraphRunUnobservable>().is_some(),
            unobserved,
            "graph run",
        )
        .await
    }
}

impl ObservedView for GraphRunView {
    fn is_terminal(&self) -> Result<bool> {
        Ok(self.status().map(GraphRunStatus::is_terminal)?)
    }

    /// Counting only facts that grow over a run's life (stage, request and
    /// plugin-call totals, their terminal outcomes, and the run's terminal
    /// status) keeps the revision from falling below what an earlier observer
    /// delivered for the same durable state.
    fn durable_revision(&self) -> u64 {
        let stages: usize = self
            .stages
            .iter()
            .map(|stage| stage.total + stage.succeeded + stage.failed)
            .sum();
        let calls: usize = [&self.requests, &self.plugin_calls]
            .into_iter()
            .map(|calls| calls.len() + calls.iter().filter(|call| call.terminal).count())
            .sum();
        1 + (stages + calls + usize::from(GraphRunView::is_terminal(self))) as u64
    }

    fn project(&self, revision: u64, now: DateTime<Utc>) -> Value {
        let status = self.status().unwrap_or(GraphRunStatus::Failed);
        project(self, status, revision, now)
    }
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

fn unobserved(run_id: &str, run: &Observed) -> Value {
    json!({
        "sessionUpdate": "workflow_updated",
        "run_id": run_id,
        "name": run.name,
        "objective": objective(&run.name),
        "foreground": false,
        "phases": [],
        "elapsed_ms": 0,
        "agents": [],
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use gents_protocol::error_chain::ErrorChain;
    use std::collections::BTreeMap;

    use super::super::observation_cursor::{ABANDONED_REVISION, MAX_RETRY_DELAY_SECS};
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
    async fn harness() -> (
        tempfile::TempDir,
        Arc<EmbeddedNode>,
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
            .insert("run-1".into(), Observed::new("research-graph"));
        let loads = AtomicUsize::new(0);
        macro_rules! step {
            ($view:expr, $at:expr) => {
                observe_view(&mut cursor, &sender, &projections, &loads, $view, $at).await
            };
        }
        step!(running_view(), now());
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

    #[tokio::test]
    async fn a_fresh_observer_continues_the_revision_from_durable_state() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let loads = AtomicUsize::new(0);
        let mut first = GraphRunCursor::default();
        first
            .runs
            .insert("run-1".into(), Observed::new("research-graph"));
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
                .insert("run-1".into(), Observed::new("research-graph"));
            observe_view(&mut fresh, &sender, &projections, &loads, view, now()).await;
        }
        let revisions = updates(&buffer)
            .await
            .iter()
            .map(|update| update["revision"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(revisions, [11, 15, 15, 18]);
    }

    #[tokio::test]
    async fn a_transient_failure_backs_off_and_is_never_abandoned() {
        let (_directory, _node, projections, buffer, sender) = harness().await;
        let mut cursor = GraphRunCursor::default();
        cursor
            .runs
            .insert("run-1".into(), Observed::new("research-graph"));
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
            let retry_at = cursor.runs.subjects["run-1"].backoff.retry_at.unwrap();
            delays.push((retry_at - at).num_seconds());
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
            .map(|error| format!("{}", ErrorChain(error.as_ref())))
            .collect::<Vec<_>>();
        assert_eq!(reports, ["graph run run-1: retrying: storage unavailable"]);
        assert!(!cursor.runs.subjects["run-1"].finished);
        assert!(buffer.lock().await.is_empty());

        let reports = cursor
            .observe("session", &sender, &projections, at, |_| async {
                Ok(running_view())
            })
            .await;
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(cursor.runs.subjects["run-1"].backoff.failures, 0);
        let sent = updates(&buffer).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["revision"], 11);
        assert_eq!(sent[0]["status"], "active");
    }

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
                .insert(run.into(), Observed::new("research-graph"));
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
            .map(|error| format!("{}", ErrorChain(error.as_ref())))
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
            .all(|run| cursor.runs.subjects[*run].finished));
        assert!(!cursor.runs.subjects["run-1"].finished);

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
                .insert(run.clone(), Observed::new("research-graph"));
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
        assert_eq!(
            ticks
                .concat()
                .into_iter()
                .collect::<RapidHashSet<_>>()
                .len(),
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
        assert!(cursor.runs.subjects.is_empty());
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
        assert!(cursor.runs.subjects.is_empty() && cursor.scanned.is_empty());
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
        assert_eq!(
            cursor.runs.subjects.keys().collect::<Vec<_>>(),
            ["run-late"]
        );
        assert_eq!(cursor.runs.subjects["run-late"].name, "research-graph");
        assert_eq!(cursor.scanned.len(), 1);
    }

    fn key_identity() -> gents::KeyIdentity {
        let directory = tempfile::tempdir().unwrap();
        gents::KeyIdentity::load_or_create(directory.path().join("owner.key"), None).unwrap()
    }

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
        ConfigAccess::transact_local_write(node, None, "grok.graph.fixture", |txn| {
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

    #[tokio::test]
    async fn a_real_run_graph_reply_is_observed_through_the_canonical_loader() {
        let (_directory, node, projections, buffer, sender) = harness().await;
        let owner = gents::NodeIdentity::did(&key_identity()).to_owned();
        let receipt = start_real_run(&node, &owner).await;
        let access = ConfigAccess::local(node.clone());
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
                    .map(|error| format!("{}", ErrorChain(error.as_ref()))),
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

        let mut intruder = GraphRunCursor::default();
        for run in [receipt.run_id.as_str(), "missing-run"] {
            intruder.runs.insert(run.into(), Observed::new("pipeline"));
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
                    .map(|error| format!("{}", ErrorChain(error.as_ref()))),
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
        assert!(intruder.runs.subjects.values().all(|run| run.finished));
        let sent = updates(&buffer).await;
        assert_eq!(sent.len(), 3);
        let delivered = sent[0]["revision"].as_u64().unwrap();
        assert!(sent[1..].iter().all(|update| update["status"] == "failed"
            && update["pause_message"].is_string()
            && update["revision"].as_u64().unwrap() > delivered));
    }

    #[tokio::test]
    async fn a_real_runs_revision_never_decreases_across_its_lifecycle() {
        use gents::lifecycle::{ClaimOutcome, RequestTerminalOutcome};
        use gents_protocol::output::TerminalOutput;
        let (_directory, node, projections, buffer, sender) = harness().await;
        let identity = key_identity();
        let owner = gents::NodeIdentity::did(&identity).to_owned();
        let receipt = start_real_run(&node, &owner).await;
        let access = ConfigAccess::local(node.clone());
        let mut cursor = GraphRunCursor::default();
        cursor
            .runs
            .insert(receipt.run_id.clone(), Observed::new("pipeline"));
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
                    .insert(receipt.run_id.clone(), Observed::new("pipeline"));
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
                    view.durable_revision(),
                    cursor.runs.subjects[&receipt.run_id].revision,
                    fresh.runs.subjects[&receipt.run_id].revision,
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
