//! `request.create`, the core operation a lifecycle plugin may request to have
//! core create a request (Lean `Hooks.Resume.perform`).

use std::sync::Arc;

use anyhow::Context;
use gents_protocol::request_admission::AgentRequestCreate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::request::hooks::effect::{CoreOp, Effect};
use crate::request::hooks::registry::{CoreOperation, OperationContext, Registry};
use crate::request::lifecycle::materialize::{sign_request, RequestSigner};
use gents_store::config_client::ConfigApplyTxn;

pub const REQUEST_CREATE: &str = "request.create";

/// The operation's arguments: the unsigned request. Core owns the session and
/// Agent document IDs, the session hop, client target binding and the
/// signature, and overwrites whatever the request carries for them; the
/// request names no signer.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestCreateArgs {
    request: AgentRequestCreate,
}

/// The effect a plugin returns to have core create `request`.
pub fn request_create(request: AgentRequestCreate) -> anyhow::Result<Effect> {
    Ok(Effect::Core(CoreOp::new(
        REQUEST_CREATE,
        serde_json::to_value(RequestCreateArgs { request })
            .context("encoding the requested request creation")?,
    )))
}

/// Creates the request under the transaction's actor, signed by core's signer
/// for that actor, only when that actor could create it itself: the actor is
/// the request's agent and its admission signer, and document ACP admits the
/// write. Then `request.create.stage` stages with the created row.
struct RequestCreateOperation;

#[async_trait::async_trait]
impl CoreOperation for RequestCreateOperation {
    fn name(&self) -> &'static str {
        REQUEST_CREATE
    }

    async fn perform(
        &self,
        txn: &ConfigApplyTxn<'_>,
        context: OperationContext<'_>,
        args: &Value,
    ) -> anyhow::Result<()> {
        let RequestCreateArgs {
            request: mut create,
        } = RequestCreateArgs::deserialize(args).context("decoding the requested request")?;
        let signer = context
            .signer
            .context("request creation requires core's signer for the transaction's actor")?;
        anyhow::ensure!(
            txn.actor_did() == Some(signer.did())
                && create.agent_did == signer.did()
                && create.admission.signer_did == signer.did(),
            "request creation requires the transaction's actor as the request's agent and signer"
        );
        let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
            txn,
            &create.node_did,
            &create.session_id,
        )
        .await?
        .context("request session is unavailable")?;
        let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
            txn,
            gents_store::collection::Collection::Agent,
            &create.node_did,
            &create.agent_did,
        )
        .await?
        .context("request Agent is unavailable")?;
        create.request_hop = crate::session::load_session_current_hop_in_txn(
            txn,
            &create.node_did,
            &create.session_id,
        )
        .await?;
        create.session_doc_id = Some(session.doc_id);
        create.agent_doc_id = Some(agent_doc_id);
        crate::request::lifecycle::bind_client_targets_in_txn(txn, &mut create).await?;
        sign_request(&mut create, RequestSigner::Identity(signer)).await?;
        let response = txn
            .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
            .await?;
        let doc_id = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
        crate::request::hooks::points::request_created(
            txn,
            context,
            &crate::request::hooks::registry::RequestCreated {
                request: create,
                doc_id,
            },
        )
        .await
    }
}

/// Core's own operations, registered by the composition root before any
/// plugin's.
pub fn register(registry: &mut Registry) {
    registry.operations.push(Arc::new(RequestCreateOperation));
}
