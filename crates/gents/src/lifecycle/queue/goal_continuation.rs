use super::*;

use crate::lifecycle::materialize::{build_request, ParentLink, RequestIdentity, RequestSpec};
use crate::lifecycle::{ExecutionOrigin, TriggerLineage, WorkspaceLineage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalContinuationIdentity {
    pub(crate) request_id: String,
    pub(crate) retry_key: String,
    pub(crate) queue_key: String,
}

/// Preserve the existing goal/parent retry identity and stable sequence IDs.
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
    // The sequence distinguishes continuation IDs. The shared Goal head selector
    // follows physical ancestry when whole-second timestamps coincide.
    Ok(GoalContinuationIdentity {
        request_id: format!("goal-cont-{continuation_sequence:020}-{digest_hex}"),
        retry_key: format!("goal-continuation:{digest_hex}"),
        queue_key: format!("goal:{digest_hex}"),
    })
}

/// Prepare the existing continuation DTO without reads, signing, or publication.
/// Transaction owners resolve behavior and time before staging this request.
pub(crate) fn prepare_goal_continuation(
    parent: &AgentRequest,
    behavior_id: String,
    goal_id: &str,
    content: &str,
    continuation_sequence: i64,
    wrapup: bool,
    created_at: &str,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let continuation =
        goal_continuation_identity(goal_id, &parent.request_id, continuation_sequence)?;
    let queue_hints = QueueHints {
        source: QueueSource::Goal,
        policy: QueuePolicy::Coalesce,
        key: Some(continuation.queue_key),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };
    let metadata = serde_json::json!({
        "queue": queue_hints,
        "goal": {
            "goal_id": goal_id,
            "parent_request_id": parent.request_id,
            "continuation_sequence": continuation_sequence,
            "wrapup": wrapup,
        }
    })
    .to_string();
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            &parent.agent_did,
            &parent.request_id,
        );
    let identity = RequestIdentity {
        requester_did: None,
        request_id: continuation.request_id,
        agent_did: parent.agent_did.clone(),
        behavior_id,
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
            ..Default::default()
        },
        workspace: Some(WorkspaceLineage {
            workspace_id: parent.workspace_id.clone(),
            workspace_authority: parent.workspace_authority.clone(),
            workspace_owner_deployment_id: parent.workspace_owner_deployment_id.clone(),
            workspace_seal_hash: parent.workspace_seal_hash.clone(),
        }),
        subagent: Some(ParentLink {
            depth: parent.subagent_depth,
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        metadata: Some(metadata),
        retry_key: Some(continuation.retry_key),
        ..RequestSpec::new(identity, admission)
    };
    build_request(spec)
}

/// Resolve the historical conversation fallback through the caller's transaction.
/// This helper prepares request binding only; Goal owns publication.
pub(crate) async fn goal_continuation_behavior(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    parent: &AgentRequest,
) -> Result<String> {
    if let Some(behavior) = parent
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(behavior.to_owned());
    }
    let agent_did = escape_graphql_string(&parent.agent_did);
    let session_id = escape_graphql_string(&parent.session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentConversation(filter: {{
        agent_did: {{ _eq: "{agent_did}" }}, session_id: {{ _eq: "{session_id}" }}
    }}, limit: 2) {{ behavior_id }} }}"#
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentConversation")
        .and_then(serde_json::Value::as_array)
        .context("parent conversation query omitted rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "parent conversation scope resolved to multiple rows"
    );
    rows.first()
        .and_then(|row| row.get("behavior_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| {
            format!(
                "cannot enqueue same-session request: parent request {} has no behavior_id",
                parent.request_id
            )
        })
}
