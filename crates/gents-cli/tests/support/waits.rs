use gents::support::snapshots::SnapshotReader;
use gents_store::config_client::ConfigAccess;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;

use anyhow::{anyhow, bail, Result};
use gents_protocol::row::{decode_node_readiness_snapshot, NodeReadinessRow};
use serde_json::Value;

use super::fs::read_runtime_state_json;
use super::graphql::{escape_graphql_string, first_graphql_row, graphql_query};

pub async fn wait_for_runtime_ready(
    graphql: &str,
    node_did: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("NodeReadiness").await;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    NodeReadiness(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        node_did
                        snapshot_json
                        updated_at
                    }}
                }}"#,
                escape_graphql_string(node_did),
            ),
        )
        .await;
        let last_observation = format!("{response:?}");
        match response {
            Ok(response) => {
                if let Ok(row) = first_graphql_row(&response, "NodeReadiness") {
                    if let Ok(row) = serde_json::from_value::<NodeReadinessRow>(row.clone()) {
                        if let Ok(snapshot) = decode_node_readiness_snapshot(&row, node_did) {
                            if snapshot.process_state
                                == gents_protocol::row::NodeReadinessProcessState::Ready
                                && snapshot.active_generation > 0
                                && snapshot.router_generation == snapshot.active_generation
                            {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            Err(error)
                if runtime_schema_is_starting(&error) || runtime_http_is_starting(&error) => {}

            Err(error) => return Err(error),
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for authoritative runtime readiness for {node_did}; last observation: {last_observation}");
        }

        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

pub async fn wait_for_runtime_state_graphql(
    home_dir: &Path,
    expected_graphql: &str,
    timeout: Duration,
) -> Result<Value> {
    use notify::Watcher;
    let (tx, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })?;
    watcher.watch(home_dir, notify::RecursiveMode::Recursive)?;
    let deadline = Instant::now() + timeout;
    let mut last_state = None::<Value>;
    let mut last_error: Option<anyhow::Error>;

    loop {
        match read_runtime_state_json(home_dir) {
            Ok(state) => {
                if state.get("graphql").and_then(Value::as_str) == Some(expected_graphql) {
                    return Ok(state);
                }
                last_state = Some(state);
                last_error = None;
            }
            Err(error) => {
                last_error = Some(error);
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for runtime.json graphql={expected_graphql}; last_state={}; last_error={}",
                last_state
                    .map(|state| state.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "none".to_string())
            );
        }

        if let Ok(Some(event)) = tokio::time::timeout_at(deadline, changes.recv()).await {
            event?;
        }
    }
}

pub async fn wait_for_runtime_quiescence(
    graphql: &str,
    node_did: &str,
    minimum_generation: i64,
    quiet_period: Duration,
) -> Result<i64> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut readiness_changes = access.subscribe_changes("NodeReadiness").await;
    let mut runtime_changes = access.subscribe_changes("NodeRuntime").await;
    let mut last_generation = None;
    let mut last_change_at = None;
    let mut last_runtime_row = None::<Value>;

    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    NodeReadiness(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        node_did
                        snapshot_json
                        updated_at
                    }}
                    NodeRuntime(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        reconcile_phase
                        last_reconcile_result
                    }}
                }}"#,
                escape_graphql_string(node_did),
                escape_graphql_string(node_did),
            ),
        )
        .await?;
        if let (Ok(readiness_row), Ok(runtime_row)) = (
            first_graphql_row(&response, "NodeReadiness"),
            first_graphql_row(&response, "NodeRuntime"),
        ) {
            last_runtime_row = Some(serde_json::json!({
                "readiness": readiness_row,
                "diagnostic": runtime_row,
            }));
            let decoded_row = serde_json::from_value::<NodeReadinessRow>(readiness_row.clone())?;
            let readiness = decode_node_readiness_snapshot(&decoded_row, node_did)
                .map_err(|reason| anyhow!("invalid agent readiness: {reason:?}"))?;
            let generation = i64::try_from(readiness.active_generation).unwrap_or(i64::MAX);
            let phase = runtime_row
                .get("reconcile_phase")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = runtime_row
                .get("last_reconcile_result")
                .and_then(Value::as_str)
                .unwrap_or_default();

            if generation >= minimum_generation
                && readiness.router_generation == readiness.active_generation
                && phase == "idle"
                && matches!(result, "startup" | "applied" | "noop")
            {
                let now = Instant::now();
                match last_generation {
                    Some(previous) if previous == generation => {
                        if last_change_at.is_some_and(|changed_at| {
                            now.duration_since(changed_at) >= quiet_period
                        }) {
                            return Ok(generation);
                        }
                    }
                    _ => {
                        last_generation = Some(generation);
                        last_change_at = Some(now);
                    }
                }
            } else {
                last_generation = None;
                last_change_at = None;
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for authoritative runtime quiescence at generation >= {minimum_generation} for {node_did}; last_runtime_row={}",
                last_runtime_row
                    .map(|row| row.to_string())
                    .unwrap_or_else(|| "null".to_string())
            );
        }
        let wake = last_change_at
            .map(|at| at + quiet_period)
            .unwrap_or(deadline)
            .min(deadline);
        let _ = tokio::time::timeout_at(wake, async {
            tokio::select! {
                _ = readiness_changes.recv() => {},
                _ = runtime_changes.recv() => {},
            }
        })
        .await;
    }
}

/// The listener accepts connections before serve opens client access; until
/// then its startup gate answers 503 to every path but the bind probe.
fn runtime_http_is_starting(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<reqwest::Error>().is_some_and(|error| {
            error.is_connect() || error.status() == Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
        })
    })
}

