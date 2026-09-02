use super::*;

pub(super) async fn session_request_create_mutation(
    parent: &AgentRequest,
    behavior_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    metadata: &str,
    request_id: &str,
    created_at: &str,
    retry_key: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !parent.request_id.trim().is_empty() && !parent.doc_id.trim().is_empty(),
        "cannot enqueue runtime control continuation from an unbound parent request"
    );
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            &parent.agent_did,
            &parent.request_id,
        );
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id,
        &parent.agent_did,
        &parent.agent_did,
        behavior_id,
        &parent.session_id,
        content,
        execution_origin.as_str(),
        created_at,
        admission,
    );
    create.metadata = Some(metadata.to_string());
    create.retry_key = retry_key.map(ToOwned::to_owned);
    create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
    create.subagent_depth = parent.subagent_depth;
    create.caused_by_parent_request_id = Some(parent.request_id.clone());
    create.caused_by_parent_request_doc_id = Some(parent.doc_id.clone());
    create.caused_by_parent_tool_call_id = None;
    create.caused_by_parent_tool_call_doc_id = None;
    create.caused_by_correlation = parent.caused_by_correlation.clone();
    create.caused_by_trigger_context = parent.caused_by_trigger_context.clone();
    crate::sign_agent_request_create_as_registered_target(&mut create).await?;
    create.graphql_mutation().map_err(anyhow::Error::msg)
}
