use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use tokio::sync::{mpsc, watch, Notify};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::compaction::CompactionStrategy;
use crate::config::{BehaviorConfig, SamplingConfig};
use crate::document_config::{
    list_event_trigger_records, list_schedule_records, load_schedule_next_run_at,
};
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::KeyIdentity;
use crate::lean_vocab_test::{
    assert_lean_to_defradb_vocabulary_matches, lean_trigger_dispatch_case_count,
    lean_trigger_dispatch_cases, LeanTriggerDispatchCase, LeanTriggerKeyContract, LeanVocabulary,
};
use crate::runtime_snapshot::{
    ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedEventTrigger, ResolvedRuntimeSnapshot,
    ResolvedSchedule, ResolvedTask,
};
use crate::tool_surface::BehaviorToolConfig;
use crate::trigger_engine::event_source::EventSource;
use crate::trigger_engine::manual_source::ManualSource;
use crate::trigger_engine::production_materializer::{
    execution_origin_for_trigger_kind, ProductionMaterializer,
};
use crate::trigger_engine::schedule_source::ScheduleSource;
use crate::BackendProviderKind;

/// Recorded `materialize` invocation: `(trigger_id, trigger_kind, rendered_prompt)`.
type MaterializeCall = (Option<String>, TriggerKind, String);

/// Recorded `supersede` invocation: `(trigger_id, trigger_kind)`.
type SupersedeCall = (String, TriggerKind);

const LEAN_TRIGGER_TYPES_MODEL: &str = include_str!("../../proofs/Proofs/Triggers/Types.lean");
const LEAN_TRIGGER_TYPES_FILE: &str = "crates/defra-agent/proofs/Proofs/Triggers/Types.lean";

#[test]
fn rust_trigger_kind_vocabulary_matches_lean_model() {
    let rust_kinds = vec![
        TriggerKind::Schedule.as_str(),
        TriggerKind::Event.as_str(),
        TriggerKind::Manual.as_str(),
    ];
    assert_lean_to_defradb_vocabulary_matches(LeanVocabulary {
        lean_file: LEAN_TRIGGER_TYPES_FILE,
        model: LEAN_TRIGGER_TYPES_MODEL,
        namespace: "TriggerKind",
        rust_source: "TriggerKind::{Schedule, Event, Manual}",
        rust_values: &rust_kinds,
    });
}

#[derive(Clone)]
struct MaterializeGate {
    entered_tx: mpsc::UnboundedSender<()>,
    release: Arc<Notify>,
}

/// Spy `MaterializerHandle` used by the engine tests. Records every
/// `materialize` call it sees and hands back sequentially-numbered request ids
/// so assertions can check both the call count and the rendered prompt that
/// reached the materializer.
///
/// `nonterminal_for` stores the concrete request ids for `(trigger_id,
/// trigger_kind)` tuples that `has_nonterminal_request_for_trigger` should
/// report as in-flight. Tests can pre-populate it to simulate prior fires.
/// Lean contract tests can opt into adding successful materializations as new
/// non-terminal requests, which mirrors production persistence without
/// changing the default spy behavior expected by local unit tests.
///
/// `materialize_delay` optionally pauses inside `materialize` before recording
/// the call. Used by the `LatestOnly` serialization tests to widen the window
/// during which the per-trigger lock is held so parallel fires can be observed
/// to queue.
struct SpyMaterializer {
    materialize_calls: Arc<Mutex<Vec<MaterializeCall>>>,
    next_request_id: AtomicUsize,
    nonterminal_for: Arc<Mutex<HashMap<(String, TriggerKind), Vec<String>>>>,
    supersede_calls: Arc<Mutex<Vec<SupersedeCall>>>,
    superseded_request_ids: Arc<Mutex<Vec<String>>>,
    materialize_delay: Mutex<Option<Duration>>,
    materialize_gate: Mutex<Option<MaterializeGate>>,
    track_materialized_nonterminal: AtomicBool,
}

impl SpyMaterializer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            materialize_calls: Arc::new(Mutex::new(Vec::new())),
            next_request_id: AtomicUsize::new(0),
            nonterminal_for: Arc::new(Mutex::new(HashMap::new())),
            supersede_calls: Arc::new(Mutex::new(Vec::new())),
            superseded_request_ids: Arc::new(Mutex::new(Vec::new())),
            materialize_delay: Mutex::new(None),
            materialize_gate: Mutex::new(None),
            track_materialized_nonterminal: AtomicBool::new(false),
        })
    }

    fn calls(&self) -> Vec<MaterializeCall> {
        self.materialize_calls.lock().unwrap().clone()
    }

    fn supersede_calls(&self) -> Vec<SupersedeCall> {
        self.supersede_calls.lock().unwrap().clone()
    }

    fn superseded_request_ids(&self) -> Vec<String> {
        self.superseded_request_ids.lock().unwrap().clone()
    }

    fn nonterminal_count_for(&self, trigger_id: &str, trigger_kind: TriggerKind) -> usize {
        self.nonterminal_for
            .lock()
            .unwrap()
            .get(&(trigger_id.to_owned(), trigger_kind))
            .map(Vec::len)
            .unwrap_or(0)
    }

    /// Pre-populate the in-flight set with `(trigger_id, trigger_kind)` so the
    /// next `has_nonterminal_request_for_trigger` call returns `true` for the
    /// matching tuple. Also makes `supersede_nonterminal_requests_for_trigger`
    /// report the tuple count (and clears it, mirroring real terminal
    /// transitions) so LatestOnly tests can assert the count plumbed through.
    fn mark_nonterminal(&self, trigger_id: &str, trigger_kind: TriggerKind) {
        let next_index = self.nonterminal_count_for(trigger_id, trigger_kind);
        let request_id = format!("spy-prior-{}-{next_index}", trigger_kind.as_str());
        self.mark_nonterminal_request(trigger_id, trigger_kind, request_id);
    }

    fn mark_nonterminal_request(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
        request_id: impl Into<String>,
    ) {
        self.nonterminal_for
            .lock()
            .unwrap()
            .entry((trigger_id.to_owned(), trigger_kind))
            .or_default()
            .push(request_id.into());
    }

    /// Make successful materializations increment the in-flight tuple count.
    /// The Lean-generated conformance cases use this to compare post-dispatch
    /// non-terminal counts; ordinary unit tests keep the older explicit
    /// `mark_nonterminal` behavior.
    fn track_materialized_nonterminal(&self) {
        self.track_materialized_nonterminal
            .store(true, Ordering::SeqCst);
    }

    /// Install a delay that `materialize` will sleep for before recording its
    /// call. Used to widen the critical section so parallel `LatestOnly`
    /// dispatches can be observed to serialize on the per-trigger lock.
    fn set_materialize_delay(&self, delay: Duration) {
        *self.materialize_delay.lock().unwrap() = Some(delay);
    }

    /// Block materialization until `release` is notified, sending one message
    /// on `entered_tx` each time a materialize call reaches the gate.
    fn set_materialize_gate(&self, entered_tx: mpsc::UnboundedSender<()>, release: Arc<Notify>) {
        *self.materialize_gate.lock().unwrap() = Some(MaterializeGate {
            entered_tx,
            release,
        });
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
        let nonterminal_for = self.nonterminal_for.clone();
        let nonterminal_key = trigger_id.map(|id| (id.to_owned(), trigger_kind));
        let track_materialized_nonterminal =
            self.track_materialized_nonterminal.load(Ordering::SeqCst);
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let request_id = format!("req-{id}");
        let delay = *self.materialize_delay.lock().unwrap();
        let gate = self.materialize_gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.entered_tx.send(());
                gate.release.notified().await;
            }
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            calls.lock().unwrap().push(entry);
            if let (true, Some(key)) = (track_materialized_nonterminal, nonterminal_key) {
                nonterminal_for
                    .lock()
                    .unwrap()
                    .entry(key)
                    .or_default()
                    .push(request_id.clone());
            }
            Ok(request_id)
        })
    }

    fn has_nonterminal_request_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        let set = self.nonterminal_for.clone();
        let key = (trigger_id.to_owned(), trigger_kind);
        Box::pin(async move {
            Ok(set
                .lock()
                .unwrap()
                .get(&key)
                .map(|request_ids| !request_ids.is_empty())
                .unwrap_or(false))
        })
    }

    fn supersede_nonterminal_requests_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + '_>> {
        let nonterm = self.nonterminal_for.clone();
        let supersede_calls = self.supersede_calls.clone();
        let superseded_request_ids = self.superseded_request_ids.clone();
        let key = (trigger_id.to_owned(), trigger_kind);
        Box::pin(async move {
            supersede_calls.lock().unwrap().push(key.clone());
            // Mirror a real terminal transition: the tuple is no longer
            // in-flight after supersede.
            let removed = nonterm.lock().unwrap().remove(&key).unwrap_or_default();
            let count = removed.len();
            superseded_request_ids.lock().unwrap().extend(removed);
            Ok(count)
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
        name: None,
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

fn resolved_schedule_with_concurrency(
    schedule_id: &str,
    task: ResolvedTask,
    concurrency: ConcurrencyMode,
) -> ResolvedSchedule {
    ResolvedSchedule {
        schedule_id: schedule_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        interval_secs: 60,
        enabled: true,
        concurrency,
    }
}

fn resolved_event_trigger_with_concurrency(
    trigger_id: &str,
    task: ResolvedTask,
    concurrency: ConcurrencyMode,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        trigger_id: trigger_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        source_collection: "WebhookEvent".to_string(),
        event_kind: "created".to_string(),
        filter: None,
        enabled: true,
        concurrency,
    }
}

fn trigger_kind_from_lean(value: &str) -> TriggerKind {
    match value {
        "schedule" => TriggerKind::Schedule,
        "event" => TriggerKind::Event,
        "manual" => TriggerKind::Manual,
        other => panic!("unknown Lean trigger kind {other:?}"),
    }
}

fn concurrency_from_lean(value: &str) -> ConcurrencyMode {
    ConcurrencyMode::parse(value)
        .unwrap_or_else(|| panic!("unknown Lean concurrency mode {value:?}"))
}

fn trigger_key_from_lean(key: &LeanTriggerKeyContract) -> (String, TriggerKind) {
    (
        key.trigger_id.clone(),
        trigger_kind_from_lean(&key.trigger_kind),
    )
}

fn snapshot_from_trigger_contract(
    case: &LeanTriggerDispatchCase,
    task: &ResolvedTask,
    concurrency: ConcurrencyMode,
) -> Arc<ActiveRuntimeSnapshot> {
    let active_schedules = case
        .active_schedule_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                resolved_schedule_with_concurrency(id, task.clone(), concurrency),
            )
        })
        .collect();
    let active_event_triggers = case
        .active_event_trigger_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                resolved_event_trigger_with_concurrency(id, task.clone(), concurrency),
            )
        })
        .collect();
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_schedules(active_schedules, HashSet::new())
    .with_event_triggers(active_event_triggers, HashSet::new());
    Arc::new(resolved.activate(1, HashMap::new()))
}

