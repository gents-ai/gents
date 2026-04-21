use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::{
    ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedRuntimeSnapshot, ResolvedSchedule,
    ResolvedTask,
};
use crate::trigger_engine::schedule_source::ScheduleSource;

/// Recorded `materialize` invocation: `(trigger_id, trigger_kind, rendered_prompt)`.
type MaterializeCall = (Option<String>, TriggerKind, String);

/// Recorded `supersede` invocation: `(trigger_id, trigger_kind)`.
type SupersedeCall = (String, TriggerKind);

/// Spy `MaterializerHandle` used by the engine tests. Records every
/// `materialize` call it sees and hands back sequentially-numbered request ids
/// so assertions can check both the call count and the rendered prompt that
/// reached the materializer.
///
/// `nonterminal_for` pre-populates the set of `(trigger_id, trigger_kind)`
/// tuples that `has_nonterminal_request_for_trigger` should report as having
/// an in-flight request. The concurrency gate tests insert tuples here to
/// simulate a prior fire that has not yet reached a terminal state.
///
/// `materialize_delay` optionally pauses inside `materialize` before recording
/// the call. Used by the `LatestOnly` serialization tests to widen the window
/// during which the per-trigger lock is held so parallel fires can be observed
/// to queue.
struct SpyMaterializer {
    materialize_calls: Arc<Mutex<Vec<MaterializeCall>>>,
    next_request_id: AtomicUsize,
    nonterminal_for: Arc<Mutex<HashSet<(String, TriggerKind)>>>,
    supersede_calls: Arc<Mutex<Vec<SupersedeCall>>>,
    materialize_delay: Mutex<Option<Duration>>,
}

impl SpyMaterializer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            materialize_calls: Arc::new(Mutex::new(Vec::new())),
            next_request_id: AtomicUsize::new(0),
            nonterminal_for: Arc::new(Mutex::new(HashSet::new())),
            supersede_calls: Arc::new(Mutex::new(Vec::new())),
            materialize_delay: Mutex::new(None),
        })
    }

    fn calls(&self) -> Vec<MaterializeCall> {
        self.materialize_calls.lock().unwrap().clone()
    }

    fn supersede_calls(&self) -> Vec<SupersedeCall> {
        self.supersede_calls.lock().unwrap().clone()
    }

    /// Pre-populate the in-flight set with `(trigger_id, trigger_kind)` so the
    /// next `has_nonterminal_request_for_trigger` call returns `true` for the
    /// matching tuple. Also makes `supersede_nonterminal_requests_for_trigger`
    /// report `1` for that tuple (and clears it, mirroring a real terminal
    /// transition) so LatestOnly tests can assert the count plumbed through.
    fn mark_nonterminal(&self, trigger_id: &str, trigger_kind: TriggerKind) {
        self.nonterminal_for
            .lock()
            .unwrap()
            .insert((trigger_id.to_owned(), trigger_kind));
    }

    /// Install a delay that `materialize` will sleep for before recording its
    /// call. Used to widen the critical section so parallel `LatestOnly`
    /// dispatches can be observed to serialize on the per-trigger lock.
    fn set_materialize_delay(&self, delay: Duration) {
        *self.materialize_delay.lock().unwrap() = Some(delay);
    }
}

impl MaterializerHandle for SpyMaterializer {
    fn materialize(
        &self,
        _task: &ResolvedTask,
        trigger_id: Option<&str>,
        trigger_kind: TriggerKind,
        rendered_prompt: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + '_>> {
        let entry = (
            trigger_id.map(str::to_owned),
            trigger_kind,
            rendered_prompt.to_owned(),
        );
        let calls = self.materialize_calls.clone();
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let delay = *self.materialize_delay.lock().unwrap();
        Box::pin(async move {
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            calls.lock().unwrap().push(entry);
            Ok(format!("req-{id}"))
        })
    }

