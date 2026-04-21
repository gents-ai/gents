use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::*;
use crate::runtime_snapshot::{
    ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedRuntimeSnapshot, ResolvedSchedule,
    ResolvedTask,
};

/// Recorded `materialize` invocation: `(trigger_id, trigger_kind, rendered_prompt)`.
type MaterializeCall = (Option<String>, TriggerKind, String);

/// Spy `MaterializerHandle` used by the engine tests. Records every
/// `materialize` call it sees and hands back sequentially-numbered request ids
/// so assertions can check both the call count and the rendered prompt that
/// reached the materializer.
struct SpyMaterializer {
    materialize_calls: Arc<Mutex<Vec<MaterializeCall>>>,
    next_request_id: AtomicUsize,
}

impl SpyMaterializer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            materialize_calls: Arc::new(Mutex::new(Vec::new())),
            next_request_id: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> Vec<MaterializeCall> {
        self.materialize_calls.lock().unwrap().clone()
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
        Box::pin(async move {
            calls.lock().unwrap().push(entry);
            Ok(format!("req-{id}"))
        })
    }

    fn has_nonterminal_request_for_trigger(
        &self,
        _trigger_id: &str,
        _trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }

    fn supersede_nonterminal_requests_for_trigger(
        &self,
        _trigger_id: &str,
        _trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + '_>> {
        Box::pin(async { Ok(0) })
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
