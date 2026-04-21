use super::*;

use defra_agent_protocol::graphql::{
    create_agent_request_mutation, execute_graphql_blocking, parse_session_shape_response,
    parse_turn_state_response, session_shape_query, turn_state_query, CreateAgentRequestInput,
    GraphqlRequestOptions, GraphqlSessionShape, GraphqlSubmittedRequest, GraphqlTurnState,
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
    assert_live_submission_rows_with_options(
        runtime,
        core,
        label,
        deployment,
        submission,
        expected_backend_id,
        SubmissionRowAssertOptions::default(),
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SubmissionRowAssertOptions {
    pub(crate) timeout: Duration,
    pub(crate) require_response_content_match: bool,
}

impl Default for SubmissionRowAssertOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            require_response_content_match: true,
        }
    }
}

#[derive(Debug, Clone)]
struct SubmissionRowProgress {
    request_ok: bool,
    response_ok: bool,
    conversation_ok: bool,
    session_ok: bool,
    request_lifecycle_state: String,
    response_status: String,
    conversation_latest_request_id: String,
    response_preview: String,
}

pub(crate) fn assert_live_submission_rows_with_options(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    expected_backend_id: Option<&str>,
    options: SubmissionRowAssertOptions,
) -> Result<()> {
    let deadline = Instant::now() + options.timeout;

    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();

        let request = snapshot
            .requests
            .iter()
            .find(|row| row.request_id == submission.effective_request_id);
        let request_ok = request.is_some_and(|request| {
            request.agent_did.as_deref() == Some(deployment.agent_did.as_str())
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
                )
        });

        let response = snapshot.latest_response_for_request(&submission.effective_request_id);
        let response_ok = response.is_some_and(|response| {
            let content_ok = response.content.as_deref().is_some_and(|content| {
                let trimmed = content.trim();
                !trimmed.is_empty()
                    && (!options.require_response_content_match
                        || trimmed.contains(submission.response.trim()))
            });
            response.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && response.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && response.session_id.as_deref() == Some(submission.session_id.as_str())
                && matches!(response.status.as_deref(), Some("complete" | "completed"))
                && response
                    .error_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && content_ok
        });

        let conversation = snapshot
            .conversations
            .iter()
            .find(|row| row.session_id == submission.session_id);
        let conversation_ok = conversation.is_some_and(|row| {
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

        if request_ok && response_ok && conversation_ok && session_ok {
            return Ok(());
        }

        let progress = SubmissionRowProgress {
            request_ok,
            response_ok,
            conversation_ok,
            session_ok,
            request_lifecycle_state: request
                .and_then(|row| row.lifecycle_state.as_deref())
                .unwrap_or_default()
                .to_string(),
            response_status: response
                .and_then(|row| row.status.as_deref())
                .unwrap_or_default()
                .to_string(),
            conversation_latest_request_id: conversation
                .and_then(|row| row.latest_request_id.as_deref())
                .unwrap_or_default()
                .to_string(),
            response_preview: response
                .and_then(|row| row.content.as_deref())
                .map(compact_response_preview_for_assert)
                .unwrap_or_default(),
        };

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for {label} submission rows for {}: request_ok={} response_ok={} conversation_ok={} session_ok={} effective_request_id={} request_lifecycle_state={} response_status={} conversation_latest_request_id={} response_preview={}",
                deployment.label,
                progress.request_ok,
                progress.response_ok,
                progress.conversation_ok,
                progress.session_ok,
                submission.effective_request_id,
                progress.request_lifecycle_state,
                progress.response_status,
                progress.conversation_latest_request_id,
                progress.response_preview,
            );
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn compact_response_preview_for_assert(content: &str) -> String {
    const LIMIT: usize = 160;
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= LIMIT {
        normalized
    } else {
        let truncated = normalized.chars().take(LIMIT).collect::<String>();
        format!("{truncated}...")
    }
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
    let mutation = create_agent_request_mutation(&CreateAgentRequestInput {
        request_id: &request_id,
        agent_did,
        content,
        session_id: &session_id,
        behavior_id,
        created_at: &created_at,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
    });
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
    let query = turn_state_query(request_id);
    let response = execute_graphql(graphql_url, &query)?;
    Ok(parse_turn_state_response(&response)?)
}

pub(crate) fn fetch_graphql_session_shape(
    graphql_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<GraphqlSessionShape> {
    let turn_state = fetch_graphql_turn_state(graphql_url, request_id)?;
    let query = session_shape_query(session_id);
    let response = execute_graphql(graphql_url, &query)?;
    Ok(parse_session_shape_response(
        session_id, request_id, turn_state, &response,
    )?)
}

pub(crate) fn execute_graphql(url: &str, query: &str) -> Result<serde_json::Value> {
    execute_graphql_blocking(
        url,
        query,
        GraphqlRequestOptions {
            timeout: Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: Duration::from_millis(100),
        },
    )
}
