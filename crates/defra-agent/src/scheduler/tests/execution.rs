use super::super::*;
use super::support::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::admission::BackendAdmissionConfig;
use crate::compaction::CompactionStrategy;
use crate::config::{
    BehaviorConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_STREAM_BATCH_MS,
};
use crate::ensure_runtime_schemas;
use crate::identity::SimpleIdentity;
use crate::tool_surface::{BehaviorToolConfig, ToolRuntimeContext};
use crate::BackendProviderKind;

#[tokio::test]
async fn scheduled_execution_succeeds_without_external_ops_service() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend(node.as_ref(), "backend-1", mock_endpoint.endpoint()).await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-1".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    };

    let tool_surface = behavior.tools.resolve(node.as_ref()).await.unwrap();
    let tool_runtime = ToolRuntimeContext::oneshot(node.clone());
    let task = ScheduledTask {
        doc_id: "task-doc".to_string(),
        task_id: "task-1".to_string(),
        name: "nightly-check".to_string(),
        behavior_id: behavior.name.clone(),
        prompt: "Say scheduled-ok".to_string(),
        interval_secs: 60,
        enabled: true,
        next_run_at: None,
        run_count: 0,
    };

    super::super::execution::execute_task_standalone(
        &task,
        &behavior,
        &tool_surface,
        &tool_runtime,
        &node,
        test_admission_registry(node.clone(), "backend-1", 1),
        CancellationToken::new(),
    )
    .await
    .expect("scheduled execution should not depend on external ops service");
}

#[tokio::test]
async fn scheduled_execution_updates_live_task_runtime_fields() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend(node.as_ref(), "backend-runtime", mock_endpoint.endpoint()).await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-runtime".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    };

    let tool_surface = behavior.tools.resolve(node.as_ref()).await.unwrap();
    let tool_runtime = ToolRuntimeContext::oneshot(node.clone());
    insert_due_task(
        node.as_ref(),
        "task-runtime-state",
        &behavior.name,
        "Say scheduled-ok",
    )
    .await;
    let task = ScheduledTask::from_value(
        &query_task_row(node.as_ref(), "task-runtime-state", false)
            .await
            .expect("task should exist"),
    )
    .expect("task row should parse");

    super::super::execution::execute_task_standalone(
        &task,
        &behavior,
        &tool_surface,
        &tool_runtime,
        &node,
        test_admission_registry(node.clone(), "backend-runtime", 1),
        CancellationToken::new(),
    )
    .await
    .expect("scheduled execution should succeed");

    let updated = query_task_row(node.as_ref(), "task-runtime-state", false)
        .await
        .expect("updated task should exist");
    assert_eq!(
        updated
            .get("last_status")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
    assert_eq!(
        updated
            .get("last_error")
            .and_then(serde_json::Value::as_str),
        Some("")
    );
    assert_eq!(
        updated.get("run_count").and_then(serde_json::Value::as_i64),
        Some(1)
    );
    let last_run_at = updated
        .get("last_run_at")
        .and_then(serde_json::Value::as_str)
        .expect("last_run_at should be set");
    let next_run_at = updated
        .get("next_run_at")
        .and_then(serde_json::Value::as_str)
        .expect("next_run_at should be set");
    let last_run = chrono::DateTime::parse_from_rfc3339(last_run_at).unwrap();
    let next_run = chrono::DateTime::parse_from_rfc3339(next_run_at).unwrap();
    assert!(next_run > last_run);
}

#[tokio::test]
async fn stale_runtime_bookkeeping_is_skipped_after_task_delete() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let doc_id = insert_due_task(
        node.as_ref(),
        "task-stale-delete",
        "did:defra-agent:scheduled-test:default",
        "Say scheduled-ok",
    )
    .await;
    let task = ScheduledTask::from_value(
        &query_task_row(node.as_ref(), "task-stale-delete", false)
            .await
            .expect("task should exist"),
    )
    .expect("task row should parse");

    delete_task(node.as_ref(), &doc_id).await;
    super::super::update_task_runtime_state(&node, &task, "success", None)
        .await
        .expect("deleted task bookkeeping should be skipped cleanly");

    assert!(
        query_task_row(node.as_ref(), "task-stale-delete", false)
            .await
            .is_none(),
        "deleted task should not reappear in live queries"
    );
    let deleted = query_task_row(node.as_ref(), "task-stale-delete", true)
        .await
        .expect("showDeleted should return tombstone");
    assert_eq!(
        deleted.get("_deleted").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(deleted.get("last_status").is_none() || deleted.get("last_status").unwrap().is_null());
    assert_eq!(
        deleted.get("run_count").and_then(serde_json::Value::as_i64),
        Some(0)
    );
}

#[tokio::test]
async fn scheduler_tick_shutdown_is_prompt_while_task_waits_for_backend_capacity() {
    use crate::admission::CallKind;
    use tokio::sync::watch;

    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend_with_capacity(
        node.as_ref(),
        "backend-blocked",
        mock_endpoint.endpoint(),
        1,
    )
    .await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = Arc::new(BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-blocked".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    });
    let tool_surface = Arc::new(behavior.tools.resolve(node.as_ref()).await.unwrap());
    insert_due_task(
        node.as_ref(),
        "task-blocked",
        &behavior.name,
        "Say scheduled-ok",
    )
    .await;

    let active_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        default_behavior_id: behavior.name.clone(),
        behaviors: std::collections::HashMap::from([(behavior.name.clone(), behavior.clone())]),
        tool_surfaces: std::collections::HashMap::from([(behavior.name.clone(), tool_surface)]),
        backend_admission_configs: std::collections::HashMap::from([(
            "backend-blocked".to_string(),
            BackendAdmissionConfig {
                backend_id: "backend-blocked".to_string(),
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true,
                probe_status: "healthy".to_string(),
                config_fingerprint: "backend-blocked:1:100".to_string(),
            },
        )]),
        unavailable_behaviors: std::collections::HashMap::new(),
        dispatchers: std::collections::HashMap::new(),
    });
    let registry = test_admission_registry(node.clone(), "backend-blocked", 1);
    let _held_permit = registry
        .acquire_for_test(
            "req-held-scheduler-capacity",
            "backend-blocked",
            &behavior.name,
            behavior.did(),
            CallKind::Scheduled,
        )
        .await
        .expect("test permit should acquire backend capacity");
    let (_tx, rx) = watch::channel(active_snapshot);
    let mut scheduler = Scheduler::new(
        node.clone(),
        rx,
        ToolRuntimeContext::oneshot(node.clone()),
        registry,
    );
    let cancel = CancellationToken::new();
    let cancel_for_tick = cancel.clone();

    let tick = tokio::spawn(async move { scheduler.tick(cancel_for_tick).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    tokio::time::timeout(Duration::from_secs(2), tick)
        .await
        .expect("scheduler tick should not wait for backend deadline")
        .expect("tick task should join")
        .expect("scheduler tick should return ok");
}
