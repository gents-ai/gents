use super::*;

use crate::lifecycle::materialize::{
    build_request, sign_request, RequestIdentity, RequestSigner, RequestSpec, SubagentLink,
};
use crate::lifecycle::{ExecutionOrigin, TriggerLineage, WorkspaceLineage};

pub(crate) async fn enqueue_goal_continuation(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    goal_id: &str,
    content: &str,
    continuation_sequence: i64,
    wrapup: bool,
) -> Result<EnqueuedAgentRequest> {
    use sha2::{Digest, Sha256};

    let behavior_id = parent_behavior_id(node, parent).await?;
    let digest = Sha256::digest(format!("{goal_id}\0{}", parent.request_id).as_bytes());
    let digest_hex = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // Signed request timestamps are canonicalized to whole seconds, so several
    // fast continuations can legitimately share `created_at`. Keep the durable
    // controller sequence in the request ID so the request query's documented
    // `(created_at, request_id)` ordering remains causal within that second.
    let request_id = format!("goal-cont-{continuation_sequence:020}-{}", digest_hex);
    let retry_key = format!("goal-continuation:{digest_hex}");
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let queue_hints = QueueHints {
        source: QueueSource::Goal,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("goal:{digest_hex}")),
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
        request_id: request_id.clone(),
        agent_did: parent.agent_did.clone(),
        requester_did: None,
        behavior_id,
        session_id: parent.session_id.clone(),
        content: content.to_string(),
        execution_origin: ExecutionOrigin::Scheduled,
        created_at: now,
    };
    let spec = RequestSpec {
        trigger_lineage: TriggerLineage {
            trigger_id: Some(goal_id.to_string()),
            trigger_kind: Some("goal".to_string()),
            correlation: parent.caused_by_correlation.clone(),
            trigger_context: parent.caused_by_trigger_context.clone(),
            ..Default::default()
        },
        workspace: Some(WorkspaceLineage {
            workspace_id: parent.workspace_id.clone(),
            workspace_authority: parent.workspace_authority.clone(),
            workspace_owner_deployment_id: parent.workspace_owner_deployment_id.clone(),
            workspace_seal_hash: parent.workspace_seal_hash.clone(),
        }),
        subagent: Some(SubagentLink {
            depth: parent.subagent_depth,
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        metadata: Some(metadata),
        retry_key: Some(retry_key.clone()),
        ..RequestSpec::new(identity, admission)
    };
    // Peek at the unsigned DTO before paying for a signature: the dedupe
    // lookup below only needs a fingerprint of the pre-signature fields.
    let mut create = build_request(spec)?;
    let expected = crate::goal::GoalBackedRequestFingerprint::from_create(&create)?;
    if let Some(doc_id) = lookup_goal_continuation_by_retry_key(node, &retry_key, &expected).await?
    {
        return Ok(EnqueuedAgentRequest {
            doc_id,
            request_id,
            session_id: parent.session_id.clone(),
        });
    }
    sign_request(&mut create, RequestSigner::RegisteredTarget).await?;
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let response =
        session::execute_mutation_with_retry(node, &mutation, "enqueue_goal_continuation").await;
    let doc_id = match response {
        Ok(response) => match extract_single_doc_id(&response, "create_AgentRequest") {
            Some(doc_id) => Some(doc_id),
            None => lookup_goal_continuation_by_retry_key(node, &retry_key, &expected).await?,
        },
        Err(create_error) => {
            // `retry_key` is unique. A concurrent reconciler or a lost create
            // acknowledgement therefore resolves to the same durable child.
            // Only surface the original error when no such child exists.
            match lookup_goal_continuation_by_retry_key(node, &retry_key, &expected).await? {
                Some(doc_id) => Some(doc_id),
                None => return Err(create_error),
            }
        }
    }
    .context("goal continuation create returned no _docID")?;

    Ok(EnqueuedAgentRequest {
        doc_id,
        request_id,
        session_id: parent.session_id.clone(),
    })
}

async fn lookup_goal_continuation_by_retry_key(
    node: &EmbeddedNode,
    retry_key: &str,
    expected: &crate::goal::GoalBackedRequestFingerprint,
) -> Result<Option<String>> {
    let retry_key = crate::graphql::escape_graphql_string(retry_key);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{retry_key}" }} }}, limit: 2) {{
            _docID {}
        }} }}"#,
        crate::goal::GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS,
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "lookup goal continuation by retry key failed: {:?}",
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(rows.len() <= 1, "goal continuation retry key is not unique");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let persisted: crate::goal::GoalBackedRequestFingerprint = serde_json::from_value(row.clone())
        .context("decoding existing goal continuation fingerprint")?;
    anyhow::ensure!(
        persisted == *expected,
        "goal continuation retry key conflicts with different immutable lineage"
    );
    Ok(Some(
        row.get("_docID")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .context("goal continuation has no document ID")?,
    ))
}

// SAFETY (#664): `agent_did` scopes the candidate query AND the supersede
// mutation to the owning principal. Under P2P replication a foreign-DID
// `AgentRequest` sharing this `session_id` can be replicated onto this node;
// without the owner guard the session-only filter would supersede that foreign
// replica locally. Defense in depth: the foreign row never becomes a candidate,
// and the write is DID-scoped even if it somehow did.
