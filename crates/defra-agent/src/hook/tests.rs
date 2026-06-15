use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use crate::llm::{HookAction, ToolCallHookAction};
use serde_json::json;

use super::*;
use crate::ensure_schemas;
use crate::lean_vocab_test::{
    lean_persistence_failure_policy_cases, lean_storage_observation_runtime_cases,
};
use crate::test_support::first_content;

fn user_text_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

fn session_state_for_test() -> SessionState {
    SessionState {
        session_id: Some("session-1".to_string()),
        current_request_id: None,
        request_deadline_at: None,
        agent_name: "agent".to_string(),
        sequence: 0,
        transcript_turn: TranscriptTurnState::Idle,
        persisted_tool_result_keys: std::collections::HashSet::new(),
        persisted_tool_result_message_sequences: std::collections::HashMap::new(),
        tool_result_identities: std::collections::HashMap::new(),
        initialized: true,
    }
}

fn hook_counters_for_test() -> HookCounters {
    HookCounters {
        failures: AtomicU64::new(0),
        successes: AtomicU64::new(0),
    }
}

fn failure_policy_from_contract(policy: &str) -> FailurePolicy {
    match policy {
        "failOpen" => FailurePolicy::FailOpen,
        "failClosed" => FailurePolicy::FailClosed,
        other => panic!("unknown Lean persistence failure policy {other:?}"),
    }
}

#[test]
fn transcript_turn_state_allocates_new_assistant_after_saved_turn() {
    let mut state = session_state_for_test();

    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.persist_assistant_turn(), 1);
    assert!(state
        .mark_stream_tool_result_seen("call-1", "call-1", None)
        .unwrap());
    assert!(!state
        .mark_stream_tool_result_seen("call-1", "call-1", None)
        .unwrap());

    state.reset_after_user_message();
    assert_eq!(state.begin_or_continue_assistant_turn(), 2);
    assert_eq!(state.persist_assistant_turn(), 2);
}

#[test]
fn transcript_turn_state_rejects_stream_result_before_assistant_is_saved() {
    let mut state = session_state_for_test();

    assert!(state
        .mark_stream_tool_result_seen("call-1", "call-1", None)
        .is_err());
    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert!(state
        .mark_stream_tool_result_seen("call-1", "call-1", None)
        .is_err());
    assert_eq!(state.persist_assistant_turn(), 1);
    assert!(state
        .mark_stream_tool_result_seen("call-1", "call-1", None)
        .unwrap());
}

#[test]
fn transcript_turn_state_preserves_distinct_tool_results() {
    let mut state = session_state_for_test();

    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.persist_assistant_turn(), 1);
    assert!(state
        .mark_stream_tool_result_seen("internal-1", "result-1", Some("call-1"))
        .unwrap());
    assert!(state
        .mark_stream_tool_result_seen("internal-2", "result-2", Some("call-2"))
        .unwrap());
}

#[test]
fn transcript_turn_state_keeps_persisted_turn_across_parallel_results() {
    let mut state = session_state_for_test();

    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.persist_assistant_turn(), 1);
    // Every parallel result of the once-persisted turn passes the stream gate
    // (Lean: Transcript.parallel_results_complete_independently).
    assert!(state
        .mark_stream_tool_result_seen("internal-1", "result-1", Some("call-1"))
        .unwrap());
    assert!(state
        .mark_stream_tool_result_seen("internal-2", "result-2", Some("call-2"))
        .unwrap());
    assert!(state
        .mark_stream_tool_result_seen("internal-3", "result-3", Some("call-3"))
        .unwrap());
    // A persisted prior turn starts a NEW turn on the next assistant persist
    // (text-only final turn after tool results).
    assert_eq!(state.persist_assistant_turn(), 2);
}

#[test]
fn fail_closed_persistence_policy_terminates_and_records_failure() {
    let counters = hook_counters_for_test();
    let error = anyhow::anyhow!("synthetic persistence failure");

    let decision = decide_persistence_outcome(
        FailurePolicy::FailClosed,
        &counters,
        "unit-test failure",
        &error,
    );

    assert!(matches!(
        decision,
        PolicyDecision::Terminate(reason) if reason.contains("synthetic persistence failure")
    ));
    assert_eq!(counters.failures.load(Ordering::Relaxed), 1);
    assert_eq!(counters.successes.load(Ordering::Relaxed), 0);
}

