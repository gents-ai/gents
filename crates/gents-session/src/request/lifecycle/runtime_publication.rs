use anyhow::{ensure, Context};
use gents_protocol::request_admission::{AgentRequestAdmissionKind, AgentRequestCreate};
use gents_store::config_client::ConfigApplyTxn;

use super::client::{stage_admitted_request_in_txn, stage_runtime_request_in_txn, StageOptions};
use super::materialize::{sign_request, RequestSigner};

#[derive(Debug, thiserror::Error)]
#[error("runtime request publication failed: {0}")]
pub struct RuntimePublicationError(#[source] anyhow::Error);

pub async fn publish(
    txn: &ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
) -> Result<String, RuntimePublicationError> {
    stage(txn, create).await.map_err(RuntimePublicationError)
}

async fn stage(txn: &ConfigApplyTxn<'_>, create: &AgentRequestCreate) -> anyhow::Result<String> {
    let signer = txn
        .managed_agent_signer()
        .context("runtime publication requires its managed Agent signer")?;
    ensure!(
        txn.actor_did() == Some(signer.did())
            && create.agent_did == signer.did()
            && create.admission.signer_did == signer.did()
            && matches!(
                create.admission.kind,
                AgentRequestAdmissionKind::RuntimeInternal | AgentRequestAdmissionKind::LocalSelf
            ),
        "runtime publication requires its acting Agent as author"
    );
    let existing = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
    )
    .await?;
    if let Some(session) = &existing {
        ensure!(
            session.session.agent_did == create.agent_did,
            "runtime publication cannot change the session's selected Agent"
        );
        ensure!(
            create.purpose.is_auxiliary()
                || session.session.requester_did.as_deref() == Some(create.requester_did.as_str()),
            "runtime publication cannot change session requester provenance"
        );
    } else {
        ensure!(
            !create.purpose.is_auxiliary(),
            "auxiliary request requires its parent's session"
        );
    }
    let reserved = if existing.is_none() {
        crate::session::ensure_reserved_session_in_txn(
            txn,
            crate::session::NewAgentSession {
                session_id: &create.session_id,
                node_did: &create.node_did,
                agent_did: &create.agent_did,
                requester_did: Some(&create.requester_did),
                title: create.input.initial_title.clone(),
                provenance: None,
                created_at: &create.created_at,
            },
        )
        .await?
    } else {
        false
    };
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
    )
    .await?
    .context("runtime publication session is unavailable")?;
    let (agent, _) = gents_documents::read_desired_state_record_in_txn(
        txn,
        gents_store::collection::Collection::Agent,
        &create.node_did,
        &create.agent_did,
    )
    .await?
    .context("runtime publication selected Agent is unavailable")?;
    if reserved {
        crate::session::stage_session_relationships_in_txn(
            txn,
            &session.doc_id,
            &agent,
            &identity::Did::new(&create.agent_did)?,
        )
        .await?;
    }
    let mut create = create.clone();
    create.session_doc_id = Some(session.doc_id.clone());
    create.agent_doc_id = Some(agent.clone());
    sign_request(&mut create, RequestSigner::Identity(&signer)).await?;
    if create.purpose.is_auxiliary() {
        return stage_runtime_request_in_txn(txn, &create, &session.doc_id, &agent).await;
    }
    stage_admitted_request_in_txn(
        txn,
        &create,
        &session.doc_id,
        &agent,
        StageOptions {
            published_doc_id: None,
            signer: None,
            reserved_runtime_session: reserved,
        },
    )
    .await
}
