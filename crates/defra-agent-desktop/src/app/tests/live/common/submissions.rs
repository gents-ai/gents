use super::*;

use defra_agent::graphql::escape_graphql_string;
use defra_agent_protocol::client_protocol::{
    derive_turn as derive_client_turn, AttemptView, ClientTurnState, RequestLifecycleState,
    RequestSnapshot, ResponseSnapshot, ResponseStatus,
};

pub(crate) fn wait_for_two_requests_in_flight(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    first_request_id: &str,
    second_request_id: &str,
) -> Result<()> {
    wait_for_value(
        "two live requests accepted before either response completed",
        Duration::from_secs(20),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let first_request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == first_request_id)?;
            let second_request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == second_request_id)?;
            let first_complete = snapshot
                .latest_response_for_request(first_request_id)
                .is_some_and(|row| matches!(row.status.as_deref(), Some("complete" | "completed")));
            let second_complete = snapshot
                .latest_response_for_request(second_request_id)
                .is_some_and(|row| matches!(row.status.as_deref(), Some("complete" | "completed")));

            (!first_complete
                && !second_complete
                && !matches!(
                    first_request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                )
                && !matches!(
                    second_request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                ))
            .then_some(())
        },
    )
}

pub(crate) fn assert_live_submission_rows(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    expected_backend_id: Option<&str>,
) -> Result<()> {
    wait_for_value(
        &format!("{label} submission rows for {}", deployment.label),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == submission.effective_request_id)?;
            let request_ok = request.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && request.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && request.session_id.as_deref() == Some(submission.session_id.as_str())
                && expected_backend_id
                    .is_none_or(|backend_id| request.backend_id.as_deref() == Some(backend_id))
                && request.content.as_deref() == Some(submission.prompt.as_str())
                && request
                    .failure_reason
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && !matches!(
                    request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                );

            let response =
                snapshot.latest_response_for_request(&submission.effective_request_id)?;
            let response_ok = response.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && response.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && response.session_id.as_deref() == Some(submission.session_id.as_str())
                && matches!(response.status.as_deref(), Some("complete" | "completed"))
                && response
                    .error_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && response
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(submission.response.trim()));

            let conversation_ok = snapshot
                .conversations
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                        && row.latest_request_id.as_deref()
                            == Some(submission.effective_request_id.as_str())
                });
            let session_ok = snapshot
                .sessions
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                });

            (request_ok && response_ok && conversation_ok && session_ok).then_some(())
        },
    )
}

pub(crate) fn assert_live_deployment_default_config(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    expected_model_name: &str,
) -> Result<()> {
    wait_for_value(
        &format!("{label} default config remains isolated"),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let behavior_ok = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.backend_id.as_deref() == Some(deployment.docs.backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(deployment.docs.inference_profile_id.as_str())
                        && row.tool_selection_id.as_deref()
                            == Some(deployment.docs.tool_selection_id.as_str())
                        && row.model_name.as_deref() == Some(expected_model_name)
                        && row.enabled == Some(true)
                });
            let tools_ok = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                        && row.enable_bash == Some(true)
                        && row.bash_mode.as_deref() == Some("ReadOnly")
                        && row.cli_tool_names == vec!["rg".to_string()]
                        && row.delegate_to.is_empty()
                });
            let profile_ok = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == deployment.docs.inference_profile_id)
                .is_some_and(|row| {
                    row.max_output_tokens == Some(1024)
                        && row.max_turns == Some(50)
                        && row.temperature == Some(0.0)
                });
            (behavior_ok && tools_ok && profile_ok).then_some(())
        },
    )
}

