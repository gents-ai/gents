use super::*;

pub async fn enqueue_local_steering_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    content: &str,
    input: RequestInput,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<EnqueuedAgentRequest> {
    // The parent is the signing Agent's own request; ACP hides it from any other actor.
    let access = crate::ConfigAccess::local_with_agent_signer(
        node.clone(),
        ::identity::Did::new(signer.did())?,
        signer.clone(),
    )?;
    let parent = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &access,
        parent_request_doc_id,
    )
    .await?
    .with_context(|| format!("steering parent request {parent_request_doc_id} not found"))?;
    anyhow::ensure!(
        parent.request_id == parent_request_id,
        "steering parent request changed logical binding"
    );
    enqueue_steering_request(node, &parent, content, input, signer).await
}

pub(crate) async fn enqueue_steering_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent: &AgentRequest,
    content: &str,
    input: RequestInput,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<EnqueuedAgentRequest> {
    let queue = input
        .queue
        .as_ref()
        .context("atomic steering enqueue requires queue input")?;
    anyhow::ensure!(
        matches!(queue.source, QueueSource::Steering | QueueSource::User)
            && queue.policy == QueuePolicy::Append
            && queue.key.is_none(),
        "atomic steering enqueue requires an unkeyed user or steering append"
    );
    anyhow::ensure!(
        queue.background_completion_wake_version.is_none(),
        "steering enqueue must not carry the background wake marker"
    );
    let agent_did = parent_agent_did(parent)?;
    anyhow::ensure!(
        signer.did() == parent.agent_did,
        "steering signer differs from parent Agent"
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (agent_did, content, input, request_id, now) =
        (&agent_did, content, &input, &request_id, &now);
    let access = crate::store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(parent.agent_did.clone())?,
        signer.clone(),
    )?;
    access
        .transact_write("lifecycle.enqueue_steering", move |txn| {
            Box::pin(async move {
                // Read the session's current hop under the write gate so a
                // higher append committed first is never missed.
                let hop = crate::session::load_session_current_hop_in_txn(
                    txn,
                    &parent.node_did,
                    &parent.session_id,
                )
                .await?;
                let mutation = session_request_create_mutation_at_hop_with_signer(
                    parent,
                    hop,
                    SessionRequestFields {
                        agent_did,
                        content,
                        execution_origin: ExecutionOrigin::Interactive,
                        input: input.clone(),
                        request_id,
                        created_at: now,
                        retry_key: None,
                    },
                    Some(signer),
                )
                .await?;
                steering_transaction_attempt(txn, parent, request_id, &mutation).await
            })
        })
        .await
}
