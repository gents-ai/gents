use super::*;

pub async fn enqueue_local_steering_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    content: &str,
    input: RequestInput,
) -> Result<EnqueuedAgentRequest> {
    let parent = crate::request_binding::load_agent_request_by_doc_id(node, parent_request_doc_id)
        .await?
        .ok_or_else(|| QueueError::SteeringParentRequestNotFound {
            parent_request_doc_id: parent_request_doc_id.to_string(),
        })?;
    if parent.request_id != parent_request_id {
        return Err(QueueError::SteeringParentRequestChangedLogicalBinding);
    }
    enqueue_steering_request(node, &parent, content, input).await
}

pub(crate) async fn enqueue_steering_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent: &AgentRequest,
    content: &str,
    input: RequestInput,
) -> Result<EnqueuedAgentRequest> {
    let queue = input
        .queue
        .as_ref()
        .ok_or(QueueError::AtomicSteeringEnqueueRequiresQueueInput)?;
    if !(matches!(queue.source, QueueSource::Steering | QueueSource::User)
        && queue.policy == QueuePolicy::Append
        && queue.key.is_none())
    {
        return Err(QueueError::AtomicSteeringEnqueueRequiresUnkeyedUser);
    }
    if !queue.background_completion_wake_version.is_none() {
        return Err(QueueError::SteeringEnqueueMustNotCarryBackground);
    }
    let agent_id = parent_agent_id(parent)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (agent_id, content, input, request_id, now) =
        (&agent_id, content, &input, &request_id, &now);
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "lifecycle.enqueue_steering",
        move |txn| {
            Box::pin(async move {
                // Read the session's current hop under the write gate so a
                // higher append committed first is never missed.
                let hop = crate::session::load_session_current_hop_in_txn(
                    txn,
                    &parent.node_did,
                    &parent.session_id,
                )
                .await?;
                let mutation = session_request_create_mutation_at_hop(
                    parent,
                    hop,
                    SessionRequestFields {
                        agent_id,
                        content,
                        execution_origin: ExecutionOrigin::Interactive,
                        input: input.clone(),
                        request_id,
                        created_at: now,
                        retry_key: None,
                    },
                )
                .await?;
                steering_transaction_attempt(txn, parent, request_id, &mutation).await
            })
        },
    )
    .await
}
