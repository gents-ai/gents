use gents::defra_node::{EmbeddedNode, EventName};
use gents::graphql::escape_graphql_string;
use gents::llm::message::{AssistantContent, Message, Text, ToolResultContent, UserContent};
use gents::tool_call_lifecycle::{CancelCause, ToolCallLifecycle, ToolCallState};
use gents::{interrupt_request, AgentIdentity, BackgroundExecutionRegistry};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnRuntime, AcceptedTurnSpec,
};
use crate::support::fixtures::configure_behavior_tools;
use crate::support::streaming_backend::StreamChunk;
use crate::support::test_db;

const R6_BEHAVIOR_ID: &str = "r6-background";
const R6_BACKEND_ID: &str = "r6-background-backend";
const R6_MODEL: &str = "test-model";

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
    await_mode: Option<String>,
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    content: String,
    request_id: Option<String>,
    request_doc_id: Option<String>,
}

struct AcceptedBackgroundTurn {
    db: crate::support::TestDb,
    runtime: AcceptedTurnRuntime,
    executions: BackgroundExecutionRegistry,
    session_id: String,
    request_id: String,
    prompt: String,
}

async fn boot_background_turn(
    test_name: &str,
    accepted_chunks: Vec<StreamChunk>,
) -> AcceptedBackgroundTurn {
    boot_background_turn_with_bounds(test_name, accepted_chunks, None, None).await
}