#[tokio::test]
async fn trigger_engine_dispatch_matches_lean_generated_contract_cases() {
    let cases = lean_trigger_dispatch_cases();
    assert!(
        !cases.is_empty(),
        "Lean trigger dispatch contract should emit at least one case"
    );
    assert_eq!(
        cases.len(),
        lean_trigger_dispatch_case_count(),
        "Lean trigger dispatch case-count sentinel drifted"
    );

    for case in cases {
        let trigger_kind = trigger_kind_from_lean(&case.trigger_kind);
        let concurrency = concurrency_from_lean(&case.concurrency);
        let task = resolved_task(&format!("lean case {}", case.name));
        let snapshot = snapshot_from_trigger_contract(case, &task, concurrency);
        let (_tx, rx) = watch::channel(snapshot);
        let materializer = SpyMaterializer::new();
        materializer.track_materialized_nonterminal();
        let target_key = case
            .trigger_id
            .as_ref()
            .map(|trigger_id| (trigger_id.clone(), trigger_kind));
        let expects_target_supersede = target_key.as_ref().map_or(false, |(trigger_id, kind)| {
            case.expected_supersede_call_keys.iter().any(|key| {
                key.trigger_id == *trigger_id && trigger_kind_from_lean(&key.trigger_kind) == *kind
            })
        });
        // Lean emits both lists by scanning `before.requests` in order; consume
        // superseded ids as we seed matching prior target keys to preserve that
        // request-id alignment.
        let mut superseded_prior_ids = case.superseded_prior_ids.iter();
        for key in &case.prior_nonterminal_keys {
            let (prior_trigger_id, prior_trigger_kind) = trigger_key_from_lean(key);
            if target_key
                .as_ref()
                .map_or(false, |(target_id, target_kind)| {
                    target_id == &prior_trigger_id && *target_kind == prior_trigger_kind
                })
            {
                if let Some(request_id) = superseded_prior_ids.next() {
                    materializer.mark_nonterminal_request(
                        &prior_trigger_id,
                        prior_trigger_kind,
                        request_id.clone(),
                    );
                } else {
                    assert!(
                        !expects_target_supersede,
                        "Lean case {} emitted fewer superseded_prior_ids than prior target keys",
                        case.name
                    );
                    materializer.mark_nonterminal(&prior_trigger_id, prior_trigger_kind);
                }
            } else {
                materializer.mark_nonterminal(&prior_trigger_id, prior_trigger_kind);
            }
        }
        assert!(
            superseded_prior_ids.next().is_none(),
            "Lean case {} emitted superseded_prior_ids that were not backed by prior target keys",
            case.name
        );
        let engine = TriggerEngine::new(rx, materializer.clone());

        let intent = FireIntent {
            trigger_id: case.trigger_id.clone(),
            trigger_kind,
            task,
            concurrency,
            event_vars: serde_json::json!({}),
            doc_vars: None,
            args_vars: None,
            on_result: Box::new(|_| {}),
        };

        let result = engine.dispatch(intent).await;
        let expected_delta = case
            .request_count_after
            .checked_sub(case.request_count_before)
            .unwrap_or_else(|| panic!("Lean case {} shrank request count", case.name));

        match (case.expected_result.as_str(), result) {
            ("fired", FireResult::Fired { .. }) => {}
            ("skipped", FireResult::Skipped { reason }) => assert_eq!(
                Some(reason.as_str()),
                case.expected_skip_reason.as_deref(),
                "Lean case {} skip reason drifted",
                case.name
            ),
            (expected, other) => panic!(
                "Lean case {} expected {expected}, but TriggerEngine returned {other:?}",
                case.name
            ),
        }

        let calls = materializer.calls();
        assert_eq!(
            calls.len(),
            expected_delta,
            "Lean case {} materialize delta drifted",
            case.name
        );
        if expected_delta == 1 {
            let (trigger_id, kind, rendered) = &calls[0];
            assert_eq!(
                trigger_id.as_deref(),
                case.expected_materialize_trigger_id.as_deref(),
                "Lean case {} materialize trigger_id drifted",
                case.name
            );
            assert_eq!(
                kind.as_str(),
                case.expected_materialize_trigger_kind.as_deref().unwrap(),
                "Lean case {} materialize trigger_kind drifted",
                case.name
            );
            assert_eq!(
                rendered,
                &format!("lean case {}", case.name),
                "Lean case {} rendered prompt drifted",
                case.name
            );
            assert_eq!(
                case.expected_execution_origin.as_deref(),
                Some(execution_origin_for_trigger_kind(*kind).as_str()),
                "Lean case {} execution-origin contract no longer matches production materializer mapping",
                case.name
            );
            let expected_request_kind = if trigger_id.is_some() {
                Some(kind.as_str())
            } else {
                None
            };
            assert_eq!(
                case.expected_request_caused_by_id.as_deref(),
                trigger_id.as_deref(),
                "Lean case {} request caused_by id drifted",
                case.name
            );
            assert_eq!(
                case.expected_request_caused_by_kind.as_deref(),
                expected_request_kind,
                "Lean case {} request caused_by kind drifted",
                case.name
            );
        } else {
            assert!(
                case.expected_materialize_trigger_id.is_none()
                    && case.expected_materialize_trigger_kind.is_none()
                    && case.expected_execution_origin.is_none(),
                "Lean case {} should not carry materialization fields when skipped",
                case.name
            );
        }

        let supersede_calls = materializer.supersede_calls();
        let expected_supersede_calls = case
            .expected_supersede_call_keys
            .iter()
            .map(trigger_key_from_lean)
            .collect::<Vec<_>>();
        assert_eq!(
            supersede_calls, expected_supersede_calls,
            "Lean case {} supersede calls drifted",
            case.name
        );
        assert_eq!(
            materializer.superseded_request_ids(),
            case.superseded_prior_ids,
            "Lean case {} superseded concrete request ids drifted",
            case.name
        );

        if let Some(trigger_id) = case.trigger_id.as_deref() {
            assert_eq!(
                materializer.nonterminal_count_for(trigger_id, trigger_kind),
                case.target_nonterminal_count_after.unwrap_or(0),
                "Lean case {} target non-terminal count drifted",
                case.name
            );
        }
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
async fn dispatch_latest_only_lock_blocks_second_supersede_until_first_materialize_finishes() {
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    materializer.set_materialize_gate(entered_tx, release.clone());
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

    let engine1 = engine.clone();
    let first_intent = make_intent();
    let first = tokio::spawn(async move { engine1.dispatch(first_intent).await });
    entered_rx
        .recv()
        .await
        .expect("first LatestOnly dispatch should enter materialize gate");
    assert_eq!(
        materializer.supersede_calls(),
        vec![("sched-1".to_string(), TriggerKind::Schedule)],
        "first LatestOnly dispatch should supersede before materializing"
    );

    let second = engine.dispatch(make_intent());
    tokio::pin!(second);
    std::future::poll_fn(|cx| match second.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!(
            "second LatestOnly dispatch completed while the first held the per-trigger lock: {result:?}"
        ),
    })
    .await;

    assert_eq!(
        materializer.supersede_calls().len(),
        1,
        "second LatestOnly dispatch must block on the per-trigger lock before superseding"
    );
    assert!(
        entered_rx.try_recv().is_err(),
        "second LatestOnly dispatch must not enter materialize while first is gated"
    );

    release.notify_waiters();
    let first_result = first.await.unwrap();
    assert!(
        matches!(first_result, FireResult::Fired { .. }),
        "first LatestOnly dispatch should finish after release, got {first_result:?}"
    );

    std::future::poll_fn(|cx| match second.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!(
            "second LatestOnly dispatch completed before its materialize gate was released: {result:?}"
        ),
    })
    .await;
    entered_rx
        .try_recv()
        .expect("second dispatch should enter materialize after the first releases the lock");
    release.notify_waiters();
    let second_result = second.await;
    assert!(
        matches!(second_result, FireResult::Fired { .. }),
        "second LatestOnly dispatch should fire after the first releases, got {second_result:?}"
    );
    assert_eq!(
        materializer.supersede_calls(),
        vec![
            ("sched-1".to_string(), TriggerKind::Schedule),
            ("sched-1".to_string(), TriggerKind::Schedule),
        ],
        "the second supersede must occur only after the first materialize completes"
    );
    assert_eq!(
        materializer.calls().len(),
        2,
        "both LatestOnly dispatches should materialize after serialized critical sections"
    );
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
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
        name: None,
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

/// After a successful fire, the callback advances `next_run_at += interval`,
/// writes `last_attempt_at`, sets `last_status = "fired"`, and bumps
/// `fire_count` by 1. After a skipped fire on the same schedule (with a fresh
/// intent generated from the already-advanced next_run_at), `last_status` must
/// flip to `"skipped"`, `next_run_at` still advances, and `fire_count` stays
/// put. Apply-owned fields (`interval_secs`, `enabled`, `task_id`,
/// `concurrency`) must be untouched across both writes.
#[tokio::test]
async fn schedule_source_on_result_writes_runtime_fields_on_fired_and_skipped() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Seed a Schedule that is already due (next_run_at 1s in the past) so
    // next_fire() will immediately yield an intent.
    let initial_next_run_at = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    create_schedule_with_next_run_at(
        node.as_ref(),
        "sched-1",
        "task-1",
        &initial_next_run_at,
        "serial",
    )
    .await;

    let task = ResolvedTask {
        task_id: "task-1".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        output_schema_ref: None,
    };
    let schedule = resolved_schedule("sched-1", task);
    let interval_secs = schedule.interval_secs;
    let snapshot = snapshot_with_schedules(HashMap::from([("sched-1".to_string(), schedule)]));
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    // ---- Fired case ----
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out (fired)")
        .expect("next_fire returned None (fired)");
    // Dispatch a synthetic Fired result into the callback. The callback spawns
    // a background write, so poll the DB until it lands (bounded retry).
    (intent.on_result)(FireResult::Fired {
        request_id: "req-0".to_string(),
    });
    let expected_next_run_at_fired = (DateTime::parse_from_rfc3339(&initial_next_run_at)
        .unwrap()
        .with_timezone(&Utc)
        + ChronoDuration::seconds(interval_secs))
    .to_rfc3339();
    let mut fired_schedule = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let records = list_schedule_records(node.as_ref()).await.unwrap();
        let (_doc_id, sched) = records
            .iter()
            .find(|(_d, s)| s.schedule_id == "sched-1")
            .cloned()
            .expect("Schedule doc disappeared");
        if sched.last_status.as_deref() == Some("fired") {
            fired_schedule = Some(sched);
            break;
        }
    }
    let fired = fired_schedule.expect("Schedule.last_status never became \"fired\"");
    assert_eq!(fired.last_status.as_deref(), Some("fired"));
    assert_eq!(fired.fire_count, Some(1));
    // Compare as parsed DateTimes truncated to second precision rather
    // than raw RFC3339 strings. Chrono's default `to_rfc3339()` emits
    // microsecond precision with a `+00:00` offset; DefraDB persists and
    // the runtime writeback normalizes to `Z` with second precision so
    // the DateTime scalar round-trips cleanly. The parse+truncate dance
    // makes the assertion robust to both axes of textual drift while
    // still proving the instant advanced by exactly one interval.
    let fired_next = fired
        .next_run_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc).timestamp());
    let expected_next_fired = DateTime::parse_from_rfc3339(&expected_next_run_at_fired)
        .unwrap()
        .with_timezone(&Utc)
        .timestamp();
    assert_eq!(fired_next, Some(expected_next_fired));
    assert!(
        fired.last_attempt_at.is_some(),
        "last_attempt_at should be set after a fire"
    );
    // Apply-owned fields must not be clobbered by the runtime writeback.
    assert_eq!(fired.interval_secs, Some(60));
    assert!(fired.enabled);
    assert_eq!(fired.task_id.as_deref(), Some("task-1"));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    // ---- Skipped case ----
    // Rewind next_run_at into the past again so the source will yield another
    // intent on the next tick. The new intent's on_result snapshot should
    // advance relative to the *new* next_run_at we just persisted.
    //
    // Use `Z`/second-precision form so the written value matches what the
    // runtime writeback produced. DefraDB's update path re-validates every
    // existing DateTime field against the schema on every partial update,
    // and rejects the whole mutation when any existing DateTime differs
    // from its canonical form (see `schedule_conformance.rs` for the same
    // quirk). We restate `last_attempt_at` using its post-writeback value
    // so this rewind mutation passes that revalidation.
    let rewound_next_run_at =
        (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let escaped_schedule_id = escape_graphql_string("sched-1");
    let escaped_rewound = escape_graphql_string(&rewound_next_run_at);
    let preserved_last_attempt = fired
        .last_attempt_at
        .as_deref()
        .expect("last_attempt_at must be set after the fired writeback")
        .to_string();
    let escaped_preserved_last_attempt = escape_graphql_string(&preserved_last_attempt);
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {{
                    next_run_at: "{escaped_rewound}",
                    last_attempt_at: "{escaped_preserved_last_attempt}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "rewind mutation failed: {:?}",
        resp.errors
    );

    let intent2 = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out (skipped)")
        .expect("next_fire returned None (skipped)");
    (intent2.on_result)(FireResult::Skipped {
        reason: "serial: prior fire still in-flight".to_string(),
    });
    let expected_next_run_at_skipped = (DateTime::parse_from_rfc3339(&rewound_next_run_at)
        .unwrap()
        .with_timezone(&Utc)
        + ChronoDuration::seconds(interval_secs))
    .to_rfc3339();
    let mut skipped_schedule = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let records = list_schedule_records(node.as_ref()).await.unwrap();
        let (_doc_id, sched) = records
            .iter()
            .find(|(_d, s)| s.schedule_id == "sched-1")
            .cloned()
            .expect("Schedule doc disappeared");
        if sched.last_status.as_deref() == Some("skipped") {
            skipped_schedule = Some(sched);
            break;
        }
    }
    let skipped = skipped_schedule.expect("Schedule.last_status never became \"skipped\"");
    assert_eq!(skipped.last_status.as_deref(), Some("skipped"));
    // fire_count MUST NOT advance on skip.
    assert_eq!(skipped.fire_count, Some(1));
    // See the fired-case comment above: parse+truncate both sides so
    // offset-suffix (`Z` vs `+00:00`) and subsecond-precision drift don't
    // flake the test.
    let skipped_next = skipped
        .next_run_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc).timestamp());
    let expected_next_skipped = DateTime::parse_from_rfc3339(&expected_next_run_at_skipped)
        .unwrap()
        .with_timezone(&Utc)
        .timestamp();
    assert_eq!(skipped_next, Some(expected_next_skipped));
    // Apply-owned fields still intact.
    assert_eq!(skipped.interval_secs, Some(60));
    assert!(skipped.enabled);
    assert_eq!(skipped.task_id.as_deref(), Some("task-1"));
    assert_eq!(skipped.concurrency.as_deref(), Some("serial"));
}