    fn has_nonterminal_request_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        let set = self.nonterminal_for.clone();
        let key = (trigger_id.to_owned(), trigger_kind);
        Box::pin(async move { Ok(set.lock().unwrap().contains(&key)) })
    }

    fn supersede_nonterminal_requests_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + '_>> {
        let nonterm = self.nonterminal_for.clone();
        let supersede_calls = self.supersede_calls.clone();
        let key = (trigger_id.to_owned(), trigger_kind);
        Box::pin(async move {
            supersede_calls.lock().unwrap().push(key.clone());
            // Mirror a real terminal transition: the tuple is no longer
            // in-flight after supersede.
            let removed = nonterm.lock().unwrap().remove(&key);
            Ok(if removed { 1 } else { 0 })
        })
    }
}

/// Build an `ActiveRuntimeSnapshot` with the supplied active schedules and no
/// other live state. Matches the empty-defaults pattern used by
/// `runtime_snapshot::tests`.
fn snapshot_with_schedules(
    schedules: HashMap<String, ResolvedSchedule>,
) -> Arc<ActiveRuntimeSnapshot> {
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_schedules(schedules, HashSet::new());
    Arc::new(resolved.activate(1, HashMap::new()))
}

fn resolved_task(prompt_template: &str) -> ResolvedTask {
    ResolvedTask {
        task_id: "t1".to_string(),
        behavior_id: "general".to_string(),
        prompt_template: prompt_template.to_string(),
        output_schema_ref: None,
    }
}

fn resolved_schedule(schedule_id: &str, task: ResolvedTask) -> ResolvedSchedule {
    ResolvedSchedule {
        schedule_id: schedule_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        interval_secs: 60,
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    }
}

/// Create a `Schedule` document with an explicit `next_run_at`. Used by
/// `ScheduleSource::next_fire` tests to seed a due (or not-yet-due) schedule
/// without going through the full reconcile/apply pipeline.
async fn create_schedule_with_next_run_at(
    node: &defra_node::EmbeddedNode,
    schedule_id: &str,
    task_id: &str,
    next_run_at: &str,
    concurrency: &str,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_next_run_at = escape_graphql_string(next_run_at);
    let escaped_concurrency = escape_graphql_string(concurrency);
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "{escaped_schedule_id}",
                task_id: "{escaped_task_id}",
                interval_secs: 60,
                enabled: true,
                concurrency: "{escaped_concurrency}",
                next_run_at: "{escaped_next_run_at}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Schedule failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn dispatch_skips_when_schedule_not_in_active_schedules() {
    // Snapshot has NO active schedules — the incoming FireIntent's trigger_id
    // is therefore treated as disabled.
    let snapshot = snapshot_with_schedules(HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: resolved_task("anything"),
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Skipped { reason } => assert_eq!(reason, "trigger disabled"),
        other => panic!("expected Skipped {{ reason: \"trigger disabled\" }}, got {other:?}"),
    }
    assert!(
        materializer.calls().is_empty(),
        "materializer should not be called when the trigger is disabled"
    );
}

#[tokio::test]
async fn dispatch_renders_and_materializes_when_schedule_active() {
    let task = resolved_task("fired at {{ event.fired_at }}");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({"fired_at": "2026-04-21T00:00:00Z"}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Fired { request_id } => assert_eq!(request_id, "req-0"),
        other => panic!("expected Fired, got {other:?}"),
    }
    let calls = materializer.calls();
    assert_eq!(calls.len(), 1, "exactly one materialize call expected");
    let (trigger_id, kind, rendered) = &calls[0];
    assert_eq!(trigger_id.as_deref(), Some("sched-1"));
    assert_eq!(*kind, TriggerKind::Schedule);
    assert_eq!(rendered, "fired at 2026-04-21T00:00:00Z");
}