async fn boot_background_turn_with_bounds(
    test_name: &str,
    accepted_chunks: Vec<StreamChunk>,
    valid_until: Option<&str>,
    execution_deadline_secs: Option<i64>,
) -> AcceptedBackgroundTurn {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    let prompt = format!("{test_name}-prompt");
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: R6_BACKEND_ID,
            model: R6_MODEL,
            parent_behavior_id: R6_BEHAVIOR_ID,
            configured_behavior_ids: &[R6_BEHAVIOR_ID],
            request_id: &request_id,
            session_id: &session_id,
            prompt: &prompt,
            accepted_chunks,
            child_plans: Vec::new(),
            valid_until,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    if let Some(deadline_duration_secs) = execution_deadline_secs {
        use gents::config_client::{
            read_desired_state_record_in_txn as read, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        let agent_did = db.node_identity.did();
        gents::ConfigAccess::transact_local(
            db.node.as_ref(),
            None,
            "test.configure_r6_execution_deadline",
            |txn| {
                Box::pin(async move {
                    let profile_id = format!("{R6_BEHAVIOR_ID}-inference");
                    let (_, mut profile) = read(
                        txn,
                        gents::Collection::InferenceProfile,
                        agent_did,
                        &profile_id,
                    )
                    .await?
                    .expect("R6 inference profile");
                    let execution_id = format!("{R6_BEHAVIOR_ID}-deadline");
                    profile["execution_id"] = execution_id.clone().into();
                    let execution =
                        serde_json::to_value(gents::document_config::InferenceExecution {
                            agent_did: agent_did.to_string(),
                            execution_id,
                            stream_liveness_timeout_secs: Some(1),
                            deadline_duration_secs: Some(deadline_duration_secs),
                            ..Default::default()
                        })?;
                    let plan = DesiredStateApplyPlan::new(vec![
                        DesiredStateApplyDocument {
                            collection: gents::Collection::InferenceProfile,
                            add: profile.clone(),
                            update: profile,
                        },
                        DesiredStateApplyDocument {
                            collection: gents::Collection::InferenceExecution,
                            add: execution.clone(),
                            update: execution,
                        },
                    ])?;
                    gents::config_client::apply_desired_state_plan(txn, &plan)
                        .await
                        .map(|_| ())
                })
            },
        )
        .await
        .expect("configure R6 execution deadline");
    }
    prepared.backend.enable_dynamic_followups(&prompt);
    configure_behavior_tools(
        db.node.as_ref(),
        db.node_identity.did(),
        R6_BEHAVIOR_ID,
        None,
        gents::document_config::Tools {
            tools_id: format!("{R6_BEHAVIOR_ID}:tools"),
            agent_did: db.node_identity.did().to_string(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec![
                        "sleep".to_string(),
                        "printf".to_string(),
                        "sh".to_string(),
                    ]),
                    background_enabled: true,
                    wait_timeout_secs: Some(1),
                    max_wait_timeout_secs: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build accepted background runtime");
    let executions = agent.background_execution_registry();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    AcceptedBackgroundTurn {
        db,
        runtime,
        executions,
        session_id,
        request_id,
        prompt,
    }
}

async fn fetch_messages(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID agent_did requester_did request_doc_id }}
            AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch AgentMessage rows failed: {:?}",
        response.errors
    );
    let request_ids = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some((
                row.get("_docID")?.as_str()?,
                row.get("request_id")?.as_str()?,
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let headers = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut messages = Vec::new();
    for header in headers {
        let header_doc_id = header["_docID"].as_str().expect("message header _docID");
        let agent_did = header["agent_did"].as_str().expect("message header agent");
        let requester_did = header["requester_did"].as_str();
        let request_doc_id = header["request_doc_id"].as_str().map(str::to_owned);
        let (_, message) = gents::session::load_canonical_message_from_node(
            node,
            header_doc_id,
            agent_did,
            requester_did,
        )
        .await
        .expect("reconstruct canonical message");
        let content = match message {
            Message::System { content } => content,
            Message::User { content } => content
                .into_iter()
                .filter_map(|item| match item {
                    UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Message::Assistant { content, .. } => content
                .into_iter()
                .filter_map(|item| match item {
                    AssistantContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        messages.push(MessageRow {
            content,
            request_id: request_doc_id
                .as_deref()
                .and_then(|doc_id| request_ids.get(doc_id).copied())
                .map(str::to_owned),
            request_doc_id,
        });
    }
    messages
}

async fn bounded_diagnostic<T: Debug>(
    timeout_duration: Duration,
    diagnostic: impl std::future::Future<Output = T>,
) -> String {
    match tokio::time::timeout(timeout_duration, diagnostic).await {
        Ok(value) => format!("{value:?}"),
        Err(_) => format!("<unavailable: diagnostic query timed out after {timeout_duration:?}>"),
    }
}

async fn wait_for_tool_completion_message(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> MessageRow {
    let marker = format!(r#"<tool-completion tool_call_id="{tool_call_id}""#);
    let mut updates = node.subscribe(&[EventName::Update]);
    let wait = async {
        loop {
            if let Some(message) = fetch_messages(node, session_id)
                .await
                .into_iter()
                .find(|message| message.content.contains(&marker))
            {
                return message;
            }
            updates
                .recv()
                .await
                .expect("embedded-node update subscription closed");
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), wait).await {
        Ok(message) => message,
        Err(_) => {
            let messages =
                bounded_diagnostic(Duration::from_secs(1), fetch_messages(node, session_id)).await;
            panic!(
                "tool completion message for {tool_call_id} was not appended; \
                 last_messages={messages}"
            );
        }
    }
}

async fn fetch_background_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<serde_json::Value> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}
            ) {{ _docID request_id input }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch background wake rows failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn load_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    fetch_tool_call(node, session_id, tool_call_id)
        .await
        .expect("AgentToolCall row")
}

async fn fetch_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<ToolCallRow> {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }}
                limit: 1
            ) {{
                tool_name
                tool_call_id
                lifecycle_state
                cancel_cause
                await_mode
                child_request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch AgentToolCall failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| serde_json::from_value(row.clone()).ok())
}

async fn canonical_tool_payload_json(
    db: &crate::support::TestDb,
    session_id: &str,
    provider_call_id: &str,
) -> Value {
    canonical_tool_payload_json_for(db, session_id, db.node_identity.did(), provider_call_id).await
}

async fn canonical_tool_payload_json_for(
    db: &crate::support::TestDb,
    session_id: &str,
    principal_did: &str,
    provider_call_id: &str,
) -> Value {
    let mut last_history = Vec::new();
    for _ in 0..200 {
        let history = gents::load_history(
            db.node.as_ref(),
            session_id,
            principal_did,
            Some(principal_did),
        )
        .await
        .expect("load canonical history");
        if let Some(payload) = history.iter().find_map(|message| match message {
            Message::User { content } => content.iter().find_map(|content| match content {
                UserContent::ToolResult(result)
                    if result.id == provider_call_id
                        || result.call_id.as_deref() == Some(provider_call_id) =>
                {
                    result.content.iter().find_map(|content| match content {
                        ToolResultContent::Text(Text { text }) => serde_json::from_str(text).ok(),
                        _ => None,
                    })
                }
                _ => None,
            }),
            _ => None,
        }) {
            return payload;
        }
        last_history = history;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "canonical tool-result payload missing for {provider_call_id}; history={last_history:#?}"
    )
}

async fn canonical_tool_result_text(
    db: &crate::support::TestDb,
    session_id: &str,
    provider_call_id: &str,
) -> String {
    let mut last_history = Vec::new();
    for _ in 0..400 {
        let history = gents::load_history(
            db.node.as_ref(),
            session_id,
            db.node_identity.did(),
            Some(db.node_identity.did()),
        )
        .await
        .expect("load canonical history");
        let results = history
            .iter()
            .flat_map(|message| match message {
                Message::User { content } => content.as_slice(),
                _ => &[],
            })
            .filter_map(|content| match content {
                UserContent::ToolResult(result)
                    if result.id == provider_call_id
                        || result.call_id.as_deref() == Some(provider_call_id) =>
                {
                    Some(result)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if !results.is_empty() {
            assert_eq!(
                results.len(),
                1,
                "one canonical reply for {provider_call_id}"
            );
            let [ToolResultContent::Text(Text { text })] = results[0].content.as_slice() else {
                panic!(
                    "expected one text part for {provider_call_id}: {:?}",
                    results[0]
                );
            };
            return text.clone();
        }
        last_history = history;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("canonical tool result missing for {provider_call_id}; history={last_history:#?}")
}

async fn canonical_latest_tool_payload_json(
    db: &crate::support::TestDb,
    session_id: &str,
    minimum_results: usize,
) -> Value {
    for _ in 0..200 {
        let history = gents::load_history(
            db.node.as_ref(),
            session_id,
            db.node_identity.did(),
            Some(db.node_identity.did()),
        )
        .await
        .expect("load canonical history");
        let payloads = history
            .iter()
            .flat_map(|message| match message {
                Message::User { content } => content.as_slice(),
                _ => &[],
            })
            .filter_map(|content| match content {
                UserContent::ToolResult(result) => {
                    result.content.iter().find_map(|content| match content {
                        ToolResultContent::Text(Text { text }) => serde_json::from_str(text).ok(),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect::<Vec<Value>>();
        if payloads.len() >= minimum_results {
            return payloads.into_iter().last().expect("latest tool result");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("fewer than {minimum_results} canonical tool-result payloads")
}

async fn wait_for_named_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_name: &str,
) -> ToolCallRow {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    for _ in 0..200 {
        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_name: {{ _eq: "{tool_name}" }} }}, limit: 1) {{ tool_call_id tool_name lifecycle_state cancel_cause await_mode child_request_id }} }}"#
            ))
            .await;
        if let Some(row) = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| serde_json::from_value(row.clone()).ok())
        {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let requests = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID request_id lifecycle_state deadline valid_until claimed_at terminalized_at failure_reason }} }}"#
        ))
        .await;
    let tools = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID request_id tool_call_id tool_name lifecycle_state cancel_cause }} }}"#
        ))
        .await;
    panic!(
        "tool {tool_name} was not durably persisted before provider follow-up; requests={:?}; tools={:?}",
        requests.data,
        tools.data,
    )
}

