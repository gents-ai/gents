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
    let spec = crate::lifecycle::materialize::RequestSpec {
        identity: crate::lifecycle::materialize::RequestIdentity {
            request_id: request_id.to_string(),
            agent_did: parent.agent_did.clone(),
            requester_did: None,
            behavior_id: behavior_id.to_string(),
            session_id: parent.session_id.clone(),
            content: content.to_string(),
            execution_origin,
            created_at: created_at.to_string(),
        },
        admission,
        initial_lifecycle_state: gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
        trigger_lineage: crate::lifecycle::TriggerLineage {
            trigger_id: None,
            trigger_kind: None,
            source_doc_id: None,
            correlation: parent.caused_by_correlation.clone(),
            trigger_context: parent.caused_by_trigger_context.clone(),
        },
        trigger_doc_id: None,
        workspace: None,
        subagent: Some(crate::lifecycle::materialize::SubagentLink {
            depth: parent.subagent_depth,
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            parent_tool_call_id: None,
            parent_tool_call_doc_id: None,
        }),
        retry: None,
        sampling: None,
        metadata: Some(metadata.to_string()),
        retry_key: retry_key.map(ToOwned::to_owned),
        valid_until: None,
    };
    let create = crate::lifecycle::materialize::build_signed_request(
        spec,
        crate::lifecycle::materialize::RequestSigner::RegisteredTarget,
    )
    .await?;
    create.graphql_mutation().map_err(anyhow::Error::msg)
}
