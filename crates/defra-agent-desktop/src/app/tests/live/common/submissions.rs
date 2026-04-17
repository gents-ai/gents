use super::*;

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
                .find(|row| row.request_id == submission.request_id)?;
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

            let response = snapshot.latest_response_for_request(&submission.request_id)?;
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
                        && row.latest_request_id.as_deref() == Some(submission.request_id.as_str())
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
                        && row.enable_file_tools == Some(false)
                        && row.enable_bash == Some(false)
                        && row.cli_tool_names.is_empty()
                        && row.delegate_to.is_empty()
                });
            let profile_ok = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == deployment.docs.inference_profile_id)
                .is_some_and(|row| {
                    row.max_output_tokens == Some(1024)
                        && row.max_turns == Some(12)
                        && row.temperature == Some(0.0)
                });
            (behavior_ok && tools_ok && profile_ok).then_some(())
        },
    )
}