async fn wait_for_running_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> ToolCallRow {
    let timeout = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row = load_tool_call(node, session_id, tool_call_id).await;
        if row.lifecycle_state.as_deref() == Some("running") {
            return row;
        }
        assert_eq!(
            row.lifecycle_state.as_deref(),
            Some("pending"),
            "accepted process must start before recovery premise: {row:?}"
        );
        assert!(
            tokio::time::Instant::now() < timeout,
            "accepted process did not start: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_accepted_rows_visible(turn: &AcceptedBackgroundTurn) {
    let session_id = escape_graphql_string(&turn.session_id);
    let response = turn
        .db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID request_id request_doc_id requester_did tool_call_id tool_name lifecycle_state spawned_by_tool_call_doc_id }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "accepted-row query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(Value::as_array)
        .expect("AgentToolCall rows");
    assert!(
        !rows.is_empty(),
        "accepted provider result reached follow-up before durable headers; bodies={:?}",
        turn.runtime.backend.observed_completion_bodies()
    );
}

async fn wait_for_provider_requests(turn: &AcceptedBackgroundTurn, expected: usize) {
    for _ in 0..200 {
        if turn.runtime.backend.observed_requests(&turn.prompt) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("provider did not receive {expected} requests")
}

fn finish_dynamic_turn(turn: &AcceptedBackgroundTurn) {
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        crate::support::streaming_backend::StreamResponse::completes(
            &turn.prompt,
            ["parent complete"],
        ),
    );
}

async fn wait_for_initial_request_terminal(turn: &AcceptedBackgroundTurn) {
    let state = crate::support::live_inference::wait_for_request_terminal(
        turn.db.node.as_ref(),
        &turn.request_id,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state, "completed",
        "initial accepted request did not complete"
    );
}