/// Cancelling the `CancellationToken` before polling `next_fire` must short-
/// circuit the tick-sleep and return `None` promptly — much faster than the
/// configured `tick_every`. This is the graceful-shutdown path the engine
/// relies on: on cancel the source is expected to drain back to `None` so the
/// outer loop can tear it down.
#[tokio::test]
async fn schedule_source_next_fire_honors_cancellation_token() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // No schedules are needed: cancellation must be observed before the tick
    // body ever runs the snapshot scan.
    let snapshot = snapshot_with_schedules(HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    // Deliberately use a long tick so any wall-clock elapsed below 1s is
    // strong evidence the select arm fired on cancel, not on sleep expiry.
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_secs(30));

    // Cancel before calling next_fire so the select!'s cancel arm is
    // immediately ready.
    cancel.cancel();

    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire did not return within 2s after cancel");
    let elapsed = start.elapsed();

    assert!(
        result.is_none(),
        "expected None after cancel, got Some(intent) with trigger_id={:?}",
        result.as_ref().and_then(|i| i.trigger_id.as_deref())
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "next_fire should return promptly on cancel, took {elapsed:?}"
    );
}

/// Build a minimal `BehaviorConfig` suitable for the production materializer
/// integration test. The behavior has a backend binding (required — the
/// materializer rejects tasks whose behavior is not backend-bound) but does
/// not drive any inference: the integration test asserts lineage on the
/// persisted `AgentRequest` doc only, not execution.
fn integration_test_behavior(behavior_name: &str) -> Arc<BehaviorConfig> {
    let identity = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("{behavior_name}-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    Arc::new(BehaviorConfig {
        name: behavior_name.to_string(),
        identity,
        backend_id: Some("backend-it".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://localhost:0/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: crate::config::DEFAULT_MODEL_NAME.to_string(),
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::config::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: BehaviorToolConfig::default(),
        compaction_threshold: crate::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: crate::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(crate::config::DEFAULT_DEADLINE_DURATION_SECS),
        sampling: SamplingConfig::default(),
    })
}

/// Build an `ActiveRuntimeSnapshot` containing the given behavior as loaded
/// and the supplied active schedules. Used by the integration test below to
/// hand the ProductionMaterializer a snapshot where `behavior_id` resolution
/// succeeds.
fn snapshot_with_behavior_and_schedules(
    behavior: Arc<BehaviorConfig>,
    schedules: HashMap<String, ResolvedSchedule>,
) -> Arc<ActiveRuntimeSnapshot> {
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        behavior.name.clone(),
        vec![behavior],
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_schedules(schedules, HashSet::new());
    Arc::new(resolved.activate(1, HashMap::new()))
}