#[derive(Debug, Clone)]
pub(crate) struct GraphqlSubmittedRequest {
    pub(crate) request_id: String,
    pub(crate) session_id: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct GraphqlRequestRow {
    pub(crate) request_id: String,
    #[serde(default)]
    pub(crate) retry_parent_request: Option<String>,
    #[serde(default)]
    pub(crate) superseded_by_request: Option<String>,
    #[serde(default)]
    pub(crate) lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct GraphqlResponseRow {
    #[serde(default)]
    pub(crate) request_id: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct GraphqlMessageRow {
    #[serde(default)]
    pub(crate) message_key: Option<String>,
    #[serde(default)]
    pub(crate) sequence: Option<i64>,
    #[serde(default)]
    pub(crate) role: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) timestamp: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct GraphqlToolCallRow {
    #[serde(default)]
    pub(crate) tool_call_key: Option<String>,
    #[serde(default)]
    pub(crate) message_sequence: Option<i64>,
    #[serde(default)]
    pub(crate) tool_name: Option<String>,
    #[serde(default)]
    pub(crate) tool_call_id: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) args: Option<String>,
    #[serde(default)]
    pub(crate) result: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct GraphqlToolResultRow {
    #[serde(default)]
    pub(crate) agent_did: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) tool_name: Option<String>,
    #[serde(default)]
    pub(crate) tool_input: Option<String>,
    #[serde(default)]
    pub(crate) output_text: Option<String>,
    #[serde(default)]
    pub(crate) truncated: Option<bool>,
    #[serde(default)]
    pub(crate) truncation_metadata: Option<String>,
    #[serde(default)]
    pub(crate) conversation_doc_id: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GraphqlSessionShape {
    pub(crate) session_id: String,
    pub(crate) request_id: String,
    pub(crate) turn_state: Option<String>,
    pub(crate) request: Option<GraphqlRequestRow>,
    pub(crate) response: Option<GraphqlResponseRow>,
    pub(crate) messages: Vec<GraphqlMessageRow>,
    pub(crate) tool_calls: Vec<GraphqlToolCallRow>,
    pub(crate) tool_results: Vec<GraphqlToolResultRow>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GraphqlTurnState {
    pub(crate) request: Option<GraphqlRequestRow>,
    pub(crate) response: Option<GraphqlResponseRow>,
}

impl GraphqlTurnState {
    pub(crate) fn derived_turn_state(&self) -> Option<ClientTurnState> {
        let attempt = self.attempt_view()?;
        derive_client_turn(&[attempt])
    }

    pub(crate) fn response_is_durably_complete(&self) -> bool {
        self.request.as_ref().is_some_and(|request| {
            matches!(
                request.lifecycle_state.as_deref(),
                Some("completed" | "superseded")
            )
        }) && self.response.as_ref().is_some_and(|response| {
            matches!(response.status.as_deref(), Some("complete" | "completed"))
        })
    }

    pub(crate) fn successor_request_id(&self) -> Option<String> {
        self.request
            .as_ref()
            .and_then(|row| clean_optional_string(row.superseded_by_request.as_deref()))
    }

    fn attempt_view(&self) -> Option<AttemptView> {
        let request = self.request.as_ref()?;
        let lifecycle =
            RequestLifecycleState::try_from(request.lifecycle_state.as_deref().unwrap_or_default())
                .ok()?;

        Some(AttemptView {
            request: RequestSnapshot {
                request_id: request.request_id.clone(),
                retry_parent_request: clean_optional_string(
                    request.retry_parent_request.as_deref(),
                ),
                lifecycle_state: lifecycle,
                is_superseded: clean_optional_string(request.superseded_by_request.as_deref())
                    .is_some(),
            },
            response: self
                .response
                .as_ref()
                .and_then(graphql_response_status)
                .map(|status| ResponseSnapshot { status }),
        })
    }
}

pub(crate) fn create_live_agent_request_via_graphql(
    graphql_url: &str,
    agent_did: &str,
    content: &str,
    session_id: Option<&str>,
    behavior_id: Option<&str>,
) -> Result<GraphqlSubmittedRequest> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = session_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = chrono::Utc::now().to_rfc3339();
    let behavior_field = behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                r#"
                behavior_id: "{}","#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                {behavior_field}
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "{content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_field = behavior_field,
        session_id = escape_graphql_string(&session_id),
        content = escape_graphql_string(content),
        created_at = escape_graphql_string(&created_at),
    );
    execute_graphql(graphql_url, &mutation)?;

    Ok(GraphqlSubmittedRequest {
        request_id,
        session_id,
    })
}

pub(crate) fn fetch_graphql_turn_state(
    graphql_url: &str,
    request_id: &str,
) -> Result<GraphqlTurnState> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                request_id
                retry_parent_request
                superseded_by_request
                lifecycle_state
            }}
            AgentResponse(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                request_id
                status
                content
                error_message
            }}
        }}"#
    );

    let response = execute_graphql(graphql_url, &query)?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let request = data
        .get("AgentRequest")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let response_row = data
        .get("AgentResponse")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;

    Ok(GraphqlTurnState {
        request,
        response: response_row,
    })
}

