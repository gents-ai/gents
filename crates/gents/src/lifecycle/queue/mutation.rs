use super::*;

use crate::lifecycle::materialize::{
    build_signed_request, ParentLink, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::lifecycle::{TriggerLineage, WorkspaceLineage};

pub(super) struct SessionRequestFields<'a> {
    pub(super) agent_id: &'a str,
    pub(super) content: &'a str,
    pub(super) execution_origin: ExecutionOrigin,
    pub(super) input: RequestInput,
    pub(super) request_id: &'a str,
    pub(super) created_at: &'a str,
    pub(super) retry_key: Option<&'a str>,
}

#[cfg(test)]
pub(super) async fn session_request_create_mutation(
    parent: &AgentRequest,
    fields: SessionRequestFields<'_>,
) -> Result<String> {
    session_request_create_mutation_at_hop(parent, parent.request_hop, fields).await
}

pub(super) async fn session_request_create_mutation_at_hop(
    parent: &AgentRequest,
    hop: u32,
    fields: SessionRequestFields<'_>,
) -> Result<String> {
    let SessionRequestFields {
        agent_id,
        content,
        execution_origin,
        input,
        request_id,
        created_at,
        retry_key,
    } = fields;
    anyhow::ensure!(
        !parent.request_id.trim().is_empty() && !parent.doc_id.trim().is_empty(),
        "cannot enqueue runtime control continuation from an unbound parent request"
    );
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            &parent.node_did,
            &parent.request_id,
        );
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
        workspace: Some(WorkspaceLineage {
            workspace_id: parent.workspace_id.clone(),
            workspace_owner_node_did: parent.workspace_owner_node_did.clone(),
            workspace_authority: parent.workspace_authority.clone(),
            workspace_seal_hash: parent.workspace_seal_hash.clone(),
        }),
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