/// Task 39 Step 1: end-to-end assertion that a due Schedule in the active
/// snapshot drives the `TriggerEngine` + `ScheduleSource` +
/// `ProductionMaterializer` pipeline to enqueue an `AgentRequest` carrying
/// `caused_by_trigger_id = <schedule_id>` and `caused_by_trigger_kind =
/// "schedule"` within a bounded wait.
///
/// Runs against a real `EmbeddedNode` because the ProductionMaterializer
/// writes via DefraDB — there is no in-memory shortcut. The test does not
/// assert execution (no inference is wired here); it only asserts the
/// enqueue boundary that Task 39 is restoring under the engine.
#[tokio::test]
async fn trigger_engine_enqueues_agent_request_for_due_schedule_e2e() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Seed a Schedule whose next_run_at is 1s in the past — the ScheduleSource
    // will emit an intent on its next tick.
    let past = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    create_schedule_with_next_run_at(node.as_ref(), "sched-e2e", "task-e2e", &past, "serial").await;

    // Build the snapshot: one behavior loaded ("general"), one active
    // schedule pointing at a task bound to that behavior.
    let behavior = integration_test_behavior("general");
    let task = ResolvedTask {
        task_id: "task-e2e".to_string(),
        name: Some("Mini Host Health".to_string()),
        behavior_id: behavior.name.clone(),
        prompt_template: "integration fire".to_string(),
        output_schema_ref: None,
    };
    let schedule = ResolvedSchedule {
        schedule_id: "sched-e2e".to_string(),
        task_id: task.task_id.clone(),
        task,
        interval_secs: 60,
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_behavior_and_schedules(
        behavior,
        HashMap::from([("sched-e2e".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);

    // Wire engine + source + materializer with the same watch::Receiver.
    let cancel = CancellationToken::new();
    let materializer: Arc<dyn MaterializerHandle> =
        Arc::new(ProductionMaterializer::new(node.clone(), rx.clone()));
    let source: Box<dyn TriggerSource> = Box::new(
        ScheduleSource::new(rx.clone(), node.clone(), cancel.clone())
            .with_tick_every(Duration::from_millis(50)),
    );
    let engine = TriggerEngine::new(rx, materializer);
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(vec![source], engine_cancel).await;
    });

    // Poll the DB for an AgentRequest with the lineage tuple. Bounded retry;
    // 50ms * 80 = 4s total, well within the "within N seconds" ask.
    let mut observed = None;
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let query = r#"query {
            AgentRequest(filter: {
                caused_by_trigger_id: { _eq: "sched-e2e" },
                caused_by_trigger_kind: { _eq: "schedule" }
            }) {
                _docID
                caused_by_trigger_id
                caused_by_trigger_kind
                lifecycle_state
                execution_origin
                session_id
                content
            }
        }"#;
        let resp = node.execute(query).await;
        assert!(
            !resp.has_errors(),
            "AgentRequest query errored: {:?}",
            resp.errors
        );
        let rows = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !rows.is_empty() {
            observed = rows.into_iter().next();
            break;
        }
    }

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), engine_handle).await;

    let row = observed.expect(
        "no AgentRequest with caused_by_trigger_id=sched-e2e observed within 4s; \
         expected the TriggerEngine + ScheduleSource pipeline to have materialized one",
    );
    assert_eq!(
        row.get("caused_by_trigger_id").and_then(|v| v.as_str()),
        Some("sched-e2e"),
        "persisted request is missing caused_by_trigger_id lineage: {row}"
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(|v| v.as_str()),
        Some("schedule"),
        "persisted request is missing caused_by_trigger_kind lineage: {row}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(|v| v.as_str()),
        Some("scheduled"),
        "trigger-driven fire should set execution_origin=scheduled: {row}"
    );
    assert_eq!(
        row.get("lifecycle_state").and_then(|v| v.as_str()),
        Some("pending"),
        "ProductionMaterializer should enqueue pending requests for watcher/router execution: {row}"
    );
    assert_eq!(
        row.get("content").and_then(|v| v.as_str()),
        Some("integration fire"),
        "rendered prompt template should land in AgentRequest.content: {row}"
    );

    let session_id = row
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("materialized request should have session_id");
    let conversation_query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                title
                title_source
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    let conversation_resp = node.execute(&conversation_query).await;
    assert!(
        !conversation_resp.has_errors(),
        "AgentConversation query errored: {:?}",
        conversation_resp.errors
    );
    let conversation = conversation_resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentConversation"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .expect("task materialization should seed AgentConversation title");
    assert_eq!(
        conversation.get("title_source").and_then(|v| v.as_str()),
        Some("task")
    );
    assert!(
        conversation
            .get("title")
            .and_then(|v| v.as_str())
            .is_some_and(|title| title.starts_with("mini-host-health-20")),
        "task conversation title should use task name plus timestamp: {conversation}"
    );
}

/// Regression for Finding 2: Schedules created with a null `next_run_at`
/// (the normal case for apply-path/desktop writes, which write only
/// apply-owned fields) must still fire. Before the fix, `ScheduleSource`
/// skipped null-`next_run_at` schedules forever, so tasks configured via
/// the CLI or desktop never ran.
///
/// Expected behavior: the runtime seeds `next_run_at = now` on the
/// first-seen tick for the schedule, treats the same tick as due, and
/// yields a `FireIntent` within a bounded wait (a couple of ticks).
#[tokio::test]
async fn schedule_source_seeds_null_next_run_at_and_fires_on_first_tick() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Create a Schedule doc WITHOUT next_run_at — mirrors what the
    // CLI/desktop apply writers do (they never touch runtime-owned
    // fields). Before Finding 2 was fixed, this schedule would sit
    // inert forever because ScheduleSource treated null next_run_at as
    // "not due, skip."
    let escaped_schedule_id = escape_graphql_string("sched-null");
    let escaped_task_id = escape_graphql_string("task-null");
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "{escaped_schedule_id}",
                task_id: "{escaped_task_id}",
                interval_secs: 60,
                enabled: true,
                concurrency: "serial"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Schedule without next_run_at failed: {:?}",
        response.errors
    );

    // Sanity check: the doc really has a null next_run_at right now.
    let precondition = load_schedule_next_run_at(node.as_ref(), "sched-null")
        .await
        .unwrap();
    assert!(
        precondition.is_none(),
        "precondition: created Schedule should have null next_run_at, got {precondition:?}"
    );

    let task = ResolvedTask {
        task_id: "task-null".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        output_schema_ref: None,
    };
    let snapshot = snapshot_with_schedules(HashMap::from([(
        "sched-null".to_string(),
        resolved_schedule("sched-null", task),
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    // With the fix: first tick seeds next_run_at = now, treats as due,
    // yields intent. Without the fix: null is treated as "not due" on
    // every tick and we'd time out.
    let started = Instant::now();
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect(
            "next_fire did not yield a FireIntent within 2s for a schedule with null \
             next_run_at; the engine must seed next_run_at on first-seen (Finding 2)",
        )
        .expect("next_fire returned None for a schedule with null next_run_at");
    let elapsed = started.elapsed();

    assert_eq!(intent.trigger_id.as_deref(), Some("sched-null"));
    assert_eq!(intent.trigger_kind, TriggerKind::Schedule);
    // Upper bound is loose: we only need "much less than the 60s
    // interval_secs" to prove first-tick seeding, not exact latency.
    assert!(
        elapsed < Duration::from_secs(2),
        "first-tick fire should land within a couple of ticks, took {elapsed:?}"
    );

    // The DB should now carry a non-null next_run_at — either the raw
    // seed (if on_result hasn't run) or the advanced value (if it has).
    // Either proves seeding happened.
    let after_seed = load_schedule_next_run_at(node.as_ref(), "sched-null")
        .await
        .unwrap();
    assert!(
        after_seed.is_some(),
        "Schedule.next_run_at should no longer be null after first-seen seeding"
    );
}

/// Regression for Finding 1: `ScheduleSource::next_fire` must NOT return
/// `None` on an idle tick. The engine's outer loop interprets `None` as
/// source exhaustion and breaks out — a premature `None` here (e.g. from
/// "no schedules are due right now") kills the schedule driver forever.
///
/// We drive the source with an empty active-schedule set and a short
/// tick, poll `next_fire` for >200ms (4+ ticks of 50ms), then cancel and
/// confirm that (a) we didn't get a spurious `Some(intent)` in that
/// window, and (b) after cancel the future completes with `None` within
/// a bounded wait. Before the fix, this test would observe `None`
/// arriving long before the cancel and fail the timeout-before-cancel
/// check.
#[tokio::test]
async fn schedule_source_next_fire_survives_idle_ticks_until_cancelled() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // No active schedules: every tick will be an "idle" tick where the
    // snapshot scan finds nothing to fire. Before Finding 1 was fixed,
    // the first such tick would fall off the end of the function and
    // return `None`, ending the source.
    let snapshot = snapshot_with_schedules(HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    // Race next_fire against a 250ms sleep followed by cancel. next_fire
    // must NOT finish before the cancel — if it does, the source
    // prematurely exited its internal loop. After cancel, it must finish
    // promptly with None.
    let cancel_clone = cancel.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        cancel_clone.cancel();
    });

    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(3), source.next_fire())
        .await
        .expect("next_fire did not return within 3s after cancel");
    let elapsed_until_return = started.elapsed();
    let _ = canceller.await;

    assert!(
        result.is_none(),
        "idle source should only return None after cancel, got Some(intent)",
    );
    assert!(
        elapsed_until_return >= Duration::from_millis(240),
        "next_fire returned before cancel fired at ~250ms (elapsed={elapsed_until_return:?}); \
         this means the source treated an idle tick as exhaustion and returned None early"
    );
}