#[tokio::test]
async fn dispatch_parallel_materializes_every_intent() {
    // Two fires for the same trigger with `Parallel` concurrency. Both should
    // materialize unconditionally — the in-flight check is bypassed.
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent1 = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: task.clone(),
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };
    let intent2 = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let r1 = engine.dispatch(intent1).await;
    let r2 = engine.dispatch(intent2).await;

    assert!(
        matches!(r1, FireResult::Fired { .. }),
        "first parallel dispatch should Fire, got {r1:?}"
    );
    assert!(
        matches!(r2, FireResult::Fired { .. }),
        "second parallel dispatch should Fire, got {r2:?}"
    );
    assert_eq!(
        materializer.calls().len(),
        2,
        "both parallel fires should materialize"
    );
}

#[tokio::test]
async fn dispatch_serial_materializes_when_no_inflight() {
    // Serial mode with no in-flight request for the trigger — should fire.
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    assert!(
        matches!(result, FireResult::Fired { .. }),
        "serial dispatch with no in-flight should Fire, got {result:?}"
    );
    assert_eq!(
        materializer.calls().len(),
        1,
        "serial dispatch with no in-flight should materialize once"
    );
}

#[tokio::test]
async fn dispatch_serial_skips_when_inflight_exists() {
    // Serial mode with an in-flight request pre-populated for
    // (sched-1, Schedule). Dispatch should Skip and not materialize.
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    materializer.mark_nonterminal("sched-1", TriggerKind::Schedule);
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Skipped { reason } => {
            assert_eq!(reason, "serial: prior fire still in-flight");
        }
        other => panic!(
            "expected Skipped {{ reason: \"serial: prior fire still in-flight\" }}, got {other:?}"
        ),
    }
    assert!(
        materializer.calls().is_empty(),
        "serial dispatch with in-flight should not materialize"
    );
}

#[tokio::test]
async fn dispatch_latest_only_supersedes_prior_and_fires_new() {
    // LatestOnly with a pre-existing in-flight request for (sched-1, Schedule).
    // Dispatch should: (1) supersede the prior request, (2) materialize the
    // new fire, (3) return Fired.
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    materializer.mark_nonterminal("sched-1", TriggerKind::Schedule);
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    assert!(
        matches!(result, FireResult::Fired { .. }),
        "latest_only dispatch should Fire after superseding prior, got {result:?}"
    );
    let supersede_calls = materializer.supersede_calls();
    assert_eq!(
        supersede_calls,
        vec![("sched-1".to_string(), TriggerKind::Schedule)],
        "exactly one supersede call for (sched-1, Schedule) expected"
    );
    let calls = materializer.calls();
    assert_eq!(
        calls.len(),
        1,
        "exactly one materialize call after supersede expected"
    );
    let (trigger_id, kind, _rendered) = &calls[0];
    assert_eq!(trigger_id.as_deref(), Some("sched-1"));
    assert_eq!(*kind, TriggerKind::Schedule);
}

