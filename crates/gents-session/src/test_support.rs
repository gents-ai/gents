pub async fn publish_provider_message(
    writer: &crate::request::streaming::DefraStreamWriter,
    request: &crate::request::lifecycle::RequestLifecycle,
    turn: usize,
    message: &gents_protocol::message::Message,
) -> anyhow::Result<String> {
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse()?,
        )
        .await;
    Ok(writer
        .publish_native_turn(request, turn, 0, message)
        .await?
        .message_doc_id)
}

pub async fn admitted_client_lifecycle(
    node: std::sync::Arc<defra_node::Node>,
    runtime: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    request: &gents_protocol::row::AgentRequest,
    agent: &dyn gents_store::identity::NodeIdentity,
) -> anyhow::Result<crate::request::lifecycle::RequestLifecycle> {
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        runtime,
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(request, agent)
        .await?;
    let mut lifecycle =
        crate::request::lifecycle::RequestLifecycle::new_with_node_did(node, request, 60);
    lifecycle.bind_managed_agent(agent)?;
    lifecycle.set_client_claim_admission(admission);
    Ok(lifecycle)
}

/// Create an Agent-owned session as that Agent and publish its Agent
/// relationships in the same transaction, so requests and Goals may bind it.
pub async fn create_agent_owned_session(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> String {
    let agent_did = access
        .actor_did()
        .expect("an Agent-owned session needs its Agent actor");
    access
        .transact_write("test.agent_owned_session", |txn| {
            let agent_did = &agent_did;
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    session_id,
                    node_did,
                    agent_did,
                    requester_did,
                    None,
                    None,
                    "2020-01-01T00:00:00Z",
                )
                .await?;
                let session =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("created session is not visible"))?;
                let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("session Agent is not visible"))?;
                crate::session::stage_created_session_relationships_in_txn(
                    txn,
                    &session.doc_id,
                    &agent_doc,
                    &identity::Did::new(agent_did)?,
                )
                .await?;
                Ok::<_, anyhow::Error>(session.doc_id)
            })
        })
        .await
        .unwrap()
}