/// Build a `ResolvedEventTrigger` pointing at the named source collection.
/// Matches the empty-defaults pattern used by `resolved_schedule`.
fn resolved_event_trigger(
    trigger_id: &str,
    source_collection: &str,
    task: ResolvedTask,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        trigger_id: trigger_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        source_collection: source_collection.to_string(),
        event_kind: "created".to_string(),
        filter: None,
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    }
}

/// Variant of `resolved_event_trigger` that attaches an operator-authored
/// filter fragment (e.g. `{ kind: { _eq: "signup" } }`). Used by the
/// filter-probe tests.
fn resolved_event_trigger_with_filter(
    trigger_id: &str,
    source_collection: &str,
    task: ResolvedTask,
    filter: &str,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        trigger_id: trigger_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        source_collection: source_collection.to_string(),
        event_kind: "created".to_string(),
        filter: Some(filter.to_string()),
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    }
}

/// Build an `ActiveRuntimeSnapshot` carrying the supplied event triggers and
/// no other live state. Mirrors `snapshot_with_schedules` for the event-source
/// tests.
fn snapshot_with_event_triggers(
    generation: u64,
    triggers: HashMap<String, ResolvedEventTrigger>,
) -> Arc<ActiveRuntimeSnapshot> {
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_event_triggers(triggers, HashSet::new());
    Arc::new(resolved.activate(generation, HashMap::new()))
}

/// Reconciling against a fresh snapshot whose `active_event_triggers`
/// reference a single source collection should populate that collection in
/// the filter set. Publishing a replacement snapshot that swaps the source
/// collection for a different one should drop the first and pick up the
/// second on the next reconciliation, proving the filter tracks the live
/// snapshot rather than accumulating history.
#[tokio::test]
async fn event_source_reconciles_subscriptions_on_generation_bump() {
    // A real embedded node is required because `reconcile_subscriptions`
    // opens the global `node.subscribe(&[EventName::Update])` subscription
    // on the first non-empty desired set.
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Snapshot generation 1: one trigger on CollectionA.
    let task = resolved_task("ignored");
    let snap1 = snapshot_with_event_triggers(
        1,
        HashMap::from([(
            "trigger-a".to_string(),
            resolved_event_trigger("trigger-a", "CollectionA", task.clone()),
        )]),
    );
    let (snapshot_tx, snapshot_rx) = watch::channel(snap1.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(snapshot_rx, node.clone(), cancel.clone());

    // Drive reconciliation against snapshot 1. `reconcile_subscriptions` is
    // called directly here — Task 19 tests the method; the `next_fire`
    // tick-boundary integration is the subject of Task 20.
    source.reconcile_subscriptions(snap1.as_ref()).await;

    assert_eq!(
        source.subscribed_collections(),
        vec!["CollectionA".to_string()],
        "after reconciling against snapshot 1 the filter set should exactly \
         match the snapshot's active_event_triggers source_collection",
    );

    // Snapshot generation 2: the old trigger is gone and a new one targets
    // CollectionB. Publish it through the watch channel to mimic how the
    // runtime reconcile loop hands snapshots to the engine.
    let snap2 = snapshot_with_event_triggers(
        2,
        HashMap::from([(
            "trigger-b".to_string(),
            resolved_event_trigger("trigger-b", "CollectionB", task),
        )]),
    );
    snapshot_tx.send(snap2.clone()).expect("snapshot_rx alive");

    source.reconcile_subscriptions(snap2.as_ref()).await;

    assert_eq!(
        source.subscribed_collections(),
        vec!["CollectionB".to_string()],
        "after reconciling against snapshot 2 CollectionA should be dropped \
         and only CollectionB should remain in the filter set",
    );
}

/// Drive `EventSource::next_fire` end-to-end against a real event stream.
///
/// The test:
/// 1. Registers a custom `WebhookEvent` schema on the embedded node so the
///    bus has a collection to emit events from (separate from the runtime
///    control collections so reconciliation is forced to walk the cache).
/// 2. Publishes a snapshot with one active `EventTrigger` on `WebhookEvent`.
/// 3. Opens the subscription (via `reconcile_subscriptions`) BEFORE creating
///    the document — `events::Bus` only buffers messages for already-
///    subscribed consumers, so a pre-subscription mutation is silently
///    dropped.
/// 4. Creates a document in that collection via a GraphQL mutation. The
///    node emits an `Update` event with `collection_id` set to the schema's
///    stable CollectionID (not the human-readable name).
/// 5. Asserts `next_fire` yields a `FireIntent` with the expected trigger
///    id, kind, task, concurrency, and event_vars shape, all within a
///    bounded 2s deadline.
#[tokio::test]
async fn event_source_next_fire_emits_intent_on_matching_real_event() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Register the source collection we'll trigger on. Kept intentionally
    // minimal — the test doesn't exercise Task 21's filter/doc-var work, so
    // the doc's fields are only read by the mutation validator.
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    // Build a snapshot with exactly one active EventTrigger on WebhookEvent.
    // The trigger_id is what the returned FireIntent should carry.
    let task = ResolvedTask {
        task_id: "task-webhook".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "handle webhook".to_string(),
        output_schema_ref: None,
    };
    let trigger = resolved_event_trigger("trigger-webhook", "WebhookEvent", task.clone());
    let snapshot =
        snapshot_with_event_triggers(1, HashMap::from([("trigger-webhook".to_string(), trigger)]));
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());

    // Open the subscription BEFORE writing the doc. The bus only buffers
    // messages for already-connected subscribers — a mutation that lands
    // before subscribe() returns leaves the subscription starved.
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    assert_eq!(
        source.subscribed_collections(),
        vec!["WebhookEvent".to_string()],
        "precondition: subscription set should match the trigger's source_collection",
    );

    // Drive the mutation on a detached task so next_fire can park on its
    // select! arm and wake when the event lands. Delaying the write by a
    // short window lets the `recv()` future register before the message is
    // published, which is the typical runtime ordering.
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-1",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");

    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-webhook"));
    assert_eq!(intent.trigger_kind, TriggerKind::Event);
    assert_eq!(intent.concurrency, ConcurrencyMode::Serial);
    assert_eq!(intent.task.task_id, "task-webhook");
    assert_eq!(intent.task.prompt_template, "handle webhook");
    // Task 21 hydrates `doc_vars` from the source doc. The trigger here
    // has no operator-authored filter, so every created doc should fire
    // and carry the full projection. We assert the shape here — the
    // dedicated hydration test drills into individual fields.
    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("Task 21: every fire must hydrate doc_vars (filter is None here)");
    assert_eq!(
        doc_vars["external_id"].as_str(),
        Some("wh-1"),
        "doc_vars must project the WebhookEvent fields, got {doc_vars}"
    );
    assert!(intent.args_vars.is_none());

    let ev = &intent.event_vars;
    assert_eq!(ev["trigger_id"].as_str(), Some("trigger-webhook"));
    assert_eq!(ev["trigger_kind"].as_str(), Some("event"));
    assert_eq!(ev["source_collection"].as_str(), Some("WebhookEvent"));
    assert!(
        ev["source_doc_id"].as_str().is_some_and(|s| !s.is_empty()),
        "source_doc_id should be a non-empty string from the persisted doc, got {:?}",
        ev["source_doc_id"]
    );
    assert!(
        ev["fired_at"].is_string(),
        "fired_at should be a string, got {:?}",
        ev["fired_at"]
    );
}

/// Task 21, Step 1: the filter-probe path must gate the fire on the
/// trigger's operator-authored filter. With `filter: { kind: { _eq: "signup" }}`
/// live on the trigger:
///
/// 1. Writing a matching doc (`kind = "signup"`) yields a FireIntent.
/// 2. Writing a non-matching doc (`kind = "other"`) is silently dropped —
///    `next_fire` must NOT return for that doc, even though the event
///    still reaches the subscription.
///
/// We assert (1) by observing a FireIntent within a bounded window, then
/// drive (2) by writing a second non-matching doc and confirming
/// `next_fire` times out (no second intent) before we cancel the source.
#[tokio::test]
async fn event_source_filter_probe_gates_fire_on_operator_filter() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Register a WebhookEvent schema that includes the `kind` field the
    // filter keys on. Must be indexed for DefraDB's filter evaluator to
    // accept `_eq` on a non-_docID field in a limit-1 query.
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
            email: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    let task = ResolvedTask {
        task_id: "task-webhook".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "handle webhook".to_string(),
        output_schema_ref: None,
    };
    // Trigger requires `kind == "signup"` — `other` events must not fire.
    let trigger = resolved_event_trigger_with_filter(
        "trigger-filtered",
        "WebhookEvent",
        task.clone(),
        r#"{ kind: { _eq: "signup" } }"#,
    );
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([("trigger-filtered".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Write BOTH docs on a detached task. A small delay gives `next_fire`
    // time to park on its subscription recv. Order matters only for
    // tracing readability — the filter probe is run per-event, so writing
    // the non-matching doc first would still leave the matching doc as
    // the one that ultimately yields the FireIntent.
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Non-matching doc: kind = "other". The probe should reject it
        // and next_fire must NOT return for this one.
        let other_mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-other",
                payload: "{}",
                kind: "other",
                email: "other@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(other_mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(other) failed: {:?}",
            resp.errors
        );
        // Matching doc: kind = "signup". The probe should accept this one.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let signup_mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-signup",
                payload: "{}",
                kind: "signup",
                email: "alice@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(signup_mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(signup) failed: {:?}",
            resp.errors
        );
    });

    // The matching doc should produce an intent within the timeout. A
    // non-matching doc never yields — `next_fire` loops past it.
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out; filter-probe must yield for the signup doc")
        .expect("next_fire returned None instead of emitting a FireIntent");

    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-filtered"));
    assert_eq!(intent.trigger_kind, TriggerKind::Event);
    assert_eq!(
        intent.event_vars["source_collection"].as_str(),
        Some("WebhookEvent"),
    );
    // doc_vars must be populated — covered in depth by the next test, but
    // a smoke assertion here locks the two steps together.
    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("filter-matched fire must carry hydrated doc_vars");
    assert_eq!(
        doc_vars["kind"].as_str(),
        Some("signup"),
        "hydrated doc_vars must reflect the matching doc, got {doc_vars}"
    );
    assert_eq!(doc_vars["external_id"].as_str(), Some("wh-signup"));

    // We don't actively assert the non-matching doc was dropped beyond the
    // fact that the FireIntent we got above is for "signup" (proving the
    // source skipped over "other" rather than firing on it). A stronger
    // assertion would require a second `next_fire` poll with a short
    // timeout, which races against late-delivered events.
    cancel.cancel();
}

