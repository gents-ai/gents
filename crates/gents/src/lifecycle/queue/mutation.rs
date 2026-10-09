use super::*;

use crate::lifecycle::materialize::{
    build_signed_request, ParentLink, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::lifecycle::TriggerLineage;

#[cfg(test)]
pub(super) async fn session_request_create_mutation(
    parent: &AgentRequest,
    agent_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    input: RequestInput,
    request_id: &str,
    created_at: &str,
    retry_key: Option<&str>,
) -> Result<String> {
    session_request_create_mutation_at_hop(
        parent,
        parent.request_hop,
        agent_id,
        content,
        execution_origin,
        input,
        request_id,
        created_at,
        retry_key,
    )
    .await
}

/// A control continuation of `parent` written at `hop` (Lean
/// `CausalHop.nextHop`): the session's current hop for a same-session
/// continuation, higher for a wake caused by another session.
#[allow(clippy::too_many_arguments)]
pub(super) async fn session_request_create_mutation_at_hop(
    parent: &AgentRequest,
    hop: u32,
    agent_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    input: RequestInput,
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
            &parent.node_did,
            &parent.request_id,
        );
    // A continuation stays in its parent's session under the requester that
    // owns it (Lean `Enrollment.runtimeRequesterScope`).
    let identity = RequestIdentity {
        requester_did: parent.requester_did.clone(),
        request_id: request_id.to_string(),
        node_did: parent.node_did.clone(),
        agent_id: agent_id.to_string(),
        session_id: parent.session_id.clone(),
        content: content.to_string(),
        execution_origin,
        created_at: created_at.to_string(),
    };
    let spec = RequestSpec {
        trigger_lineage: TriggerLineage {
            correlation: parent.caused_by_correlation.clone(),
            trigger_context: parent.caused_by_trigger_context.clone(),
            ..Default::default()
        },
        parent: Some(ParentLink {
            depth: hop,
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        input,
        retry_key: retry_key.map(ToOwned::to_owned),
        ..RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            identity,
            admission,
        )
    };
    let create = build_signed_request(spec, RequestSigner::RegisteredTarget).await?;
    create.graphql_mutation().map_err(anyhow::Error::msg)
}
