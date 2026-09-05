use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use gents_protocol::row::{decode_behavior_readiness_snapshot, AgentBehaviorReadinessRow};
use serde_json::Value;

use super::fs::read_runtime_state_json;
use super::graphql::{escape_graphql_string, first_graphql_row, graphql_query};
use super::process::run_cli_json;

pub async fn wait_for_runtime_ready(
    graphql: &str,
    agent_did: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentBehaviorReadiness(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        agent_did
                        snapshot_json
                        updated_at
                    }}
                }}"#,
                escape_graphql_string(agent_did),
            ),
        )
        .await;
        match response {
            Ok(response) => {
                if let Ok(row) = first_graphql_row(&response, "AgentBehaviorReadiness") {
                    if let Ok(row) =
                        serde_json::from_value::<AgentBehaviorReadinessRow>(row.clone())
                    {
                        if let Ok(snapshot) = decode_behavior_readiness_snapshot(&row, agent_did) {
                            if snapshot.process_state
                                == gents_protocol::row::BehaviorReadinessProcessState::Ready
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
                if runtime_schema_is_starting(&error)
                    || error.chain().any(|cause| {
                        cause
                            .downcast_ref::<reqwest::Error>()
                            .is_some_and(reqwest::Error::is_connect)
                    }) => {}

            Err(error) => return Err(error),
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for authoritative runtime readiness for {agent_did}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_runtime_state_graphql(
    home_dir: &Path,
    expected_graphql: &str,
    timeout: Duration,
) -> Result<Value> {
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

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_runtime_quiescence(
    graphql: &str,
    agent_did: &str,
    minimum_generation: i64,
    quiet_period: Duration,
) -> Result<i64> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last_generation = None;
    let mut last_change_at = None;
    let mut last_runtime_row = None::<Value>;

    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentBehaviorReadiness(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        agent_did
                        snapshot_json
                        updated_at
                    }}
                    AgentRuntime(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        reconcile_phase
                        last_reconcile_result
                    }}
                }}"#,
                escape_graphql_string(agent_did),
                escape_graphql_string(agent_did),
            ),
        )
        .await?;
        if let (Ok(readiness_row), Ok(runtime_row)) = (
            first_graphql_row(&response, "AgentBehaviorReadiness"),
            first_graphql_row(&response, "AgentRuntime"),
        ) {
            last_runtime_row = Some(serde_json::json!({
                "readiness": readiness_row,
                "diagnostic": runtime_row,
            }));
            let decoded_row =
                serde_json::from_value::<AgentBehaviorReadinessRow>(readiness_row.clone())?;
            let readiness = decode_behavior_readiness_snapshot(&decoded_row, agent_did)
                .map_err(|reason| anyhow!("invalid behavior readiness: {reason:?}"))?;
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
                "timed out waiting for authoritative runtime quiescence at generation >= {minimum_generation} for {agent_did}; last_runtime_row={}",
                last_runtime_row
                    .map(|row| row.to_string())
                    .unwrap_or_else(|| "null".to_string())
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn runtime_schema_is_starting(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("Cannot query field")
        && (message.contains("AgentRuntime") || message.contains("AgentBehaviorReadiness"))
}

pub async fn wait_for_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{}" }},
                        content: {{ _eq: "{}" }}
                    }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    behavior_id
                }}
            }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(content),
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
            let behavior_id = row
                .get("behavior_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok((
                request_id.to_string(),
                session_id.to_string(),
                behavior_id.to_string(),
            ));
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn insert_terminal_response(
    graphql: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
    content: &str,
) -> Result<()> {
    let response_key = format!("response-{request_id}");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{response_key}",
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                content: "{content}",
                status: "complete",
                token_count: 0,
                progress_seq: 0,
                created_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        response_key = escape_graphql_string(&response_key),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
        session_id = escape_graphql_string(session_id),
        content = escape_graphql_string(content),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub async fn wait_for_connected_peer(
    home_dir: &std::path::Path,
    peer_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut last_error = "none".to_string();
    loop {
        match run_cli_json(home_dir, &["p2p", "peers"]) {
            Ok(status) => {
                last_error.clear();
                if status
                    .get("peers")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| {
                        rows.iter()
                            .filter_map(Value::as_str)
                            .any(|row| row.contains(peer_id))
                    })
                {
                    return Ok(status);
                }
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for connected peer {peer_id}; last_error={}",
                if last_error.is_empty() {
                    "none"
                } else {
                    &last_error
                }
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn wait_for_tool_call(graphql: &str, session_id: &str, tool_name: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
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
                        tool_name
                        args
                        result
                        status
                    }}
                }}"#,
                escape_graphql_string(session_id),
                escape_graphql_string(tool_name),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentToolCall") {
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if status == "completed" {
                return Ok(row.clone());
            }
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentToolCall {tool_name} in session {session_id}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn wait_for_completed_tool_calls(
    graphql: &str,
    session_id: &str,
    tool_name: &str,
    minimum_count: usize,
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(30);
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
                        tool_name
                        args
                        result
                        status
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
            .filter(|row| row.get("status").and_then(Value::as_str) == Some("completed"))
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn wait_for_completed_inference_behaviors(
    graphql: &str,
    backend_id: &str,
    expected_behavior_ids: &[&str],
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(30);
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
                        behavior_id
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
        let all_completed = expected_behavior_ids.iter().all(|expected| {
            rows.iter().any(|row| {
                row.get("behavior_id").and_then(Value::as_str) == Some(*expected)
                    && row.get("call_kind").and_then(Value::as_str) == Some("inference")
                    && row.get("call_state").and_then(Value::as_str) == Some("completed")
            })
        });
        if all_completed {
            return Ok(rows);
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for completed inference calls on backend {backend_id} for behaviors {:?}; last rows={}",
                expected_behavior_ids,
                Value::Array(rows)
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