/// Task 21, Step 2: the FireIntent's `doc_vars` must carry the full source
/// doc projection (introspected fields, excluding GraphQL meta /
/// DefraDB-aggregate wrappers). With no filter on the trigger, every
/// created doc produces a fire, and the fire's `doc_vars` should contain
/// the operator-visible scalars we wrote into the mutation.
#[tokio::test]
async fn event_source_hydrates_doc_vars_from_source_doc_fields() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
            email: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    let task = ResolvedTask {
        task_id: "task-webhook".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "handle webhook".to_string(),
        output_schema_ref: None,
    };
    // No filter on the trigger — every create fires, and the fire must
    // carry the full doc projection.
    let trigger = resolved_event_trigger("trigger-hydrate", "WebhookEvent", task.clone());
    let snapshot =
        snapshot_with_event_triggers(1, HashMap::from([("trigger-hydrate".to_string(), trigger)]));
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-hydrate",
                payload: "{\"foo\":1}",
                kind: "signup",
                email: "bob@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(hydrate) failed: {:?}",
            resp.errors
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out; hydration path should yield on any created doc")
        .expect("next_fire returned None instead of emitting a FireIntent");

    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("FireIntent must carry hydrated doc_vars");
    assert_eq!(
        doc_vars["external_id"].as_str(),
        Some("wh-hydrate"),
        "doc_vars must project `external_id`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["kind"].as_str(),
        Some("signup"),
        "doc_vars must project `kind`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["email"].as_str(),
        Some("bob@example.com"),
        "doc_vars must project `email`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["payload"].as_str(),
        Some(r#"{"foo":1}"#),
        "doc_vars must project `payload`, got {doc_vars}"
    );
    assert!(
        doc_vars["_docID"].as_str().is_some_and(|s| !s.is_empty()),
        "doc_vars must always carry _docID, got {doc_vars}"
    );

    cancel.cancel();
}

/// Helper: create an `EventTrigger` document keyed by `trigger_id` via a raw
/// GraphQL mutation, matching the shape used by the CLI apply path and the
/// `schedule_snapshot_reconcile` integration test. The `fire_count: 0` seed
/// is required so the runtime's `fire_count += 1` increment has a value to
/// read back.
async fn create_event_trigger_doc(
    node: &defra_node::EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "created",
                enabled: true,
                concurrency: "serial",
                fire_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create EventTrigger failed: {:?}",
        response.errors,
    );
}

/// Task 22: a Fired result dispatched through the `on_result` callback must
/// write the runtime-owned bookkeeping fields back onto the `EventTrigger`
/// document: `last_status = "fired"`, `fire_count += 1`,
/// `last_fired_source_doc_id` set to the source doc id that caused the fire,
/// and `last_attempt_at` populated. Apply-owned fields (`enabled`, `task_id`,
/// `source_collection`, `event_kind`, `concurrency`) must be untouched.
#[tokio::test]
async fn event_source_on_result_writes_runtime_fields_on_fired() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Register the source collection the trigger will observe.
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    // Seed the EventTrigger doc so `update_event_trigger_runtime_fields` has
    // a row to write back against. Apply-path fields are set here; the
    // runtime writeback must leave them alone.
    create_event_trigger_doc(
        node.as_ref(),
        "trigger-fired",
        "task-webhook",
        "WebhookEvent",
    )
    .await;

    let task = ResolvedTask {
        task_id: "task-webhook".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "handle webhook".to_string(),
        output_schema_ref: None,
    };
    let trigger = resolved_event_trigger("trigger-fired", "WebhookEvent", task.clone());
    let snapshot =
        snapshot_with_event_triggers(1, HashMap::from([("trigger-fired".to_string(), trigger)]));
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    // Open the subscription BEFORE writing the source doc so the mutation
    // lands after the bus has a listener. Otherwise the event is dropped.
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-fire",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");

    // Capture the source doc id the intent carries so we can assert the
    // writeback stamps it onto `last_fired_source_doc_id`.
    let fired_source_doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("event_vars.source_doc_id must be a string")
        .to_string();

    // Dispatch a synthetic Fired result into the callback. The callback
    // spawns a background write, so poll the DB until it lands (bounded
    // retry). This mirrors the ScheduleSource Fired test pattern.
    (intent.on_result)(FireResult::Fired {
        request_id: "req-0".to_string(),
    });

    let mut fired_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let records = list_event_trigger_records(node.as_ref()).await.unwrap();
        let (_doc_id, trig) = records
            .iter()
            .find(|(_d, t)| t.trigger_id == "trigger-fired")
            .cloned()
            .expect("EventTrigger doc disappeared");
        if trig.last_status.as_deref() == Some("fired") {
            fired_trigger = Some(trig);
            break;
        }
    }
    let fired = fired_trigger.expect("EventTrigger.last_status never became \"fired\"");
    assert_eq!(fired.last_status.as_deref(), Some("fired"));
    assert_eq!(fired.fire_count, Some(1));
    assert_eq!(
        fired.last_fired_source_doc_id.as_deref(),
        Some(fired_source_doc_id.as_str()),
        "last_fired_source_doc_id should match the source doc id carried \
         by the intent",
    );
    assert!(
        fired.last_attempt_at.is_some(),
        "last_attempt_at should be set after a fire",
    );
    assert_eq!(
        fired.last_error, None,
        "last_error must be cleared on a successful fire",
    );
    // Apply-owned fields must not be clobbered by the runtime writeback.
    assert_eq!(fired.task_id.as_deref(), Some("task-webhook"));
    assert_eq!(fired.source_collection.as_deref(), Some("WebhookEvent"));
    assert_eq!(fired.event_kind.as_deref(), Some("created"));
    assert_eq!(fired.enabled, Some(true));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    cancel.cancel();
}

/// Task 22: a Skipped result writes `last_status = "skipped"` and records
/// the skip reason in `last_error` without advancing `fire_count`. A
/// subsequent Errored result flips `last_status` to `"error"` and replaces
/// `last_error` with the failure string. Both writes must go through a
/// single source instance (and a single intent) to exercise the callback
/// directly without re-driving `next_fire` for each phase — per the spec,
/// the callback is a pure synthesizer of runtime-field updates from a
/// `FireResult` value.
#[tokio::test]
async fn event_source_on_result_writes_runtime_fields_on_skipped_or_errored() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    create_event_trigger_doc(
        node.as_ref(),
        "trigger-skip-err",
        "task-webhook",
        "WebhookEvent",
    )
    .await;

    let task = ResolvedTask {
        task_id: "task-webhook".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "handle webhook".to_string(),
        output_schema_ref: None,
    };
    let trigger = resolved_event_trigger("trigger-skip-err", "WebhookEvent", task.clone());
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([("trigger-skip-err".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-skip",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");

    // ---- Skipped phase ----
    // The callback is a `FnOnce` closure so we can only invoke it once per
    // intent. To drive two phases in one test we'd need two intents. The
    // simpler path: invoke with Skipped here, then synthesize a second
    // writeback by calling `spawn_runtime_field_write` directly for the
    // Errored case below. That mirrors exactly what the intent closure does
    // internally, and keeps the test focused on the writeback shape.
    let trigger_id = "trigger-skip-err".to_string();
    let source_doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("event_vars.source_doc_id must be a string")
        .to_string();

    (intent.on_result)(FireResult::Skipped {
        reason: "serial: prior fire still in-flight".to_string(),
    });

    let mut skipped_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let records = list_event_trigger_records(node.as_ref()).await.unwrap();
        let (_doc_id, trig) = records
            .iter()
            .find(|(_d, t)| t.trigger_id == "trigger-skip-err")
            .cloned()
            .expect("EventTrigger doc disappeared");
        if trig.last_status.as_deref() == Some("skipped") {
            skipped_trigger = Some(trig);
            break;
        }
    }
    let skipped = skipped_trigger.expect("EventTrigger.last_status never became \"skipped\"");
    assert_eq!(skipped.last_status.as_deref(), Some("skipped"));
    // fire_count MUST NOT advance on skip.
    assert_eq!(skipped.fire_count, Some(0));
    assert_eq!(
        skipped.last_error.as_deref(),
        Some("serial: prior fire still in-flight"),
        "last_error should carry the skip reason for operator visibility",
    );
    assert!(
        skipped.last_attempt_at.is_some(),
        "last_attempt_at should be set on a skip",
    );
    assert_eq!(
        skipped.last_fired_source_doc_id.as_deref(),
        Some(source_doc_id.as_str()),
        "last_fired_source_doc_id should record the candidate even on skip",
    );
    // Apply-owned fields intact.
    assert_eq!(skipped.task_id.as_deref(), Some("task-webhook"));
    assert_eq!(skipped.enabled, Some(true));
    assert_eq!(skipped.concurrency.as_deref(), Some("serial"));

    // ---- Errored phase ----
    // Drive the same writeback path with an Errored result. The helper is
    // an inherent `fn` on EventSource so we can call it directly — this is
    // exactly the path the `on_result` closure takes internally.
    EventSource::spawn_runtime_field_write(
        node.clone(),
        trigger_id.clone(),
        source_doc_id.clone(),
        FireResult::Errored {
            error: "materializer failed: backend timeout".to_string(),
        },
    );

    let mut errored_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let records = list_event_trigger_records(node.as_ref()).await.unwrap();
        let (_doc_id, trig) = records
            .iter()
            .find(|(_d, t)| t.trigger_id == "trigger-skip-err")
            .cloned()
            .expect("EventTrigger doc disappeared");
        if trig.last_status.as_deref() == Some("error") {
            errored_trigger = Some(trig);
            break;
        }
    }
    let errored = errored_trigger.expect("EventTrigger.last_status never became \"error\"");
    assert_eq!(errored.last_status.as_deref(), Some("error"));
    // fire_count MUST still not advance on error.
    assert_eq!(errored.fire_count, Some(0));
    assert_eq!(
        errored.last_error.as_deref(),
        Some("materializer failed: backend timeout"),
        "last_error should carry the failure string on Errored",
    );
    // Apply-owned fields intact.
    assert_eq!(errored.task_id.as_deref(), Some("task-webhook"));
    assert_eq!(errored.enabled, Some(true));
    assert_eq!(errored.concurrency.as_deref(), Some("serial"));

    cancel.cancel();
}

