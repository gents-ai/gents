use super::*;

/// Append an authenticated same-session steering request beneath an exact
/// committed parent. External adapters provide the user input and physical
/// parent binding; the runtime remains the sole owner of request admission and
/// signing. Transcript publication belongs to the owned execution boundary.
pub async fn enqueue_local_steering_request(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    content: &str,
    input: RequestInput,
) -> Result<EnqueuedAgentRequest> {
    let parent = crate::request_binding::load_agent_request_by_doc_id(node, parent_request_doc_id)
        .await?
        .with_context(|| format!("steering parent request {parent_request_doc_id} not found"))?;
    anyhow::ensure!(
        parent.request_id == parent_request_id,
        "steering parent request changed logical binding"
    );
    enqueue_steering_request(node, &parent, content, input).await
}

/// Atomically persist the signed steering request. Its admission content is
/// displayed while queued and is published to the transcript only when owned
/// execution starts.
pub(crate) async fn enqueue_steering_request(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    content: &str,
    input: RequestInput,
) -> Result<EnqueuedAgentRequest> {
    let queue = input
        .queue
        .as_ref()
        .context("atomic steering enqueue requires queue input")?;
    anyhow::ensure!(
        queue.source == QueueSource::Steering
            && queue.policy == QueuePolicy::Append
            && queue.key.is_none(),
        "atomic steering enqueue requires an unkeyed append"
    );
    anyhow::ensure!(
        queue.background_completion_wake_version.is_none(),
        "steering enqueue must not carry the background wake marker"
    );

    let behavior_id = parent_behavior_id(parent)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let request_mutation = session_request_create_mutation(
        parent,
        &behavior_id,
        content,
        ExecutionOrigin::Interactive,
        input,
        &request_id,
        &now,
        None,
    )
    .await?;
    let request_id = &request_id;
    let request_mutation = &request_mutation;

    let enqueued = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.enqueue_steering",
        move |txn| {
            Box::pin(async move {
                steering_transaction_attempt(txn, parent, request_id, request_mutation).await
            })
        },
    )
    .await?;

    Ok(enqueued)
}