#[test]
fn fail_open_persistence_policy_continues_without_success_ack() {
    let counters = hook_counters_for_test();
    let error = anyhow::anyhow!("synthetic persistence failure");

    let decision = decide_persistence_outcome(
        FailurePolicy::FailOpen,
        &counters,
        "unit-test failure",
        &error,
    );

    assert!(matches!(decision, PolicyDecision::Continue));
    assert_eq!(counters.failures.load(Ordering::Relaxed), 1);
    assert_eq!(
        counters.successes.load(Ordering::Relaxed),
        0,
        "fail-open continuation must not count as a successful storage ack"
    );
}

#[test]
fn generated_persistence_failure_policy_cases_match_hook_decisions() {
    let cases = lean_persistence_failure_policy_cases();
    assert_eq!(cases.len(), 2);

    for case in cases {
        let counters = hook_counters_for_test();
        let error = anyhow::anyhow!("generated persistence failure for {}", case.name);
        let decision = decide_persistence_outcome(
            failure_policy_from_contract(&case.policy),
            &counters,
            &case.name,
            &error,
        );
        let actual_decision = match decision {
            PolicyDecision::Continue => "continue",
            PolicyDecision::Terminate(_) => "terminate",
        };

        assert_eq!(case.action, "writeFail", "{}", case.name);
        assert_eq!(case.pre_persistence, "committing", "{}", case.name);
        assert_eq!(actual_decision, case.hook_decision, "{}", case.name);
        assert_eq!(
            counters.failures.load(Ordering::Relaxed),
            u64::from(case.records_failure),
            "{}",
            case.name
        );
        assert_eq!(
            counters.successes.load(Ordering::Relaxed),
            u64::from(case.records_success),
            "{}",
            case.name
        );
        assert!(
            !case.external_durability_claimed,
            "{} must not claim DefraDB durability",
            case.name
        );

        match case.policy.as_str() {
            "failClosed" => {
                assert_eq!(case.post_persistence, "uncommitted");
                assert_eq!(case.post_storage_observation, "mutationFailed");
            }
            "failOpen" => {
                assert_eq!(case.post_persistence, "lost");
                assert_eq!(case.post_storage_observation, "lostAcknowledged");
            }
            other => panic!("unknown Lean persistence failure policy {other:?}"),
        }
    }
}

#[tokio::test]
async fn generated_storage_observation_cases_match_hook_runtime_classification() {
    let cases = lean_storage_observation_runtime_cases();
    assert_eq!(cases.len(), 8);
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());

    for case in cases {
        if case.mutation_result == "notApplicable" {
            assert_eq!(case.hook_result, "notApplicable", "{}", case.name);
            assert!(!case.records_failure, "{}", case.name);
            assert!(!case.records_success, "{}", case.name);
        } else {
            let hook = DefraSessionHook::with_identity(
                node.clone(),
                "agent",
                "did:defra-agent:test",
                failure_policy_from_contract(&case.policy),
            );
            let result = match case.mutation_result.as_str() {
                "success" => Ok(()),
                "failure" => Err(anyhow::anyhow!(
                    "generated storage-observation failure for {}",
                    case.name
                )),
                other => panic!("unknown Lean mutation result {other:?}"),
            };
            let actual_result = hook.apply_persistence_policy(result, &case.name);
            let stats = hook.stats();

            assert_eq!(
                actual_result.is_ok(),
                case.hook_result == "ok",
                "{}",
                case.name
            );
            assert_eq!(
                stats.persistence_failures,
                u64::from(case.records_failure),
                "{}",
                case.name
            );
            assert_eq!(
                stats.persistence_successes,
                u64::from(case.records_success),
                "{}",
                case.name
            );
        }
        assert!(
            !case.external_visibility_claimed,
            "{} must not claim storage-engine visibility",
            case.name
        );

        match case.post_observation.as_str() {
            "successAcknowledged" => {
                assert_eq!(case.action, "mutationSuccess");
                assert_eq!(case.pre_observation, "inFlight");
                assert_eq!(case.post_persistence, "committed");
                assert!(case.terminal_write_observed, "{}", case.name);
            }
            "mutationFailed" => {
                assert_eq!(case.action, "mutationFailure");
                assert_eq!(case.pre_observation, "inFlight");
                assert_eq!(case.post_persistence, "uncommitted");
                assert!(!case.terminal_write_observed, "{}", case.name);
            }
            "lostAcknowledged" => {
                assert_eq!(case.action, "mutationFailure");
                assert_eq!(case.pre_observation, "inFlight");
                assert_eq!(case.post_persistence, "lost");
                assert!(!case.terminal_write_observed, "{}", case.name);
            }
            "staleObserved" => {
                assert!(
                    matches!(case.action.as_str(), "staleRead" | "staleEvent"),
                    "{}",
                    case.name
                );
                assert_eq!(case.pre_observation, "successAcknowledged");
                assert_eq!(case.post_persistence, "committed");
                assert!(!case.terminal_write_observed, "{}", case.name);
            }
            "readVisible" => {
                assert!(
                    matches!(case.action.as_str(), "readYourWrites" | "eventArrives"),
                    "{}",
                    case.name
                );
                assert!(
                    matches!(
                        case.pre_observation.as_str(),
                        "successAcknowledged" | "staleObserved"
                    ),
                    "{}",
                    case.name
                );
                assert_eq!(case.post_persistence, "committed");
                assert!(case.terminal_write_observed, "{}", case.name);
            }
            other => panic!("unexpected Lean storage observation {other:?}"),
        }
    }
}