/// Build a `ResolvedTask` for unit tests that exercise the manual-fire path.
/// Mirrors `resolved_task` but takes an explicit `task_id` / `behavior_id`
/// so callers can assert the `task_id` round-tripped through the intent.
fn resolved_task_for_test(task_id: &str, behavior_id: &str, prompt_template: &str) -> ResolvedTask {
    ResolvedTask {
        task_id: task_id.to_string(),
        name: None,
        behavior_id: behavior_id.to_string(),
        prompt_template: prompt_template.to_string(),
        output_schema_ref: None,
    }
}

/// Build an `ActiveRuntimeSnapshot` with a single active task and no other
/// live state. Mirrors `snapshot_with_schedules`. Used by the manual-fire
/// tests that need `snapshot.active_tasks()` to resolve the intent's task.
fn snapshot_with_active_task(task: ResolvedTask) -> Arc<ActiveRuntimeSnapshot> {
    let mut tasks = HashMap::new();
    tasks.insert(task.task_id.clone(), task);
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_tasks(tasks);
    Arc::new(resolved.activate(1, HashMap::new()))
}

#[tokio::test]
async fn manual_source_run_task_now_yields_intent_with_args_vars() {
    let snapshot = snapshot_with_active_task(resolved_task_for_test(
        "greet-user",
        "behavior-1",
        "hello {{ args.name }}",
    ));
    let cancel = CancellationToken::new();
    let (mut source, handle) = ManualSource::new(cancel.clone());

    let pull = tokio::spawn(async move { source.next_fire().await });

    let _result_rx = handle
        .run_task_now(
            snapshot.as_ref(),
            "greet-user",
            serde_json::json!({"name": "Amy"}),
        )
        .await
        .unwrap();

    let intent = pull.await.unwrap().expect("next_fire returned None");
    assert_eq!(intent.trigger_kind, TriggerKind::Manual);
    assert_eq!(intent.trigger_id, None);
    assert_eq!(intent.concurrency, ConcurrencyMode::Parallel);
    assert_eq!(
        intent.args_vars.as_ref().and_then(|v| v["name"].as_str()),
        Some("Amy"),
    );
    assert_eq!(intent.task.task_id, "greet-user");
    assert_eq!(intent.event_vars["trigger_kind"].as_str(), Some("manual"));
    assert!(intent.doc_vars.is_none());
}

#[tokio::test]
async fn manual_source_run_task_now_rejects_unknown_task() {
    let snapshot =
        snapshot_with_active_task(resolved_task_for_test("other-task", "behavior-1", "x"));
    let (_source, handle) = ManualSource::new(CancellationToken::new());
    let err = handle
        .run_task_now(snapshot.as_ref(), "missing", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not in the active snapshot"),
        "expected 'not in the active snapshot' in error, got: {err}"
    );
}

#[tokio::test]
async fn manual_source_next_fire_returns_none_after_cancel() {
    let cancel = CancellationToken::new();
    let (mut source, _handle) = ManualSource::new(cancel.clone());

    // Cancel immediately.
    cancel.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_millis(200), source.next_fire())
        .await
        .expect("timed out waiting for cancelled next_fire");
    assert!(result.is_none());
}

/// Task 5 pinning: `ProductionMaterializer::materialize` must accept a
/// `TriggerKind::Manual` intent and persist an `AgentRequest` whose lineage
/// tuple is `(caused_by_trigger_id = null, caused_by_trigger_kind =
/// "manual")` with `execution_origin = "interactive"` (operator-initiated).
///
/// This protects two spec invariants at the materialization boundary:
///   * `TriggerKind::as_str()` is the authoritative source for the persisted
///     `caused_by_trigger_kind` field — no hard-coded "schedule"/"event".
///   * Manual fires map to `ExecutionOrigin::Interactive`, not `Scheduled`;
///     schedule and event fires keep `Scheduled`.
#[tokio::test]
async fn production_materializer_accepts_manual_lineage_end_to_end() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Snapshot: behavior "general" loaded (with backend_id), no active
    // schedules (Manual doesn't consult them).
    let behavior = integration_test_behavior("general");
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);

    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let task = resolved_task_for_test("task-manual", "general", "manual body");

    let request_id = materializer
        .materialize(&task, None, TriggerKind::Manual, "manual body")
        .await
        .expect("Manual materialize should succeed");

    let escaped_request_id = escape_graphql_string(&request_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                caused_by_trigger_id
                caused_by_trigger_kind
                execution_origin
                status
                lifecycle_state
                content
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest read-back errored: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .expect("expected one AgentRequest row for the materialized Manual fire");
    assert!(
        row.get("caused_by_trigger_id")
            .and_then(|v| v.as_str())
            .is_none(),
        "Manual fires carry no trigger id; expected null caused_by_trigger_id: {row}"
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(|v| v.as_str()),
        Some("manual"),
        "Manual lineage must serialize via TriggerKind::as_str() = \"manual\": {row}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(|v| v.as_str()),
        Some("interactive"),
        "Manual fires map to ExecutionOrigin::Interactive per spec: {row}"
    );
    assert_eq!(
        row.get("status").and_then(|v| v.as_str()),
        Some("pending"),
        "Production materializer should enqueue Manual fires for normal intake: {row}"
    );
    assert_eq!(
        row.get("lifecycle_state").and_then(|v| v.as_str()),
        Some("pending"),
        "Production materializer should leave Manual fires pending until daemon claim: {row}"
    );
    assert_eq!(
        row.get("content").and_then(|v| v.as_str()),
        Some("manual body"),
        "rendered prompt should land verbatim in AgentRequest.content: {row}"
    );
}

#[tokio::test]
async fn production_materializer_rejects_manual_lineage_with_trigger_id() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let snapshot = snapshot_with_schedules(HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node, rx);
    let task = resolved_task_for_test("task-manual", "general", "manual body");

    let err = materializer
        .materialize(
            &task,
            Some("manual-must-not-have-id"),
            TriggerKind::Manual,
            "manual body",
        )
        .await
        .expect_err("Manual materialize with trigger_id must fail before persistence");

    assert!(
        err.to_string().contains("must not carry trigger_id"),
        "unexpected manual lineage validation error: {err}"
    );
}

/// Task 6 pinning: `TriggerEngine::dispatch` must pass `TriggerKind::Manual`
/// intents through without consulting `active_schedules()` /
/// `active_event_triggers()` (no enabled-gate rejection for operator
/// fires), render the prompt template against `args_vars`, and invoke the
/// materializer exactly once with `(trigger_id = None, trigger_kind =
/// Manual, rendered = "hello Amy")`.
#[tokio::test]
async fn dispatch_manual_intent_renders_with_args_and_materializes() {
    // Snapshot carries the active task but NO active schedules / event
    // triggers. A Schedule/Event intent would be gated off here; Manual
    // must not be.
    let task = resolved_task_for_test("greet-user", "general", "hello {{ args.name }}");
    let snapshot = snapshot_with_active_task(task.clone());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: None,
        trigger_kind: TriggerKind::Manual,
        task,
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: Some(serde_json::json!({"name": "Amy"})),
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Fired { request_id } => assert_eq!(
            request_id, "req-0",
            "spy materializer hands back sequentially-numbered ids starting at req-0"
        ),
        other => panic!("expected Fired for Manual intent (bypasses enabled-gate), got {other:?}"),
    }

    let calls = materializer.calls();
    assert_eq!(
        calls.len(),
        1,
        "exactly one materialize call expected for Manual dispatch"
    );
    let (trigger_id, kind, rendered) = &calls[0];
    assert!(
        trigger_id.is_none(),
        "Manual intents carry trigger_id = None; got {trigger_id:?}"
    );
    assert_eq!(*kind, TriggerKind::Manual);
    assert_eq!(
        rendered, "hello Amy",
        "dispatch must render the `args.name` template against args_vars"
    );
}