#[tokio::test]
async fn background_diagnostic_snapshot_is_bounded_and_explicit() {
    let ready = bounded_diagnostic(Duration::from_secs(1), async { "ready" }).await;
    assert_eq!(ready, "\"ready\"");

    let unavailable =
        bounded_diagnostic(Duration::ZERO, std::future::pending::<&'static str>()).await;
    assert_eq!(
        unavailable,
        "<unavailable: diagnostic query timed out after 0ns>"
    );
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_name: {{ _eq: "{tool_name}" }}
                }}
            ) {{
                tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn background_tool_success_returns_handle_and_wait_tool_returns_terminal_envelope() {
    let turn = boot_background_turn(
        "r6-background-success",
        vec![StreamChunk::tool_call(
            "meta-bg-1",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"printf","args":["done"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    assert_accepted_rows_visible(&turn).await;
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            &turn.prompt,
            vec![StreamChunk::tool_call(
                "meta-wait-1",
                "wait_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
        ),
    );
    wait_for_provider_requests(&turn, 3).await;
    finish_dynamic_turn(&turn);
    let waited = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 2).await;
    assert_eq!(waited["status"], "completed", "unexpected wait: {waited}");
    assert!(waited["result"]
        .as_str()
        .is_some_and(|value| value.contains("done")));

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.tool_name.as_deref(), Some("bash"));
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(row.await_mode.as_deref(), Some("background"));
    assert_eq!(row.child_request_id.as_deref(), None);
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "wait_process").await,
        1
    );

    let message =
        wait_for_tool_completion_message(turn.db.node.as_ref(), &turn.session_id, &tool_call_id)
            .await;
    assert!(message.content.contains(r#"tool_name="bash""#));
    assert!(message.content.contains(r#"status="completed""#));
    assert!(message.content.contains("done"));
    let wakes = fetch_background_wakes(turn.db.node.as_ref(), &turn.session_id).await;
    assert_eq!(
        wakes.len(),
        1,
        "tool completion notification should enqueue one resumable agent turn"
    );
    let wake_request_id = wakes[0]["request_id"].as_str().unwrap();
    let wake_doc_id = wakes[0]["_docID"].as_str().unwrap();
    assert_ne!(wake_request_id, turn.request_id);
    assert_eq!(message.request_id.as_deref(), Some(wake_request_id));
    assert_eq!(message.request_doc_id.as_deref(), Some(wake_doc_id));
    turn.runtime.shutdown().await;
}

// #985: a backgrounded bash run's lifetime budget is decoupled from both the
// parent request deadline and the foreground command ceiling — the execution
// must complete (and notify) even though the parent deadline expires while it
// is still running.
#[tokio::test]
async fn background_tool_execution_survives_parent_request_deadline() {
    let turn = boot_background_turn("r6-background-outlives-deadline", Vec::new()).await;
    // This fixture enables follow-ups for the initial authored prompt. Supply
    // its terminal response before waiting, otherwise the mock correctly
    // treats the second provider call as a not-yet-authored dynamic response.
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    let release_dir = tempfile::tempdir().unwrap();
    let entered_path = release_dir.path().join("entered");
    let release_path = release_dir.path().join("release");
    let prompt = "start a process that outlives this request deadline";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "meta-bg-outlive",
                "spawn_process",
                serde_json::json!({
                    "tool_name": "bash",
                    "args": {
                        "command": "sh",
                        "args": [
                            "-c",
                            ": > \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.05; done; printf survived",
                            "gents-background-deadline",
                            entered_path.to_string_lossy(),
                            release_path.to_string_lossy()
                        ]
                    }
                })
                .to_string(),
            )],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["started"]),
    );
    let valid_until = (chrono::Utc::now() + chrono::Duration::seconds(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    crate::support::accepted_turn::enqueue_local_accepted_request_until(
        &turn.db,
        R6_BEHAVIOR_ID,
        "r6-background-outlives-deadline-request-2",
        &turn.session_id,
        prompt,
        Some(&valid_until),
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !entered_path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background process body entered");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(
        load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id)
            .await
            .lifecycle_state
            .as_deref(),
        Some("running"),
        "background process must remain live after its parent deadline"
    );
    std::fs::write(&release_path, b"release").unwrap();

    // The document-authored process outlives the accepted parent deadline.
    let marker = format!(r#"<tool-completion tool_call_id="{tool_call_id}""#);
    let mut message = None;
    for _ in 0..100 {
        if let Some(found) = fetch_messages(turn.db.node.as_ref(), &turn.session_id)
            .await
            .into_iter()
            .find(|message| message.content.contains(&marker))
        {
            message = Some(found);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let message = message.expect("background tool completion message was not appended");
    assert!(
        message.content.contains(r#"status="completed""#),
        "background tool must outlive the parent request deadline; got: {}",
        message.content
    );

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    let timeline = gents::run_timeline_fetch::load_run_timeline_rows(
        &gents::config_client::ConfigAccess::Local(turn.db.node.clone()),
        "r6-background-outlives-deadline-request-2",
    )
    .await
    .unwrap();
    assert_eq!(
        timeline
            .tool_calls
            .iter()
            .find(|call| call.tool_call_id == tool_call_id)
            .and_then(|call| call.result.as_deref()),
        Some("survived")
    );
    turn.runtime.shutdown().await;
}

// #985: wait_process is a bounded wait — on timeout it reports the process
// as still running without cancelling it, so a model that waits cannot pin
// the session (or kill the job) until the parent request deadline.
#[tokio::test]
async fn wait_process_bounded_wait_returns_still_running_without_cancelling() {
    let turn = boot_background_turn(
        "r6-background-wait-bounded",
        vec![StreamChunk::tool_call(
            "meta-bg-bounded",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    assert_accepted_rows_visible(&turn).await;

    let started = std::time::Instant::now();
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            &turn.prompt,
            vec![StreamChunk::tool_call(
                "meta-wait-bounded",
                "wait_process",
                serde_json::json!({ "tool_call_id": tool_call_id, "timeout_secs": 1 }).to_string(),
            )],
        ),
    );
    wait_for_provider_requests(&turn, 3).await;
    finish_dynamic_turn(&turn);
    let waited = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 2).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "bounded wait must return promptly, took {:?}",
        started.elapsed()
    );
    assert_eq!(waited["status"], "running", "unexpected wait: {waited}");
    assert_eq!(waited["error"]["reason"], "wait_timeout");

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some("running"),
        "wait timeout must not cancel the background process"
    );
    assert_eq!(row.cancel_cause.as_deref(), None);
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn periodic_recovery_does_not_terminalize_registered_background_worker() {
    assert_runtime_recovery_preserves_registered_worker(
        "r6-background-periodic-live-owner",
        "completed",
    )
    .await;
}

#[tokio::test]
async fn periodic_recovery_preserves_registered_worker_after_parent_interrupt() {
    assert_runtime_recovery_preserves_registered_worker(
        "r6-background-periodic-interrupted-owner",
        "interrupted",
    )
    .await;
}

async fn assert_runtime_recovery_preserves_registered_worker(test_name: &str, parent_state: &str) {
    let turn = boot_background_turn(
        test_name,
        vec![StreamChunk::tool_call(
            format!("{test_name}-spawn"),
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("registered process handle");
    wait_for_running_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    set_parent_state(turn.db.node.as_ref(), &turn.request_id, parent_state).await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(
        load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id)
            .await
            .lifecycle_state
            .as_deref(),
        Some("running")
    );
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn periodic_recovery_applies_deadline_before_terminal_parent_to_orphan() {
    let turn = boot_background_turn(
        "r6-background-periodic-deadline-precedence",
        vec![StreamChunk::tool_call(
            "meta-bg-expired-orphan",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("accepted background handle");
    // Acceptance first persists Pending; the orphan sweep only owns a process
    // after the real worker has moved it to Running.
    wait_for_running_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let response = turn.db.node.execute(&format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}, input: {{ deadline_at: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&tool_call_id), escape_graphql_string(&expired)
    )).await;
    assert!(
        !response.has_errors(),
        "expire accepted background row: {:?}",
        response.errors
    );
    set_parent_state(turn.db.node.as_ref(), &turn.request_id, "completed").await;

    gents::run_periodic_recovery_sweeps(
        &turn.db.node,
        turn.db.node_identity.did(),
        &BackgroundExecutionRegistry::default(),
    )
    .await
    .unwrap();
    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(row.cancel_cause.as_deref(), Some("deadline"));
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn malformed_running_row_does_not_hide_valid_orphan_recovery() {
    let turn = boot_background_turn(
        "r6-background-malformed-recovery-row",
        vec![StreamChunk::tool_call(
            "meta-bg-valid-orphan",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("accepted valid orphan");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    // A cleanly completed parent intentionally leaves its background job
    // running. Make the parent uncleanly terminal before testing orphan
    // recovery with an empty worker registry.
    set_parent_state(turn.db.node.as_ref(), &turn.request_id, "interrupted").await;

    let malformed = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "malformed-recovery-row",
                agent_did: "{}",
                lifecycle_state: "running",
                await_mode: "background"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(turn.db.node_identity.did())
    );
    let response = turn.db.node.execute(&malformed).await;
    assert!(
        !response.has_errors(),
        "failed to seed malformed recovery row: {:?}",
        response.errors
    );

    let before = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert!(
        matches!(
            before.lifecycle_state.as_deref(),
            Some("running" | "cancelled")
        ),
        "valid orphan must be running or already daemon-reconciled: {before:?}"
    );

    let report =
        gents::tool_call_lifecycle::ToolCallLifecycle::reconcile_orphaned_background_tools(
            &turn.db.node,
            turn.db.node_identity.did(),
            &BackgroundExecutionRegistry::default(),
        )
        .await
        .unwrap();
    let recovered = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(
        recovered.lifecycle_state.as_deref(),
        Some("cancelled"),
        "manual sweep report={report:?}; valid row={recovered:?}"
    );
    assert!(
        report.tool_calls_terminalized <= 1,
        "only one well-formed orphan can be due to this sweep: {report:?}"
    );
    if before.lifecycle_state.as_deref() == Some("cancelled") {
        assert_eq!(
            report.tool_calls_terminalized, 0,
            "daemon-reconciled row cannot be terminalized twice"
        );
    }
    let malformed_rows = turn.db.node.execute(r#"{ AgentToolCall(filter: { tool_call_key: { _eq: "malformed-recovery-row" } }) { lifecycle_state } }"#).await;
    assert!(!malformed_rows.has_errors(), "{:#?}", malformed_rows.errors);
    assert_eq!(
        malformed_rows.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "running",
        "malformed row must remain untouched"
    );
    turn.runtime.shutdown().await;
}

async fn set_parent_state(node: &EmbeddedNode, request_id: &str, lifecycle_state: &str) {
    let request_id = escape_graphql_string(request_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "failed to terminalize spawning request: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn wait_envelope_bounds_oversized_background_tool_result() {
    let big_line = "x".repeat(200);
    let big_output = std::iter::repeat(big_line)
        .take(500)
        .collect::<Vec<_>>()
        .join("\n");
    let full_len = big_output.len();
    let spawn_args = serde_json::json!({
        "tool_name": "bash",
        "args": { "command": "printf", "args": ["%s", big_output.clone()] }
    })
    .to_string();
    let turn = boot_background_turn(
        "r6-background-bounded",
        vec![StreamChunk::tool_call(
            "meta-bg-big",
            "spawn_process",
            spawn_args,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            &turn.prompt,
            vec![StreamChunk::tool_call(
                "meta-wait-big",
                "wait_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
        ),
    );
    wait_for_provider_requests(&turn, 3).await;
    finish_dynamic_turn(&turn);
    let waited = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 2).await;
    let terminal_row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(
        waited["status"],
        "completed",
        "wait status={:?}, error={}, result_bytes={}, result_prefix={:?}, terminal_row={terminal_row:?}",
        waited["status"],
        waited["error"],
        waited["result"].as_str().map_or(0, str::len),
        waited["result"].as_str().map(|s| s.chars().take(256).collect::<String>())
    );
    let envelope_result = waited["result"].as_str().expect("envelope result string");
    assert!(
        envelope_result.len() < full_len,
        "wait envelope must bound the model-facing result: envelope={} full={}",
        envelope_result.len(),
        full_len
    );
    assert!(
        !envelope_result.is_empty(),
        "bounded result must be non-empty"
    );

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    let timeline = gents::run_timeline_fetch::load_run_timeline_rows(
        &gents::config_client::ConfigAccess::Local(turn.db.node.clone()),
        &turn.request_id,
    )
    .await
    .unwrap();
    let full = timeline
        .tool_calls
        .iter()
        .find(|call| call.tool_call_id == tool_call_id)
        .and_then(|call| call.result.as_deref());
    assert_eq!(full, Some(big_output.as_str()));
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn background_tool_rejects_not_allowlisted_target() {
    let turn = boot_background_turn(
        "r6-background-not-allowed",
        vec![StreamChunk::tool_call(
            "meta-bg-denied",
            "spawn_process",
            r#"{"tool_name":"other_tool","args":{}}"#,
        )],
    )
    .await;
    turn.runtime.backend.wait_for_chunks(&turn.prompt, 1).await;
    wait_for_provider_requests(&turn, 2).await;
    assert_accepted_rows_visible(&turn).await;
    finish_dynamic_turn(&turn);
    let error = canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-bg-denied").await;
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], "other_tool");
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn background_tool_rejects_when_parent_budget_is_exhausted() {
    let mut chunks = (0..8)
        .map(|index| {
            StreamChunk::tool_call(
                format!("meta-bg-budget-{index}"),
                "spawn_process",
                r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
            )
        })
        .collect::<Vec<_>>();
    chunks.push(StreamChunk::tool_call(
        "meta-bg-budget-denied",
        "spawn_process",
        r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
    ));
    let turn = boot_background_turn("r6-background-budget", chunks).await;
    wait_for_provider_requests(&turn, 2).await;
    assert_accepted_rows_visible(&turn).await;
    finish_dynamic_turn(&turn);
    let denied = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 9).await;
    assert_eq!(
        denied["code"], "background_tool_budget_exceeded",
        "unexpected budget denial: {denied}"
    );
    assert_eq!(denied["current_backgrounded"], 8);
    assert_eq!(denied["max_backgrounded"], 8);
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "bash").await,
        8
    );
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn wait_tool_caller_deadline_returns_without_cancelling_background_row() {
    let turn = boot_background_turn_with_bounds(
        "r6-background-wait-deadline",
        vec![StreamChunk::tool_call(
            "meta-bg-wait-deadline",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
        None,
        Some(15),
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("accepted background handle");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    let prompt = "wait for process until caller deadline";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::Stream(
            crate::support::streaming_backend::StreamScript::paused_before(
                prompt,
                vec![StreamChunk::tool_call(
                    "meta-wait-deadline",
                    "wait_process",
                    serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
                )],
            ),
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["deadline handled"]),
    );
    let valid_until = (chrono::Utc::now() + chrono::Duration::minutes(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    crate::support::accepted_turn::enqueue_local_accepted_request_until(
        &turn.db,
        R6_BEHAVIOR_ID,
        "r6-background-wait-deadline-request-2",
        &turn.session_id,
        prompt,
        Some(&valid_until),
    )
    .await;
    // Wait for the claim owner to synthesize and persist the configured
    // execution deadline, then release inside its remaining budget. Admission
    // valid_until is intentionally a different, later clock.
    let request_id = escape_graphql_string("r6-background-wait-deadline-request-2");
    let persisted_deadline_at = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = turn.db.node.execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ lifecycle_state deadline }} }}"#,
            )).await;
            if let Some(deadline) = response.data.as_ref()
                .and_then(|data| data["AgentRequest"].as_array())
                .and_then(|rows| rows.first())
                .filter(|row| row["lifecycle_state"] == "processing")
                .and_then(|row| row["deadline"].as_str())
            {
                break chrono::DateTime::parse_from_rfc3339(deadline)
                    .expect("parse claimed execution deadline")
                    .with_timezone(&chrono::Utc);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("follow-up request was not claimed with an execution deadline");
    let release_at = persisted_deadline_at - chrono::Duration::milliseconds(900);
    let delay = (release_at - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    tokio::time::sleep(delay).await;
    turn.runtime.backend.release(prompt);
    let wait_row =
        wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "wait_process").await;
    let wait_tool_call_id = wait_row.tool_call_id.expect("accepted wait handle");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let current =
            load_tool_call(turn.db.node.as_ref(), &turn.session_id, &wait_tool_call_id).await;
        if current.lifecycle_state.as_deref() == Some("timedOut") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let requests = turn.db.node.execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state valid_until terminalized_at failure_reason terminal_output execution_generation }} }}"#,
                escape_graphql_string(&turn.session_id),
            )).await;
            let tools = turn.db.node.execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID request_id request_doc_id tool_call_id tool_name lifecycle_state cancel_cause tool_failure_class started_at completed_at message_sequence await_mode cancel_policy }} }}"#,
                escape_graphql_string(&turn.session_id),
            )).await;
            panic!(
                "accepted wait call did not follow the modeled request-deadline timeout owner: current={current:?}; requests={:?}; tools={:?}",
                requests.data,
                tools.data,
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let history = gents::load_history(
        &turn.db.node,
        &turn.session_id,
        turn.db.node_identity.did(),
        None,
    )
    .await
    .unwrap();
    assert!(
        history.iter().all(|message| !matches!(message,
            Message::User { content }
                if content.iter().any(|item| matches!(item,
                    UserContent::ToolResult(result) if result.id == "meta-wait-deadline")))),
        "an expired caller must not publish a successful late wait result"
    );

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(row.cancel_cause.as_deref(), None);
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "wait_process").await,
        1
    );
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn wait_tool_caller_interrupt_returns_without_cancelling_background_row() {
    let interrupt_case = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "caller_interrupt_cancels_wait_call_preserves_background_process",
    );
    let observer_case = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "caller_interrupt_observer_completes_wait_call_preserves_background_process",
    );
    assert!(interrupt_case.legal && observer_case.legal);
    let turn = boot_background_turn(
        "r6-background-wait-interrupt",
        vec![StreamChunk::tool_call(
            "meta-bg-interrupt",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("accepted background handle");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    let prompt = "wait for process until caller interruption";
    let request_id = "r6-background-wait-interrupt-request-2";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "meta-wait-interrupt",
                "wait_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["interrupt handled"]),
    );
    crate::support::accepted_turn::enqueue_local_accepted_request(
        &turn.db,
        R6_BEHAVIOR_ID,
        request_id,
        &turn.session_id,
        prompt,
    )
    .await;
    let wait_row =
        wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "wait_process").await;
    assert_eq!(wait_row.tool_name.as_deref(), Some("wait_process"));
    assert_eq!(wait_row.await_mode.as_deref(), Some("foreground"));
    let wait_tool_call_id = wait_row.tool_call_id.expect("accepted wait handle");
    wait_for_running_tool_call(turn.db.node.as_ref(), &turn.session_id, &wait_tool_call_id).await;
    interrupt_request(turn.db.node.as_ref(), request_id)
        .await
        .expect("interrupt caller request");

    let result_text =
        canonical_tool_result_text(&turn.db, &turn.session_id, "meta-wait-interrupt").await;
    let wait_row =
        load_tool_call(turn.db.node.as_ref(), &turn.session_id, &wait_tool_call_id).await;
    let wait_state = wait_row
        .lifecycle_state
        .as_deref()
        .and_then(ToolCallState::from_persisted)
        .expect("known wait lifecycle state");
    match result_text.as_str() {
        ToolCallLifecycle::CANCEL_DURING_RUN_OUTPUT => {
            assert_eq!(wait_state, ToolCallState::Cancelled);
            assert_eq!(
                wait_row.lifecycle_state.as_deref(),
                Some(interrupt_case.terminal_state.as_str())
            );
            assert_eq!(
                wait_row
                    .cancel_cause
                    .as_deref()
                    .and_then(CancelCause::from_persisted),
                Some(CancelCause::Interrupted)
            );
        }
        _ => {
            let waited: Value = serde_json::from_str(&result_text)
                .unwrap_or_else(|error| panic!("unexpected wait result {result_text:?}: {error}"));
            assert_eq!(wait_state, ToolCallState::Completed);
            assert_eq!(
                wait_row.lifecycle_state.as_deref(),
                Some(observer_case.terminal_state.as_str())
            );
            assert_eq!(wait_row.cancel_cause.as_deref(), None);
            assert_eq!(waited["tool_call_id"], tool_call_id);
            assert_eq!(waited["status"].as_str(), observer_case.result.as_deref());
            assert_eq!(
                waited["error"]["reason"].as_str(),
                observer_case.reason.as_deref()
            );
        }
    }

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        interrupt_case.result.as_deref()
    );
    assert_eq!(
        row.lifecycle_state.as_deref(),
        observer_case.result.as_deref()
    );
    assert_eq!(row.cancel_cause.as_deref(), None);
    gents::tool_control::cancel_session_background_process(
        turn.db.node.clone(),
        &turn.executions,
        turn.db.node_identity.did(),
        Some(turn.db.node_identity.did()),
        &turn.session_id,
        &tool_call_id,
    )
    .await
    .expect("cancel fixture process after checking interrupt isolation");
    tokio::time::timeout(
        Duration::from_secs(10),
        turn.executions.wait_for_completion(&tool_call_id),
    )
    .await
    .expect("fixture background process cleaned up");
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn process_controls_manage_same_principal_job_across_request_turns() {
    let turn = boot_background_turn(
        "r6-background-cross-turn-controls",
        vec![StreamChunk::tool_call(
            "meta-bg-cross-turn",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("cross-turn handle");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    let owner_after_first_turn =
        load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(
        owner_after_first_turn.lifecycle_state.as_deref(),
        Some("running"),
        "background process must survive the originating turn: {owner_after_first_turn:?}"
    );
    let prompt = "control existing process from second request";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![
                StreamChunk::tool_call("meta-list-cross-turn", "list_processes", r#"{}"#),
                StreamChunk::tool_call(
                    "meta-read-cross-turn",
                    "read_process",
                    serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
                ),
            ],
        ),
    );
    crate::support::accepted_turn::enqueue_local_accepted_request(
        &turn.db,
        R6_BEHAVIOR_ID,
        "r6-background-cross-turn-controls-request-2",
        &turn.session_id,
        prompt,
    )
    .await;
    turn.runtime.backend.wait_for_chunks(prompt, 1).await;
    let listed =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-list-cross-turn").await;
    assert!(
        listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["tool_call_id"] == tool_call_id),
        "list_processes lost accepted handle {tool_call_id}: {listed:#}"
    );

    let read =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-read-cross-turn").await;
    assert_eq!(read["status"], "running");
    assert_eq!(
        load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id)
            .await
            .lifecycle_state
            .as_deref(),
        Some("running"),
        "read_process must observe the job before a later accepted cancel turn"
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "meta-cancel-cross-turn",
                "cancel_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["controlled"]),
    );
    let cancelled =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-cancel-cross-turn").await;
    assert_eq!(cancelled["status"], "cancelled");
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn second_signed_principal_runtime_is_denied_foreign_process_handle() {
    let turn = boot_background_turn(
        "r6-background-cross-requester-denied",
        vec![StreamChunk::tool_call(
            "meta-bg-cross-requester",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("owner background handle");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;

    let foreign = std::sync::Arc::new(crate::support::fixtures::test_identity(
        "r6-foreign-requester",
    ));
    let foreign_behavior = "r6-background-foreign";
    let foreign_session = "r6-background-foreign-session";
    let foreign_prompt = "read a process owned by another principal";
    let prepared = crate::support::accepted_turn::prepare_accepted_turn_as(
        &turn.db,
        foreign.as_ref(),
        AcceptedTurnSpec {
            backend_id: "r6-background-foreign-backend",
            model: R6_MODEL,
            parent_behavior_id: foreign_behavior,
            configured_behavior_ids: &[foreign_behavior],
            request_id: "r6-background-foreign-request",
            session_id: foreign_session,
            prompt: foreign_prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                "meta-read-foreign-process",
                "read_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_behavior_tools(
        turn.db.node.as_ref(),
        foreign.did(),
        foreign_behavior,
        None,
        gents::document_config::Tools {
            tools_id: format!("{foreign_behavior}:tools"),
            agent_did: foreign.did().to_string(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sleep".to_string()]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let foreign_identity: Arc<dyn gents::AgentIdentity> = foreign.clone();
    let foreign_agent = gents::Gents::from_default_behavior_documents(
        turn.db.node.clone(),
        foreign_identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build foreign principal runtime");
    let foreign_runtime = boot_prepared_accepted_turn(&turn.db, prepared, foreign_agent).await;
    let denied = canonical_tool_payload_json_for(
        &turn.db,
        foreign_session,
        foreign.did(),
        "meta-read-foreign-process",
    )
    .await;
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["failure_class"], "tool_not_allowed");
    assert_eq!(denied["path"], "/tool_call_id");
    foreign_runtime.shutdown().await;
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "list_processes").await,
        0
    );
    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(row.cancel_cause.as_deref(), None);
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn list_processes_skips_malformed_legacy_rows_without_hiding_valid_jobs() {
    let turn = boot_background_turn(
        "r6-background-list-malformed-rows",
        vec![StreamChunk::tool_call(
            "meta-bg-valid-row",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let valid_tool_call_id = row.tool_call_id.expect("accepted valid process handle");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;

    let escaped_session_id = escape_graphql_string(&turn.session_id);
    let escaped_request_id = escape_graphql_string(&turn.request_id);
    let escaped_agent_did = escape_graphql_string(turn.db.node_identity.did());
    let malformed_rows = format!(
        r#"mutation {{
            null_identity: create_AgentToolCall(input: {{
                tool_call_key: "malformed-null-identity-{escaped_session_id}",
                session_id: "{escaped_session_id}",
                await_mode: "background",
                lifecycle_state: "running",
                started_at: "2026-05-14T00:00:02Z"
            }}) {{ _docID }}
            no_start: create_AgentToolCall(input: {{
                tool_call_key: "malformed-no-start-{escaped_session_id}",
                tool_call_id: "malformed-no-start",
                tool_name: "slow_tool",
                request_id: "{escaped_request_id}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                await_mode: "background",
                lifecycle_state: "running"
            }}) {{ _docID }}
        }}"#,
    );
    let response = turn.db.node.execute(&malformed_rows).await;
    assert!(
        !response.has_errors(),
        "seed malformed AgentToolCall rows failed: {:?}",
        response.errors
    );

    let prompt = "list processes while malformed legacy rows exist";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "meta-list-malformed",
                "list_processes",
                r#"{}"#,
            )],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["listed"]),
    );
    crate::support::accepted_turn::enqueue_local_accepted_request(
        &turn.db,
        R6_BEHAVIOR_ID,
        "r6-background-list-malformed-rows-request-2",
        &turn.session_id,
        prompt,
    )
    .await;
    let listed =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-list-malformed").await;
    let entries = listed["entries"]
        .as_array()
        .expect("list_processes must return entries despite malformed rows");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["tool_call_id"].as_str(),
        Some(valid_tool_call_id.as_str())
    );

    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn same_tool_background_calls_execute_concurrently_without_registry_mutex() {
    let release_dir = tempfile::tempdir().unwrap();
    let release_path = release_dir.path().join("release");
    let command_args = |marker: &str| {
        let entered_path = release_dir.path().join(marker);
        serde_json::json!({
            "tool_name": "bash",
            "args": {
                "command": "sh",
                "args": [
                    "-c",
                    "printf '%s\\n' \"$1\"; : > \"$2\"; while [ ! -f \"$3\" ]; do sleep 0.05; done",
                    "gents-background-gate",
                    marker,
                    entered_path.to_string_lossy(),
                    release_path.to_string_lossy()
                ]
            }
        })
        .to_string()
    };
    let turn = boot_background_turn(
        "r6-background-concurrent-tool",
        vec![
            StreamChunk::tool_call(
                "meta-bg-concurrent-1",
                "spawn_process",
                command_args("entered-one"),
            ),
            StreamChunk::tool_call(
                "meta-bg-concurrent-2",
                "spawn_process",
                command_args("entered-two"),
            ),
        ],
    )
    .await;
    let session = escape_graphql_string(&turn.session_id);
    let handles = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = turn.db.node.execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "bash" }}, lifecycle_state: {{ _eq: "running" }} }}) {{ tool_call_id }} }}"#
            )).await;
            let handles = response.data.as_ref()
                .and_then(|data| data["AgentToolCall"].as_array())
                .map(|rows| rows.iter().filter_map(|row| row["tool_call_id"].as_str().map(str::to_owned)).collect::<Vec<_>>())
                .unwrap_or_default();
            if handles.len() == 2 {
                break handles;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both calls to the same document tool must run concurrently");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if release_dir.path().join("entered-one").exists()
                && release_dir.path().join("entered-two").exists()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both real tool bodies must enter before release");
    finish_dynamic_turn(&turn);
    wait_for_initial_request_terminal(&turn).await;
    let prompt = "read both concurrently running process outputs";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            prompt,
            vec![
                StreamChunk::tool_call(
                    "meta-read-concurrent-1",
                    "read_process",
                    serde_json::json!({ "tool_call_id": handles[0] }).to_string(),
                ),
                StreamChunk::tool_call(
                    "meta-read-concurrent-2",
                    "read_process",
                    serde_json::json!({ "tool_call_id": handles[1] }).to_string(),
                ),
            ],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        crate::support::streaming_backend::StreamResponse::completes(prompt, ["observed"]),
    );
    crate::support::accepted_turn::enqueue_local_accepted_request(
        &turn.db,
        R6_BEHAVIOR_ID,
        "r6-background-concurrent-tool-request-2",
        &turn.session_id,
        prompt,
    )
    .await;
    let first =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-read-concurrent-1").await;
    let second =
        canonical_tool_payload_json(&turn.db, &turn.session_id, "meta-read-concurrent-2").await;
    let open_outputs = [first["output"].as_str(), second["output"].as_str()];
    assert!(open_outputs.contains(&Some("entered-one\n")));
    assert!(open_outputs.contains(&Some("entered-two\n")));
    std::fs::write(&release_path, b"release").unwrap();
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "bash").await,
        2
    );
    for handle in &handles {
        wait_for_tool_completion_message(turn.db.node.as_ref(), &turn.session_id, &handle).await;
    }
    let timeline = gents::run_timeline_fetch::load_run_timeline_rows(
        &gents::config_client::ConfigAccess::Local(turn.db.node.clone()),
        &turn.request_id,
    )
    .await
    .unwrap();
    let outputs = handles
        .iter()
        .map(|handle| {
            timeline
                .tool_calls
                .iter()
                .find(|call| &call.tool_call_id == handle)
                .and_then(|call| call.result.as_deref())
                .expect("completed process canonical raw output")
        })
        .collect::<Vec<_>>();
    assert!(outputs.iter().any(|output| *output == "entered-one\n"));
    assert!(outputs.iter().any(|output| *output == "entered-two\n"));
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn cancel_tool_cancels_running_background_row_through_canonical_control_call() {
    let turn = boot_background_turn(
        "r6-background-cancel",
        vec![StreamChunk::tool_call(
            "meta-bg-slow",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
    )
    .await;
    let row = wait_for_named_tool_call(turn.db.node.as_ref(), &turn.session_id, "bash").await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    assert_accepted_rows_visible(&turn).await;
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        crate::support::streaming_backend::StreamResponse::streams(
            &turn.prompt,
            vec![StreamChunk::tool_call(
                "meta-cancel-slow",
                "cancel_process",
                serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
            )],
        ),
    );
    wait_for_provider_requests(&turn, 3).await;
    finish_dynamic_turn(&turn);
    let cancelled = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 2).await;
    assert_eq!(cancelled["status"], "cancelled");

    let row = load_tool_call(turn.db.node.as_ref(), &turn.session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(row.cancel_cause.as_deref(), Some("userCancelled"));
    assert_eq!(
        count_tool_calls_by_name(turn.db.node.as_ref(), &turn.session_id, "cancel_process").await,
        1
    );

    let message =
        wait_for_tool_completion_message(turn.db.node.as_ref(), &turn.session_id, &tool_call_id)
            .await;
    assert!(message.content.contains(r#"status="cancelled""#));
    assert!(message.content.contains("<reason>explicit_cancel</reason>"));

    assert_eq!(
        fetch_background_wakes(turn.db.node.as_ref(), &turn.session_id)
            .await
            .len(),
        1,
        "tool cancellation notification should enqueue one resumable agent turn"
    );
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn cancel_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    let turn = boot_background_turn(
        "r6-background-cancel-missing",
        vec![StreamChunk::tool_call(
            "meta-cancel-missing",
            "cancel_process",
            r#"{"tool_call_id":"missing-background-handle"}"#,
        )],
    )
    .await;
    turn.runtime.backend.wait_for_chunks(&turn.prompt, 1).await;
    wait_for_provider_requests(&turn, 2).await;
    assert_accepted_rows_visible(&turn).await;
    finish_dynamic_turn(&turn);
    let cancelled = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 1).await;

    assert_eq!(cancelled["ok"], false);
    assert_eq!(cancelled["tool_name"], "cancel_process");
    assert!(cancelled["message"]
        .as_str()
        .unwrap()
        .contains("missing-background-handle"));
    turn.runtime.shutdown().await;
}

#[tokio::test]
async fn wait_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    let turn = boot_background_turn(
        "r6-background-wait-missing",
        vec![StreamChunk::tool_call(
            "meta-wait-missing",
            "wait_process",
            r#"{"tool_call_id":"missing-background-handle"}"#,
        )],
    )
    .await;
    turn.runtime.backend.wait_for_chunks(&turn.prompt, 1).await;
    wait_for_provider_requests(&turn, 2).await;
    assert_accepted_rows_visible(&turn).await;
    finish_dynamic_turn(&turn);
    let waited = canonical_latest_tool_payload_json(&turn.db, &turn.session_id, 1).await;

    assert_eq!(waited["ok"], false);
    assert_eq!(waited["tool_name"], "wait_process");
    assert!(waited["message"]
        .as_str()
        .unwrap()
        .contains("missing-background-handle"));
    turn.runtime.shutdown().await;
}
