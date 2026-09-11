use super::*;

/// Append an authenticated same-session steering request beneath an exact
/// committed parent. External adapters provide the user input and physical
/// parent binding; the runtime remains the sole owner of request admission,
/// signing, and atomic message persistence.
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
    enqueue_steering_request_with_message(node, &parent, content, input).await
}

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