pub(crate) fn fetch_graphql_session_shape(
    graphql_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<GraphqlSessionShape> {
    let escaped_session_id = escape_graphql_string(session_id);
    let turn_state = fetch_graphql_turn_state(graphql_url, request_id)?;
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ sequence: ASC }}) {{
                message_key
                sequence
                role
                content
                timestamp
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ message_sequence: ASC }}) {{
                tool_call_key
                message_sequence
                tool_name
                tool_call_id
                status
                args
                result
            }}
            AgentToolResult(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ created_at: ASC }}) {{
                agent_did
                session_id
                tool_name
                tool_input
                output_text
                truncated
                truncation_metadata
                conversation_doc_id
                created_at
            }}
        }}"#
    );
    let response = execute_graphql(graphql_url, &query)?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let messages = data
        .get("AgentMessage")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let tool_calls = data
        .get("AgentToolCall")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let tool_results = data
        .get("AgentToolResult")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();

    Ok(GraphqlSessionShape {
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        turn_state: turn_state
            .derived_turn_state()
            .map(|state| format!("{state:?}")),
        request: turn_state.request,
        response: turn_state.response,
        messages,
        tool_calls,
        tool_results,
    })
}

pub(crate) fn execute_graphql(url: &str, query: &str) -> Result<serde_json::Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .build()?;
    let mut last_error = None;

    for attempt in 0..5 {
        let response = client
            .post(url)
            .json(&serde_json::json!({ "query": query }))
            .send();
        let response = match response {
            Ok(response) => response,
            Err(error) if graphql_transport_error_is_retryable(&error) && attempt < 4 => {
                last_error =
                    Some(anyhow::Error::new(error).context(format!("posting GraphQL to {url}")));
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1) as u64));
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!("posting GraphQL to {url}")));
            }
        };

        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error) if graphql_transport_error_is_retryable(&error) && attempt < 4 => {
                last_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("reading GraphQL response from {url}")),
                );
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1) as u64));
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("reading GraphQL response from {url}")));
            }
        };

        let value: serde_json::Value = response.json()?;
        let errors = value
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !errors.is_empty() {
            anyhow::bail!("GraphQL query failed: {}", serde_json::Value::Array(errors));
        }
        return Ok(value);
    }

    Err(last_error.unwrap_or_else(|| anyhow!("GraphQL request retries exhausted for {url}")))
}

fn graphql_transport_error_is_retryable(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() || error.is_request() {
        return true;
    }

    let message = error.to_string();
    message.contains("connection closed before message completed")
        || message.contains("connection reset")
        || message.contains("broken pipe")
        || message.contains("channel closed")
        || message.contains("unexpected eof")
}

fn graphql_response_status(row: &GraphqlResponseRow) -> Option<ResponseStatus> {
    match row.status.as_deref().unwrap_or_default() {
        "streaming" => Some(ResponseStatus::Streaming),
        "complete" | "completed" => Some(ResponseStatus::Complete),
        "error" | "failed" | "failure" => Some(ResponseStatus::Error),
        _ => None,
    }
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
