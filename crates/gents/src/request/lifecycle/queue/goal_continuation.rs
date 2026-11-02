use super::*;

use crate::request::lifecycle::materialize::{
    build_request, ParentLink, RequestIdentity, RequestSpec,
};
use crate::request::lifecycle::{ExecutionOrigin, TriggerLineage, WorkspaceLineage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalContinuationIdentity {
    pub(crate) request_id: String,
    pub(crate) retry_key: String,
    pub(crate) queue_key: String,
}

pub(crate) fn goal_continuation_identity(
    goal_id: &str,
    parent_request_id: &str,
    continuation_sequence: i64,
) -> Result<GoalContinuationIdentity> {
    use sha2::{Digest, Sha256};

    anyhow::ensure!(
        continuation_sequence > 0,
        "goal continuation sequence must be positive"
    );
    let digest = Sha256::digest(format!("{goal_id}\0{parent_request_id}").as_bytes());
    let digest_hex = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // The shared Goal head selector follows physical ancestry when
    // whole-second timestamps coincide; the sequence distinguishes continuation IDs.
    Ok(GoalContinuationIdentity {
        request_id: format!("goal-cont-{continuation_sequence:020}-{digest_hex}"),
        retry_key: format!("goal-continuation:{digest_hex}"),
        queue_key: format!("goal:{digest_hex}"),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "called from goal, watcher and agent tests with this shape"
)]
pub(crate) fn prepare_goal_continuation(
    parent: &AgentRequest,
    agent_id: String,
    goal_id: &str,
    content: &str,
    continuation_sequence: i64,
    wrapup: bool,
    created_at: &str,
    hop: u32,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let continuation =
        goal_continuation_identity(goal_id, &parent.request_id, continuation_sequence)?;
    let input = parent.input.continuation(
        RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::Goal,
            policy: QueuePolicy::Coalesce,
            key: Some(continuation.queue_key),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        Some(GoalContinuationInput {
            sequence: continuation_sequence,
            wrapup,
        }),
        &parent.doc_id,
    );
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            &parent.node_did,
            &parent.request_id,
        );
    let identity = RequestIdentity {
        requester_did: parent.requester_did.clone(),
        request_id: continuation.request_id,
        node_did: parent.node_did.clone(),
        agent_id,
        session_id: parent.session_id.clone(),
        content: content.to_string(),
        execution_origin: ExecutionOrigin::Scheduled,
        created_at: created_at.to_owned(),
    };
    let spec = RequestSpec {
        trigger_lineage: TriggerLineage {
            trigger_id: Some(goal_id.to_string()),
            trigger_kind: Some("goal".to_string()),
            correlation: parent.caused_by_correlation.clone(),
            trigger_context: parent.caused_by_trigger_context.clone(),
            source_doc_id: parent.caused_by_source_doc_id.clone(),
        },
        workspace: Some(WorkspaceLineage {
            workspace_id: parent.workspace_id.clone(),
            workspace_owner_node_did: parent.workspace_owner_node_did.clone(),
            workspace_authority: parent.workspace_authority.clone(),
            workspace_seal_hash: parent.workspace_seal_hash.clone(),
        }),
        parent: Some(ParentLink {
            depth: hop,
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        input,
        retry_key: Some(continuation.retry_key),
        ..RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            identity,
            admission,
        )
    };
    build_request(spec)
}

pub(crate) async fn goal_continuation_agent(
    _txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    parent: &AgentRequest,
) -> Result<String> {
    parent_agent_id(parent)
}