#[tokio::test]
async fn dispatch_errors_and_skips_materialize_on_template_render_failure() {
    // Template references `event.missing_field`, but the intent's event_vars
    // has no such key. With strict-undefined semantics, rendering must fail,
    // and dispatch must return Errored (with a "template:" prefix), skip the
    // materializer entirely, and invoke `on_result` with the same Errored
    // value so the upstream source can write back `last_status = "error"`.
    let task = resolved_task("{{ event.missing_field }}");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let result_captured: Arc<Mutex<Option<FireResult>>> = Arc::new(Mutex::new(None));
    let capture = result_captured.clone();

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(move |r| {
            *capture.lock().unwrap() = Some(r);
        }),
    };

    let result = engine.dispatch(intent).await;

    match result.clone() {
        FireResult::Errored { error } => assert!(
            error.starts_with("template:"),
            "expected template-render error, got: {error}"
        ),
        other => panic!("expected Errored, got {other:?}"),
    }

    assert!(
        materializer.calls().is_empty(),
        "no materialize call should have been made on render failure"
    );
    assert!(
        materializer.supersede_calls().is_empty(),
        "no supersede call should have been made on render failure"
    );

    let captured = result_captured.lock().unwrap().clone();
    match captured {
        Some(FireResult::Errored { error }) => assert!(
            error.starts_with("template:"),
            "expected callback Errored with template prefix, got: {error}"
        ),
        other => panic!("expected callback Errored, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_latest_only_serializes_parallel_fires() {
    // Two LatestOnly dispatches for the same trigger fired in parallel. With
    // a materialize delay of ~60ms, the per-trigger lock must serialize them:
    // the second dispatch cannot enter its supersede+materialize critical
    // section until the first completes, so total wall-clock elapsed is at
    // least 2 * delay.
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let delay = Duration::from_millis(60);
    materializer.set_materialize_delay(delay);
    let engine = Arc::new(TriggerEngine::new(rx, materializer.clone()));

    let make_intent = || FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: task.clone(),
        concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: None,
        on_result: Box::new(|_| {}),
    };

    let start = Instant::now();
    let engine1 = engine.clone();
    let engine2 = engine.clone();
    let intent1 = make_intent();
    let intent2 = make_intent();
    let h1 = tokio::spawn(async move { engine1.dispatch(intent1).await });
    let h2 = tokio::spawn(async move { engine2.dispatch(intent2).await });
    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        matches!(r1, FireResult::Fired { .. }),
        "first parallel LatestOnly dispatch should Fire, got {r1:?}"
    );
    assert!(
        matches!(r2, FireResult::Fired { .. }),
        "second parallel LatestOnly dispatch should Fire, got {r2:?}"
    );
    assert_eq!(
        materializer.calls().len(),
        2,
        "both LatestOnly fires should materialize"
    );
    assert_eq!(
        materializer.supersede_calls().len(),
        2,
        "each LatestOnly fire runs a supersede call inside its critical section"
    );
    // If the two fires had run concurrently, total elapsed would be ~= delay.
    // With per-trigger serialization, elapsed must be >= 2 * delay. Allow a
    // small slack below 2x to tolerate sleep-granularity jitter on loaded CI.
    let min_expected = delay * 2 - Duration::from_millis(10);
    assert!(
        elapsed >= min_expected,
        "expected elapsed >= {min_expected:?} (2x delay, minus slack) proving \
         per-trigger serialization, got {elapsed:?}"
    );
}

#[tokio::test]
async fn schedule_source_next_fire_emits_intent_when_schedule_is_due() {
    // Seed a Schedule document with `next_run_at` 1s in the past, build a
    // snapshot that marks the same schedule active, and assert that
    // `ScheduleSource::next_fire` yields a matching `FireIntent` within 2
    // seconds. Also exercises the event_vars shape (fired_at, trigger_id,
    // trigger_kind) the downstream materializer will see.
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let past = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    create_schedule_with_next_run_at(node.as_ref(), "sched-1", "task-1", &past, "serial").await;

    let task = ResolvedTask {
        task_id: "task-1".to_string(),
        behavior_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        output_schema_ref: None,
    };
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-1".to_string(),
        resolved_schedule("sched-1", task),
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out")
        .expect("next_fire returned None");

    assert_eq!(intent.trigger_id.as_deref(), Some("sched-1"));
    assert_eq!(intent.trigger_kind, TriggerKind::Schedule);
    assert_eq!(intent.concurrency, ConcurrencyMode::Serial);
    assert_eq!(intent.task.task_id, "task-1");
    assert!(intent.doc_vars.is_none());
    assert!(intent.args_vars.is_none());

    let ev = &intent.event_vars;
    assert_eq!(ev["trigger_id"].as_str(), Some("sched-1"));
    assert_eq!(ev["trigger_kind"].as_str(), Some("schedule"));
    assert!(
        ev["fired_at"].is_string(),
        "fired_at should be a string, got {:?}",
        ev["fired_at"]
    );
}
