use super::*;

/// Atomically persist a steering input and the continuation that consumes it.
///
/// The request is created first inside the private transaction so its exact
/// DefraDB document ID can be stamped on the message.  Neither document is
/// externally visible until both writes commit, so a watcher can never claim
/// the continuation before its input is durable.
pub(crate) async fn enqueue_steering_request_with_message(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    content: &str,
    queue_hints: QueueHints,
) -> Result<EnqueuedAgentRequest> {
    anyhow::ensure!(
        queue_hints.source == QueueSource::Steering
            && queue_hints.policy == QueuePolicy::Append
            && queue_hints.key.is_none(),
        "atomic steering enqueue requires an unkeyed append"
    );

    let behavior_id = parent_behavior_id(node, parent).await?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let metadata = queue_metadata_json(&queue_hints);
    let request_mutation = session_request_create_mutation(
        parent,
        &behavior_id,
        content,
        ExecutionOrigin::Interactive,
        &metadata,
        &request_id,
        &now,
        None,
    )
    .await?;
    // Match the hook's canonical persisted representation so its
    // request-scoped prompt dedup reuses this keyed row instead of appending a
    // second copy when the continuation starts.
    let persisted_content = serde_json::to_string(&crate::llm::message::Message::user(content))?;
    let persisted_content = &persisted_content;
    let request_id = &request_id;
    let request_mutation = &request_mutation;

    let enqueued = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.enqueue_steering",
        move |txn| {
            Box::pin(async move {
                steering_transaction_attempt(
                    txn,
                    parent,
                    persisted_content,
                    request_id,
                    request_mutation,
                )
                .await
            })
        },
    )
    .await?;

    Ok(enqueued)
}