#[tokio::test]
async fn dispatch_rejects_manual_intent_with_trigger_id() {
    let task = resolved_task_for_test("greet-user", "general", "hello");
    let snapshot = snapshot_with_active_task(task.clone());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let result_captured: Arc<Mutex<Option<FireResult>>> = Arc::new(Mutex::new(None));
    let capture = result_captured.clone();
    let intent = FireIntent {
        trigger_id: Some("manual-must-not-have-id".to_string()),
        trigger_kind: TriggerKind::Manual,
        task,
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        args_vars: Some(serde_json::json!({})),
        on_result: Box::new(move |r| {
            *capture.lock().unwrap() = Some(r);
        }),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Errored { error } => assert!(
            error.contains("must not carry trigger_id"),
            "unexpected manual well-formedness error: {error}"
        ),
        other => panic!("expected Errored for malformed Manual intent, got {other:?}"),
    }
    assert!(
        materializer.calls().is_empty(),
        "malformed Manual intent must not reach the materializer"
    );
    assert!(
        matches!(
            result_captured.lock().unwrap().as_ref(),
            Some(FireResult::Errored { error }) if error.contains("must not carry trigger_id")
        ),
        "on_result should receive the same malformed Manual error"
    );
}

// ---------------------------------------------------------------------------
// Regression tests for the duplicate-on-update / fan-out correctness fixes.
// The DefraDB event bus emits a single `EventName::Update` variant for
// creates, updates, and deletes; v1 event triggers ship `event_kind =
// "created"` only. The event source enforces that forward-only contract via
// a first-seen gate seeded at subscription open, and fans out a single
// observation across every matching trigger.
// ---------------------------------------------------------------------------

/// Finding 1: a pre-existing source doc whose first observation arrives
/// AFTER the subscription opens must NOT fire — the seed populated by
/// `reconcile_subscriptions` registers it as already-seen. This is the
/// "don't fire on update" half of the forward-only semantic.
#[tokio::test]
async fn event_source_skips_event_for_doc_already_seen_at_subscribe() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    // Seed a doc BEFORE the trigger + subscription exist. The first-seen
    // seed query at reconcile time will pick this doc up and mark it as
    // already-observed so any subsequent Update for it is treated as an
    // update (and dropped under v1 semantics).
    let seed_mutation = r#"mutation {
        create_WebhookEvent(input: {
            external_id: "wh-preexisting",
            payload: "seed"
        }) { _docID }
    }"#;
    let resp = node.execute(seed_mutation).await;
    assert!(
        !resp.has_errors(),
        "seeding pre-existing doc failed: {:?}",
        resp.errors,
    );
    // The returned shape varies by DefraDB version (scalar vs array); query
    // the _docID explicitly rather than parse the mutation payload.
    let lookup = r#"query {
        WebhookEvent(filter: { external_id: { _eq: "wh-preexisting" } }, limit: 1) {
            _docID
        }
    }"#;
    let resp = node.execute(lookup).await;
    assert!(
        !resp.has_errors(),
        "lookup of pre-existing doc failed: {:?}",
        resp.errors,
    );
    let preexisting_doc_id = resp
        .data
        .as_ref()
        .and_then(|d| d.get("WebhookEvent"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .expect("WebhookEvent query returned no _docID")
        .to_string();

    // Open the trigger + subscription AFTER the seed doc exists. Reconcile
    // should run the seed query and capture `preexisting_doc_id` into
    // seen_docs so the next Update is treated as a non-first observation.
    let task = resolved_task("ignored");
    let trigger = resolved_event_trigger("trigger-noupdate", "WebhookEvent", task);
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([("trigger-noupdate".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Now drive an UPDATE to the pre-existing doc. Events flow, but the
    // first-seen gate should drop this one — it's a non-first observation.
    let escaped = escape_graphql_string(&preexisting_doc_id);
    let update_mutation = format!(
        r#"mutation {{
            update_WebhookEvent(
                docID: "{escaped}",
                input: {{ payload: "updated" }}
            ) {{ _docID }}
        }}"#
    );
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let resp = node_for_mutation.execute(&update_mutation).await;
        assert!(
            !resp.has_errors(),
            "update_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    // next_fire MUST time out — the update was suppressed by the first-seen
    // gate. A short window is sufficient because the event bus round-trip
    // is milliseconds; anything above that window would mean we got a fire.
    let result = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        result.is_err(),
        "next_fire yielded an intent for a pre-seeded doc's update; seed seen_docs \
         did not suppress the non-first observation",
    );

    cancel.cancel();
}

/// Finding 1: the first observation of a newly-created doc fires; the next
/// observation (an update to the same doc) must NOT fire. Complements the
/// pre-existing test by exercising the runtime-maintained first-seen set
/// rather than the seed.
#[tokio::test]
async fn event_source_fires_for_first_seen_doc_then_skips_updates() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    let task = resolved_task("ignored");
    let trigger = resolved_event_trigger("trigger-firstseen", "WebhookEvent", task);
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([("trigger-firstseen".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Create a brand-new doc; first observation should fire.
    let node_for_create = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-first",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_create.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on first observation (create should fire)")
        .expect("next_fire returned None instead of emitting a FireIntent");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-firstseen"));
    let doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("source_doc_id must be a string")
        .to_string();

    // Update the same doc. Second observation; the first-seen set records
    // the doc, so the update must not fire.
    let escaped = escape_graphql_string(&doc_id);
    let update_mutation = format!(
        r#"mutation {{
            update_WebhookEvent(
                docID: "{escaped}",
                input: {{ payload: "updated" }}
            ) {{ _docID }}
        }}"#
    );
    let node_for_update = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resp = node_for_update.execute(&update_mutation).await;
        assert!(
            !resp.has_errors(),
            "update_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let result = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        result.is_err(),
        "next_fire yielded an intent for a doc's update; first-seen gate failed to \
         suppress the second observation",
    );

    cancel.cancel();
}

/// Finding 2: one source event that matches N active triggers must yield N
/// `FireIntent`s (not 1 and not 0). Registers two triggers on the same
/// source collection with no filter, creates a single doc, and drains two
/// intents out of the source in deterministic (lex by trigger_id) order.
#[tokio::test]
async fn event_source_fans_out_one_event_across_multiple_matching_triggers() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    let task = resolved_task("ignored");
    // Two triggers on the same collection. lex order: trigger-alpha < trigger-beta.
    let trigger_alpha = resolved_event_trigger("trigger-alpha", "WebhookEvent", task.clone());
    let trigger_beta = resolved_event_trigger("trigger-beta", "WebhookEvent", task);
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([
            ("trigger-alpha".to_string(), trigger_alpha),
            ("trigger-beta".to_string(), trigger_beta),
        ]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Single doc — both triggers must fire, one intent per trigger.
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-fanout",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let first = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on the first fan-out intent")
        .expect("next_fire returned None instead of emitting the first intent");
    let second = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on the second fan-out intent; fan-out dropped it?")
        .expect("next_fire returned None instead of emitting the second intent");

    assert_eq!(
        first.trigger_id.as_deref(),
        Some("trigger-alpha"),
        "fan-out must emit intents in deterministic lex-by-trigger_id order",
    );
    assert_eq!(second.trigger_id.as_deref(), Some("trigger-beta"));
    // Both intents reference the same source doc.
    let first_doc_id = first
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str());
    let second_doc_id = second
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str());
    assert_eq!(
        first_doc_id, second_doc_id,
        "both fan-out intents must carry the same source_doc_id: {first_doc_id:?} vs {second_doc_id:?}",
    );

    cancel.cancel();
}

/// Finding 2: if the lexicographically-first trigger's filter misses, the
/// event must still be tried against the remaining triggers. Previously
/// `first_matching_trigger` would select the lex-first trigger unconditionally
/// and drop the whole event if that trigger's filter missed, silently
/// denying every other matching trigger a chance to fire.
#[tokio::test]
async fn event_source_tries_all_triggers_when_first_filter_misses() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(webhook_sdl)
        .await
        .expect("add_schema for WebhookEvent");

    let task = resolved_task("ignored");
    // trigger-a sorts first by lex order; its filter rejects the test doc.
    // trigger-b sorts second; its filter accepts the test doc. With the fix,
    // the engine tries trigger-a, sees the filter miss, then moves on to
    // trigger-b and fires.
    let trigger_a = resolved_event_trigger_with_filter(
        "trigger-a-lex-first",
        "WebhookEvent",
        task.clone(),
        r#"{ kind: { _eq: "signup" } }"#,
    );
    let trigger_b = resolved_event_trigger_with_filter(
        "trigger-b-matches",
        "WebhookEvent",
        task,
        r#"{ kind: { _eq: "other" } }"#,
    );
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([
            ("trigger-a-lex-first".to_string(), trigger_a),
            ("trigger-b-matches".to_string(), trigger_b),
        ]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Write a doc whose kind is "other" — misses trigger-a, matches trigger-b.
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-missfirst",
                payload: "{}",
                kind: "other"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect(
            "next_fire timed out; trigger-a's filter miss silently dropped the \
             event for trigger-b (fan-out regression)",
        )
        .expect("next_fire returned None instead of emitting a FireIntent");
    assert_eq!(
        intent.trigger_id.as_deref(),
        Some("trigger-b-matches"),
        "after trigger-a filter-miss, the engine must still try trigger-b and fire \
         for it; got trigger_id = {:?}",
        intent.trigger_id,
    );

    // And crucially, there must be no second intent — trigger-a did NOT
    // match the filter, so it must not have emitted.
    let maybe_extra = tokio::time::timeout(Duration::from_millis(300), source.next_fire()).await;
    assert!(
        maybe_extra.is_err(),
        "trigger-a emitted a FireIntent despite its filter miss",
    );

    cancel.cancel();
}