fn runtime_schema_is_starting(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("Cannot query field")
        && (message.contains("NodeRuntime") || message.contains("NodeReadiness"))
}

pub async fn wait_for_request(
    graphql: &str,
    node_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ {} }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    agent_did
                }}
            }}"#,
            gents_session::session::public_request_filter(&format!(
                r#"node_did: {{ _eq: "{}" }}, content: {{ _eq: "{}" }}"#,
                escape_graphql_string(node_did),
                escape_graphql_string(content),
            )),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing session_id: {row}"))?;
            let agent_did = row
                .get("agent_did")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok((
                request_id.to_string(),
                session_id.to_string(),
                agent_did.to_string(),
            ));
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest for {node_did}");
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

pub async fn wait_for_request_lifecycle_state(
    graphql: &str,
    request_id: &str,
    expected_states: &[&str],
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut last_row = None::<Value>;
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    request_id
                    lifecycle_state
                    interrupt_requested_at
                    failure_reason
                }}
            }}"#,
            escape_graphql_string(request_id),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            last_row = Some(row.clone());
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if expected_states.contains(&lifecycle_state) {
                return Ok(row.clone());
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for request {request_id} lifecycle_state in {:?}; last row={}",
                expected_states,
                last_row
                    .map(|row| row.to_string())
                    .unwrap_or_else(|| "null".to_string())
            );
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

pub async fn wait_for_tool_call(graphql: &str, session_id: &str, tool_name: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("AgentToolCall").await;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{}" }},
                            tool_name: {{ _eq: "{}" }}
                        }},
                        order: {{ started_at: DESC }},
                        limit: 1
                    ) {{
                        _docID
                        node_did
                        requester_did
                        request_doc_id
                        session_id
                        tool_name
                        lifecycle_state
                    }}
                }}"#,
                escape_graphql_string(session_id),
                escape_graphql_string(tool_name),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentToolCall") {
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if lifecycle_state == "completed" {
                return Ok(row.clone());
            }
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentToolCall {tool_name} in session {session_id}");
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

pub async fn wait_for_completed_tool_calls(
    graphql: &str,
    session_id: &str,
    tool_name: &str,
    minimum_count: usize,
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("AgentToolCall").await;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{}" }},
                            tool_name: {{ _eq: "{}" }}
                        }},
                        order: {{ started_at: ASC }}
                    ) {{
                        _docID
                        node_did
                        requester_did
                        request_doc_id
                        session_id
                        tool_name
                        lifecycle_state
                    }}
                }}"#,
                escape_graphql_string(session_id),
                escape_graphql_string(tool_name),
            ),
        )
        .await?;
        let rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let completed = rows
            .iter()
            .filter(|row| row.get("lifecycle_state").and_then(Value::as_str) == Some("completed"))
            .cloned()
            .collect::<Vec<_>>();
        if completed.len() >= minimum_count {
            return Ok(completed);
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for {minimum_count} completed {tool_name} tool call(s) in session {session_id}; last rows={}",
                Value::Array(rows)
            );
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

/// Read the exact invocation reply through the runtime's canonical
/// physical-tool-document owner; a completed tool can precede replicated
/// delivery briefly, so wait for the addressed reply to become visible.
pub async fn canonical_tool_result_text(graphql: &str, call: &Value) -> Result<String> {
    use gents_session::tool_call::query::result::{load_tool_call_result, render_tool_result};
    use gents_store::config_client::ConfigAccess;

    let required = |field: &str| -> Result<&str> {
        call.get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("completed tool row missing {field}: {call}"))
    };
    let tool_doc_id = required("_docID")?;
    let node_did = required("node_did")?;
    let session_id = required("session_id")?;
    required("request_doc_id")?;
    let requester_did = call.get("requester_did").and_then(Value::as_str);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut changes = access.subscribe_changes("AgentMessage").await;
    let mut segments = access.subscribe_changes("AgentOutputSegment").await;
    loop {
        match load_tool_call_result(&access, tool_doc_id, node_did, session_id, requester_did).await
        {
            Ok(message) => return render_tool_result(&message),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                let _ = tokio::time::timeout_at(deadline, async {
                    tokio::select! { _ = changes.recv() => {}, _ = segments.recv() => {} }
                })
                .await;
            }
            Err(error) => return Err(error),
        }
    }
}

pub async fn wait_for_completed_inference_agents(
    graphql: &str,
    backend_id: &str,
    expected_agent_dids: &[&str],
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let access = ConfigAccess::Graphql(super::graphql::served_endpoint(graphql));
    let mut changes = access.subscribe_changes("InferenceCall").await;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    InferenceCall(
                        filter: {{ backend_id: {{ _eq: "{}" }} }},
                        order: {{ queued_at: ASC }}
                    ) {{
                        request_id
                        agent_did
                        backend_id
                        call_kind
                        call_state
                    }}
                }}"#,
                escape_graphql_string(backend_id),
            ),
        )
        .await?;
        let rows = response
            .pointer("/data/InferenceCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let all_completed = expected_agent_dids.iter().all(|expected| {
            rows.iter().any(|row| {
                row.get("agent_did").and_then(Value::as_str) == Some(*expected)
                    && row.get("call_kind").and_then(Value::as_str) == Some("inference")
                    && row.get("call_state").and_then(Value::as_str) == Some("completed")
            })
        });
        if all_completed {
            return Ok(rows);
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for completed inference calls on backend {backend_id} for agents {:?}; last rows={}",
                expected_agent_dids,
                Value::Array(rows)
            );
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}
