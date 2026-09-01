use super::*;

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
    // Signed request timestamps are canonicalized to whole seconds, so several
    // fast continuations can legitimately share `created_at`. Keep the durable
    // controller sequence in the request ID so the request query's documented
    // `(created_at, request_id)` ordering remains causal within that second.
    let request_id = format!(
        "goal-cont-{continuation_sequence:020}-{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    if let Some(doc_id) = lookup_request_doc_id_optional(node, &request_id).await? {
        return Ok(EnqueuedAgentRequest {
            doc_id,
            request_id,
            session_id: parent.session_id.clone(),
        });
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let queue_hints = QueueHints {
        source: QueueSource::Goal,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("goal:{goal_id}:{}", parent.request_id)),
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
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id.clone(),
        &parent.agent_did,
        &parent.agent_did,
        behavior_id,
        &parent.session_id,
        content,
        "scheduled",
        now,
        admission,
    );
    create.metadata = Some(metadata);
    create.caused_by_trigger_id = Some(goal_id.to_string());
    create.caused_by_trigger_kind = Some("goal".to_string());
    create.caused_by_correlation = parent.caused_by_correlation.clone();
    create.caused_by_trigger_context = parent.caused_by_trigger_context.clone();
    create.caused_by_parent_request_id = Some(parent.request_id.clone());
    create.caused_by_parent_request_doc_id = Some(parent.doc_id.clone());
    create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
    crate::sign_agent_request_create_as_registered_target(&mut create).await?;
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let response =
        session::execute_mutation_with_retry(node, &mutation, "enqueue_goal_continuation").await?;
    let doc_id = extract_single_doc_id(&response, "create_AgentRequest")
        .or(lookup_request_doc_id_optional(node, &request_id).await?)
        .context("goal continuation create returned no _docID")?;

    Ok(EnqueuedAgentRequest {
        doc_id,
        request_id,
        session_id: parent.session_id.clone(),
    })
}

// SAFETY (#664): `agent_did` scopes the candidate query AND the supersede
// mutation to the owning principal. Under P2P replication a foreign-DID
// `AgentRequest` sharing this `session_id` can be replicated onto this node;
// without the owner guard the session-only filter would supersede that foreign
// replica locally. Defense in depth: the foreign row never becomes a candidate,
// and the write is DID-scoped even if it somehow did.