async fn create_interruptible_request(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
    session_id: &str,
) {
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "did:defra-agent:general",
                behavior_id: "general",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "child request",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "subagent",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create interruptible request failed: {:?}",
        resp.errors
    );
}

async fn fetch_tool_call_row(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> serde_json::Value {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let tool_call_id = crate::graphql::escape_graphql_string(tool_call_id);
    let resp = node
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_call_id: {{ _eq: "{tool_call_id}" }}
                    }},
                    limit: 1
                ) {{
                    request_id
                    deadline_at
                    lifecycle_state
                    result
                    status
                    tool_failure_class
                }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool call failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("tool call row")
}

async fn fetch_tool_result_spill_row(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
    tool_name: &str,
) -> serde_json::Value {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let tool_name = crate::graphql::escape_graphql_string(tool_name);
    let resp = node
        .execute(&format!(
            r#"{{
                AgentToolResult(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_name: {{ _eq: "{tool_name}" }}
                    }},
                    limit: 1
                ) {{
                    output_text
                    truncated
                    truncation_metadata
                }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query spilled tool result failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolResult"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("spilled tool result row")
}

#[tokio::test]
async fn hook_attaches_active_request_deadline_to_tool_call_lifecycle() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-deadline-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Run a tool");
    assert!(matches!(
        hook.on_completion_call(&user_prompt, &[]).await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");
    let deadline = chrono::DateTime::parse_from_rfc3339("2026-05-08T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    hook.set_active_request_id(Some("req-deadline".to_string()))
        .await;
    hook.set_request_deadline_at(Some(deadline)).await;

    assert!(matches!(
        hook.on_tool_call("read", None, "internal-deadline", "{}")
            .await,
        ToolCallHookAction::Continue
    ));

    let row = fetch_tool_call_row(&node, &session_id, "internal-deadline").await;
    assert_eq!(
        row.get("request_id").and_then(|value| value.as_str()),
        Some("req-deadline")
    );
    let observed_deadline = chrono::DateTime::parse_from_rfc3339(
        row.get("deadline_at")
            .and_then(|value| value.as_str())
            .expect("deadline_at"),
    )
    .unwrap()
    .with_timezone(&chrono::Utc);
    assert_eq!(observed_deadline, deadline);

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn hook_maps_managed_timeout_result_to_timed_out_lifecycle() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-timeout-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Run"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");
    let deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    hook.set_active_request_id(Some("req-timeout".to_string()))
        .await;
    hook.set_request_deadline_at(Some(deadline)).await;

    assert!(matches!(
        hook.on_tool_call("never", None, "internal-timeout", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    let action = hook
        .on_tool_result(
            "never",
            None,
            "internal-timeout",
            "{}",
            &crate::tool_call_lifecycle::runtime::timeout_result(Some(deadline)),
        )
        .await;
    assert!(matches!(action, HookAction::Terminate { .. }));

    let row = fetch_tool_call_row(&node, &session_id, "internal-timeout").await;
    assert_eq!(
        row.get("lifecycle_state").and_then(|value| value.as_str()),
        Some("timedOut")
    );
    assert_eq!(
        row.get("tool_failure_class")
            .and_then(|value| value.as_str()),
        Some("external")
    );
    assert!(row
        .get("result")
        .and_then(|value| value.as_str())
        .is_some_and(|result| result.contains("deadline exceeded")));

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn hook_spills_full_tool_output_and_persists_bounded_observation() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-full-spill-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Run an oversized tool"), &[],)
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");
    hook.set_active_request_id(Some("req-oversized".to_string()))
        .await;

    let full_output = (0..2101)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tool_args = "{}";

    // The owned loop bounds the model-facing result itself and hands
    // on_tool_result the FULL output; on_tool_result spills the full text and
    // persists a bounded model observation carrying a spill pointer.
    assert!(matches!(
        hook.on_tool_call("oversized", None, "internal-oversized", tool_args,)
            .await,
        ToolCallHookAction::Continue
    ));
    assert!(matches!(
        hook.on_tool_result(
            "oversized",
            None,
            "internal-oversized",
            tool_args,
            &full_output,
        )
        .await,
        HookAction::Continue
    ));

    let tool_call = fetch_tool_call_row(&node, &session_id, "internal-oversized").await;
    let persisted_result = tool_call
        .get("result")
        .and_then(|value| value.as_str())
        .expect("persisted tool call result");
    assert!(persisted_result.contains("[Showing lines 1-2000 of 2101"));
    assert!(persisted_result.contains("[Full output: DefraDB doc"));
    assert!(!persisted_result.contains("line-2100"));
    assert_ne!(
        persisted_result, full_output,
        "persisted result should be the bounded observation with a spill pointer, not the full output"
    );

    let spill = fetch_tool_result_spill_row(&node, &session_id, "oversized").await;
    assert_eq!(
        spill.get("truncated").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        spill.get("output_text").and_then(|value| value.as_str()),
        Some(full_output.as_str())
    );
    let metadata: serde_json::Value = serde_json::from_str(
        spill
            .get("truncation_metadata")
            .and_then(|value| value.as_str())
            .expect("truncation metadata"),
    )
    .expect("metadata json");
    assert_eq!(
        metadata
            .get("original_lines")
            .and_then(|value| value.as_u64()),
        Some(2101)
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn cancelling_one_hook_does_not_cancel_unrelated_live_tool_call() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-cancel-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook_a = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let hook_b = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook_a
            .on_completion_call(&user_text_message("A"), &[])
            .await,
        HookAction::Continue
    ));
    assert!(matches!(
        hook_b
            .on_completion_call(&user_text_message("B"), &[])
            .await,
        HookAction::Continue
    ));
    let session_a = hook_a.session_id().await.expect("session a");
    let session_b = hook_b.session_id().await.expect("session b");
    let deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    hook_a
        .set_active_request_id(Some("req-a".to_string()))
        .await;
    hook_a.set_request_deadline_at(Some(deadline)).await;
    hook_b
        .set_active_request_id(Some("req-b".to_string()))
        .await;
    hook_b.set_request_deadline_at(Some(deadline)).await;

    assert!(matches!(
        hook_a.on_tool_call("slow", None, "internal-a", "{}").await,
        ToolCallHookAction::Continue
    ));
    assert!(matches!(
        hook_b.on_tool_call("slow", None, "internal-b", "{}").await,
        ToolCallHookAction::Continue
    ));

    assert_eq!(hook_a.cancel_in_flight_tool_calls().await.unwrap(), 1);

    let row_a = fetch_tool_call_row(&node, &session_a, "internal-a").await;
    assert_eq!(
        row_a
            .get("lifecycle_state")
            .and_then(|value| value.as_str()),
        Some("cancelled")
    );
    let row_b = fetch_tool_call_row(&node, &session_b, "internal-b").await;
    assert_eq!(
        row_b
            .get("lifecycle_state")
            .and_then(|value| value.as_str()),
        Some("running")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn cancelling_cascade_subagent_tool_latches_child_interrupt() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-hook-cascade-cancel-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let session_id = "session-cascade";
    let child_request_id = "child-cascade";
    create_interruptible_request(&node, child_request_id, session_id).await;

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new_subagent(
        node.clone(),
        "parent-cascade".to_string(),
        session_id.to_string(),
        "did:defra-agent:general".to_string(),
        "tool-cascade".to_string(),
        1,
        "spawn_agent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        crate::tool_call_lifecycle::AwaitMode::Foreground,
        crate::tool_call_lifecycle::CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("tool-cascade".to_string(), lifecycle);

    assert_eq!(hook.cancel_in_flight_tool_calls().await.unwrap(), 1);

    let parent_row = fetch_tool_call_row(&node, session_id, "tool-cascade").await;
    assert_eq!(
        parent_row
            .get("lifecycle_state")
            .and_then(|value| value.as_str()),
        Some("cancelled")
    );
    let child_interrupt = crate::interrupt::fetch_interrupt_requested_at(&node, child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade cancel should latch child interrupt_requested_at"
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn cancelling_detached_subagent_tool_does_not_interrupt_child() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-detach-cancel-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let session_id = "session-detach";
    let child_request_id = "child-detach";
    create_interruptible_request(&node, child_request_id, session_id).await;

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new_subagent(
        node.clone(),
        "parent-detach".to_string(),
        session_id.to_string(),
        "did:defra-agent:general".to_string(),
        "tool-detach".to_string(),
        1,
        "spawn_agent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        crate::tool_call_lifecycle::AwaitMode::Foreground,
        crate::tool_call_lifecycle::CancelPolicy::Detach,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("tool-detach".to_string(), lifecycle);

    assert_eq!(hook.cancel_in_flight_tool_calls().await.unwrap(), 1);

    let parent_row = fetch_tool_call_row(&node, session_id, "tool-detach").await;
    assert_eq!(
        parent_row
            .get("lifecycle_state")
            .and_then(|value| value.as_str()),
        Some("cancelled")
    );
    let child_interrupt = crate::interrupt::fetch_interrupt_requested_at(&node, child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_none(),
        "detached cancel must leave child request interrupt unset"
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn hook_can_fail_live_tool_call_without_conflating_timeout_or_cancel() {
    let data_path = std::env::temp_dir().join(format!("agent-hook-fail-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("fail"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");
    hook.set_active_request_id(Some("req-fail".to_string()))
        .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;

    assert!(matches!(
        hook.on_tool_call("slow", None, "internal-fail", "{}").await,
        ToolCallHookAction::Continue
    ));
    assert_eq!(
        hook.fail_in_flight_tool_calls(
            "stream liveness timeout while tool call was running",
            crate::tool_call_lifecycle::FailureClass::External,
        )
        .await
        .unwrap(),
        1
    );

    let row = fetch_tool_call_row(&node, &session_id, "internal-fail").await;
    assert_eq!(
        row.get("lifecycle_state").and_then(|value| value.as_str()),
        Some("failed")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn streaming_turn_persists_full_assistant_history_in_sequence() {
    let data_path =
        std::env::temp_dir().join(format!("agent-daemon-hook-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Inspect /tmp/main.rs");
    assert!(matches!(
        hook.on_completion_call(&user_prompt, &[]).await,
        HookAction::Continue
    ));

    let tool_args = r#"{"file_path":"/tmp/main.rs"}"#;
    assert!(matches!(
        hook.on_tool_call("read", Some("call-1".to_string()), "internal-1", tool_args,)
            .await,
        ToolCallHookAction::Continue
    ));

    assert!(matches!(
        hook.on_tool_result(
            "read",
            Some("call-1".to_string()),
            "internal-1",
            tool_args,
            "fn main() {}\n",
        )
        .await,
        HookAction::Continue
    ));

    let streamed_assistant_turn = Message::Assistant {
        id: None,
        content: vec![
            AssistantContent::Reasoning(
                Reasoning::new("Need to inspect the file first").with_id("rs_1".to_string()),
            ),
            AssistantContent::ToolCall(ToolCall {
                id: "internal-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: ToolFunction {
                    name: "read".to_string(),
                    arguments: json!({ "file_path": "/tmp/main.rs" }),
                },
                signature: None,
                additional_params: None,
            }),
            AssistantContent::Text(Text {
                text: "I'm reading the file now.".to_string(),
            }),
        ],
    };
    hook.persist_message(&streamed_assistant_turn)
        .await
        .unwrap();

    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "internal-1".to_string(),
            call_id: Some("call-1".to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: "ephemeral stream payload".to_string(),
            })],
        },
        "internal-1",
    )
    .await
    .unwrap();

    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: "The file looks healthy.".to_string(),
        })],
    })
    .await
    .unwrap();

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 4);

    assert!(matches!(
        &history[0],
        Message::User { content }
            if matches!(first_content(&content), UserContent::Text(Text { text }) if text == "Inspect /tmp/main.rs")
    ));
    assert!(matches!(
        &history[1],
        Message::Assistant { content, .. }
            if content.len() == 3
                && matches!(first_content(&content), AssistantContent::Reasoning(reasoning) if reasoning.id.as_deref() == Some("rs_1"))
                && matches!(content.iter().nth(1), Some(AssistantContent::ToolCall(tool_call)) if tool_call.call_id.as_deref() == Some("call-1"))
                && matches!(content.iter().nth(2), Some(AssistantContent::Text(Text { text })) if text == "I'm reading the file now.")
    ));
    assert!(matches!(
        &history[2],
        Message::User { content }
            if matches!(first_content(&content), UserContent::ToolResult(tool_result)
                if tool_result.call_id.as_deref() == Some("call-1")
                    && matches!(first_content(&tool_result.content), ToolResultContent::Text(Text { text }) if text == "fn main() {}\n"))
    ));
    assert!(matches!(
        &history[3],
        Message::Assistant { content, .. }
            if matches!(first_content(&content), AssistantContent::Text(Text { text }) if text == "The file looks healthy.")
    ));

    let resp = node
        .execute(&format!(
            r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{session_id}" }},
                            tool_call_id: {{ _eq: "internal-1" }}
                        }},
                        limit: 1
                    ) {{
                        message_sequence
                        result
                        status
                    }}
                }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool call failed: {:?}",
        resp.errors
    );

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("tool call row");

    assert_eq!(
        row.get("message_sequence").and_then(|value| value.as_u64()),
        Some(2)
    );
    assert_eq!(
        row.get("result").and_then(|value| value.as_str()),
        Some("fn main() {}\n")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

/// #492 durable reasoning: an assistant turn that carries chain-of-thought
/// reasoning persists that reasoning into the DURABLE `AgentMessage.reasoning`
/// field at materialize time. This is the Rust realization of the Lean
/// `finalizeComplete_copies_reasoning_then_clears` contract
/// (`durableReasoning := tailReasoning`): the durable copy is captured at
/// materialize independent of the live `AgentResponse.reasoning` tail, which
/// the #64 contract still clears on finalize (asserted separately by
/// `streaming::tests::write_reasoning_persists_on_response`).
#[tokio::test]
async fn assistant_turn_materializes_durable_reasoning_into_agent_message() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-hook-durable-reasoning-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Explain the plan");
    assert!(matches!(
        hook.on_completion_call(&user_prompt, &[]).await,
        HookAction::Continue
    ));

    // Assistant turn WITH reasoning + visible text.
    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![
            AssistantContent::Reasoning(Reasoning::new("First weigh the trade-offs, then answer.")),
            AssistantContent::Text(Text {
                text: "Here is the plan.".to_string(),
            }),
        ],
    })
    .await
    .unwrap();

    let session_id = hook.session_id().await.expect("session id");

    // Read the DURABLE AgentMessage rows directly (load_history decodes only
    // `content`; here we assert the dedicated `reasoning` column).
    let resp = node
        .execute(&format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    order: {{ sequence: ASC }}
                ) {{ role content reasoning }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query AgentMessage failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| value.as_array())
        .cloned()
        .expect("agent message rows");

    let assistant = rows
        .iter()
        .find(|row| row.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .expect("assistant row");

    // Durable reasoning persisted into the dedicated field.
    let reasoning = assistant
        .get("reasoning")
        .and_then(|value| value.as_str())
        .expect("reasoning field present");
    assert_eq!(
        reasoning, "First weigh the trade-offs, then answer.",
        "durable AgentMessage.reasoning must carry the assistant turn's reasoning"
    );

    // The user turn carries no reasoning (empty, not null) so the field
    // round-trips deterministically.
    let user = rows
        .iter()
        .find(|row| row.get("role").and_then(|v| v.as_str()) == Some("user"))
        .expect("user row");
    assert_eq!(
        user.get("reasoning").and_then(|value| value.as_str()),
        Some(""),
        "non-assistant rows carry empty durable reasoning"
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn read_file_result_persists_raw_output_but_models_compact_observation() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-hook-read-file-model-observation-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Read notes.txt"), &[])
            .await,
        HookAction::Continue
    ));

    let tool_args = r#"{"path":"notes.txt","start_line":2,"end_line":3}"#;
    assert!(matches!(
        hook.on_tool_call(
            "read_file",
            Some("call-read".to_string()),
            "internal-read",
            tool_args,
        )
        .await,
        ToolCallHookAction::Continue
    ));

    let raw_read_output = concat!(
        r#"defra_fs: {"ok":true,"status":"success","tool":"read_file","path":"notes.txt","returned_count":2,"total_count":3,"truncated":false,"start_line":2,"end_line":3}"#,
        "\ncontent:\nL2: beta\nL3: gamma"
    );
    assert!(matches!(
        hook.on_tool_result(
            "read_file",
            Some("call-read".to_string()),
            "internal-read",
            tool_args,
            raw_read_output,
        )
        .await,
        HookAction::Continue
    ));

    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "internal-read".to_string(),
            call_id: Some("call-read".to_string()),
            function: ToolFunction {
                name: "read_file".to_string(),
                arguments: json!({
                    "path": "notes.txt",
                    "start_line": 2,
                    "end_line": 3,
                }),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .await
    .unwrap();

    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "internal-read".to_string(),
            call_id: Some("call-read".to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: "ephemeral stream payload".to_string(),
            })],
        },
        "internal-read",
    )
    .await
    .unwrap();

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 3);

    let Message::User { content } = &history[2] else {
        panic!("expected tool result message");
    };
    let UserContent::ToolResult(tool_result) = first_content(&content) else {
        panic!("expected tool result content");
    };
    assert_eq!(tool_result.call_id.as_deref(), Some("call-read"));
    let ToolResultContent::Text(Text { text }) = first_content(&tool_result.content) else {
        panic!("expected text tool result content");
    };
    assert_eq!(
        text,
        "Read notes.txt (lines 2-3 of 3):\nL2: beta\nL3: gamma"
    );
    assert!(!text.contains("defra_fs"));

    let row = fetch_tool_call_row(&node, &session_id, "internal-read").await;
    assert_eq!(
        row.get("result").and_then(|value| value.as_str()),
        Some(raw_read_output)
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn duplicate_tool_result_message_observation_reuses_transcript_row() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-hook-tool-result-message-dedupe-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Inspect /tmp/main.rs"), &[])
            .await,
        HookAction::Continue
    ));

    let stored_call_id = "OaoTQYzCdoptKiK_mdhBA";
    let model_result_id = "c6b8bdeb-ab92-4481-b763-bdafbd463904";
    let tool_args = r#"{"file_path":"/tmp/main.rs"}"#;
    let tool_result_text = "fn main() {}\n";

    assert!(matches!(
        hook.on_tool_call(
            "read",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
        )
        .await,
        ToolCallHookAction::Continue
    ));

    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: model_result_id.to_string(),
            call_id: Some(model_result_id.to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: json!({ "file_path": "/tmp/main.rs" }),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .await
    .unwrap();

    assert!(matches!(
        hook.on_tool_result(
            "read",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
            tool_result_text,
        )
        .await,
        HookAction::Continue
    ));

    let duplicate_tool_result_message = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: model_result_id.to_string(),
            call_id: Some(model_result_id.to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: tool_result_text.to_string(),
            })],
        })],
    };
    let reused_sequence = hook
        .persist_message(&duplicate_tool_result_message)
        .await
        .unwrap();
    assert_eq!(
        reused_sequence, 3,
        "a duplicate observation must reuse the first tool-result message sequence"
    );

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(
        history.len(),
        3,
        "transcript should contain user prompt, assistant tool call, and one tool result"
    );

    let tool_results = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => match first_content(&content) {
                UserContent::ToolResult(tool_result) => Some(tool_result),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tool_results.len(),
        1,
        "one logical tool result must materialize as one transcript message"
    );
    assert_eq!(tool_results[0].id, model_result_id);
    assert_eq!(tool_results[0].call_id.as_deref(), Some(model_result_id));
    assert!(matches!(
        first_content(&tool_results[0].content),
        ToolResultContent::Text(Text { text }) if text == tool_result_text
    ));

    let resp = node
        .execute(&format!(
            r#"{{
                AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                    tool_call_key
                    tool_call_id
                }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool calls failed: {:?}",
        resp.errors
    );
    let tool_call_rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .expect("tool call rows");
    assert_eq!(tool_call_rows.len(), 1);
    let tool_call_keys = tool_call_rows
        .iter()
        .filter_map(|row| row.get("tool_call_key").and_then(|value| value.as_str()))
        .collect::<std::collections::HashSet<_>>();
    let tool_call_ids = tool_call_rows
        .iter()
        .filter_map(|row| row.get("tool_call_id").and_then(|value| value.as_str()))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(tool_call_keys.len(), 1);
    assert_eq!(tool_call_ids.len(), 1);
    assert_eq!(tool_call_ids.iter().next().copied(), Some(stored_call_id));

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn tool_result_message_dedupe_preserves_distinct_result_ids() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-hook-tool-result-distinct-message-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Run two tools"), &[])
            .await,
        HookAction::Continue
    ));

    for result_id in ["result-1", "result-2"] {
        hook.persist_message(&Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: result_id.to_string(),
                call_id: Some(result_id.to_string()),
                content: vec![ToolResultContent::Text(Text {
                    text: "same payload".to_string(),
                })],
            })],
        })
        .await
        .unwrap();
    }

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    let tool_results = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => match first_content(&content) {
                UserContent::ToolResult(tool_result) => Some(tool_result.id.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(tool_results, vec!["result-1", "result-2"]);

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn tool_call_after_saved_assistant_starts_new_turn_without_orphan_result() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-tool-turn-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Inspect mini-1");
    assert!(matches!(
        hook.on_completion_call(&user_prompt, &[]).await,
        HookAction::Continue
    ));

    assert!(matches!(
        hook.on_tool_call("first", None, "internal-1", "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "call-1".to_string(),
            call_id: None,
            function: ToolFunction {
                name: "first".to_string(),
                arguments: json!({}),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .await
    .unwrap();

    assert!(matches!(
        hook.on_tool_call("second", None, "internal-2", "{}").await,
        ToolCallHookAction::Continue
    ));
    assert!(matches!(
        hook.on_tool_result(
            "second",
            Some("call-2".to_string()),
            "internal-2",
            "{}",
            "second result",
        )
        .await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(
        history.len(),
        2,
        "tool result must not be persisted before its assistant turn"
    );

    let resp = node
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_call_id: {{ _eq: "internal-2" }}
                    }},
                    limit: 1
                ) {{ message_sequence result status }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool call failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("tool call row");
    assert_eq!(
        row.get("message_sequence").and_then(|value| value.as_u64()),
        Some(3)
    );
    assert_eq!(
        row.get("result").and_then(|value| value.as_str()),
        Some("second result")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );

    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "call-2".to_string(),
            call_id: None,
            function: ToolFunction {
                name: "second".to_string(),
                arguments: json!({}),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .await
    .unwrap();
    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "call-2".to_string(),
            call_id: None,
            content: vec![ToolResultContent::Text(Text {
                text: "stream fallback".to_string(),
            })],
        },
        "internal-2",
    )
    .await
    .unwrap();

    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 4);
    assert!(matches!(
        &history[2],
        Message::Assistant { content, .. }
            if matches!(first_content(&content), AssistantContent::ToolCall(tool_call)
                if tool_call.id == "call-2")
    ));
    assert!(matches!(
        &history[3],
        Message::User { content }
            if matches!(first_content(&content), UserContent::ToolResult(tool_result)
                if tool_result.id == "call-2"
                    && matches!(first_content(&tool_result.content), ToolResultContent::Text(Text { text }) if text == "second result"))
    ));

    let _ = std::fs::remove_dir_all(&data_path);
}
