use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

pub mod send;

#[derive(Debug, thiserror::Error)]
pub enum ClientControlError {
    #[error("the signed control intent is not authorized")]
    NotAuthorized,
    #[error("the authenticated client cannot read this invocation")]
    NotFound,
    #[error("the requested invocation belongs to another runtime")]
    WrongNode,
    #[error("the control intent could not be saved or observed")]
    Unavailable(#[source] anyhow::Error),
    #[error("control intent {intent_doc_id} was saved but its agent grant is unconfirmed")]
    Unconfirmed {
        intent_doc_id: String,
        #[source]
        source: anyhow::Error,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct Invocation {
    #[serde(rename = "_docID")]
    doc_id: String,
    node_did: String,
    agent_did: String,
    session_id: String,
    session_doc_id: String,
    request_doc_id: String,
    requester_did: Option<String>,
    tool_call_id: String,
}

/// Resolve the physical invocation as the client and sign the exact scope
/// before sending it to the runtime.
pub async fn prepare(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    node_did: &str,
    tool_call_doc_id: &str,
    expected: Option<(&str, &str)>,
    action: &str,
) -> Result<gents_protocol::process_control::ToolControlIntent, ClientControlError> {
    if access.actor_did().as_deref() != Some(signer.did()) {
        return Err(ClientControlError::NotAuthorized);
    }
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID node_did agent_did session_id session_doc_id request_doc_id requester_did tool_call_id }} }}"#,
        escape_graphql_string(tool_call_doc_id)
    );
    let response = access
        .execute(&query)
        .await
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    let rows: Vec<Invocation> = serde_json::from_value(response["data"]["AgentToolCall"].clone())
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    let [row] = rows.as_slice() else {
        return Err(ClientControlError::NotFound);
    };
    if row.node_did != node_did
        || expected
            .is_some_and(|(session, call)| row.session_id != session || row.tool_call_id != call)
    {
        return Err(ClientControlError::NotFound);
    }
    let mut intent = gents_protocol::process_control::ToolControlIntent {
        version: 1,
        intent_id: uuid::Uuid::new_v4().to_string(),
        client_did: signer.did().into(),
        node_did: row.node_did.clone(),
        agent_did: row.agent_did.clone(),
        session_id: row.session_id.clone(),
        session_doc_id: row.session_doc_id.clone(),
        request_doc_id: row.request_doc_id.clone(),
        requester_did: row.requester_did.clone(),
        tool_call_id: row.tool_call_id.clone(),
        tool_call_doc_id: row.doc_id.clone(),
        action: action.into(),
        signature: Vec::new(),
    };
    intent.signature = signer
        .sign(
            &intent
                .signing_payload()
                .map_err(|error| ClientControlError::Unavailable(error.into()))?,
        )
        .await
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    intent
        .validate_shape()
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    Ok(intent)
}

fn invocation_matches(
    row: &Invocation,
    intent: &gents_protocol::process_control::ToolControlIntent,
) -> bool {
    row.doc_id == intent.tool_call_doc_id
        && row.node_did == intent.node_did
        && row.agent_did == intent.agent_did
        && row.session_id == intent.session_id
        && row.session_doc_id == intent.session_doc_id
        && row.request_doc_id == intent.request_doc_id
        && row.requester_did == intent.requester_did
        && row.tool_call_id == intent.tool_call_id
}

fn signature_matches(
    intent: &gents_protocol::process_control::ToolControlIntent,
    actor: &str,
) -> bool {
    intent.validate_shape().is_ok()
        && actor == intent.client_did
        && intent.signing_payload().ok().is_some_and(|payload| {
            crate::store::identity::verify_did_signature(actor, &payload, &intent.signature)
                .unwrap_or(false)
        })
}

fn signed_invocation_authorized(
    intent: &gents_protocol::process_control::ToolControlIntent,
    actor: &str,
    action: &str,
    row: &Invocation,
) -> bool {
    signature_matches(intent, actor) && intent.action == action && invocation_matches(row, intent)
}

/// The client publishes its signed intent through its own client-signed
/// endpoint as the User (`Background.ProcessControl.ClientIntent`); the
/// runtime holds no client key and never writes an intent on a client's
/// behalf. The signed scope is checked against the client's own native ACP
/// read in the transaction that creates the intent.
pub async fn submit(
    access: &ConfigAccess,
    intent: &gents_protocol::process_control::ToolControlIntent,
) -> Result<String, ClientControlError> {
    let Some(actor) = access.actor_did() else {
        return Err(ClientControlError::NotAuthorized);
    };
    if !signature_matches(intent, &actor) {
        return Err(ClientControlError::NotAuthorized);
    }
    let actor = identity::Did::new(&actor)
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    let signed = serde_json::to_string(intent)
        .map_err(|error| ClientControlError::Unavailable(error.into()))?;
    let (document, created) = access.transact_write("tool_control.submit", |txn| {
        let signed = &signed;
        let actor = &actor;
        Box::pin(async move {
            crate::store::graphql::fence_arrivals(txn, "AgentToolControl").await?;
            let prior = txn.execute(&format!(r#"{{ AgentToolControl(filter: {{ intent_id: {{ _eq: "{}" }}, client_did: {{ _eq: "{}" }} }}) {{ _docID intent_json }} }}"#, escape_graphql_string(&intent.intent_id), escape_graphql_string(actor.as_str()))).await?;
            let rows = prior["data"]["AgentToolControl"].as_array().context("control identity query omitted rows")?;
            for row in rows {
                let id = row["_docID"].as_str().context("control intent lacks document ID")?;
                let owners = txn.document_owners("AgentToolControl", &[id.to_owned()]).await?;
                if owners.len() == 1 && owners[0].owner.as_ref().map(identity::Did::as_str) == Some(actor.as_str()) {
                    anyhow::ensure!(row["intent_json"].as_str() == Some(signed.as_str()), "control intent identity is bound to different input");
                    return Ok(Some((id.to_owned(), false)));
                }
            }
            let query = format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID node_did agent_did session_id session_doc_id request_doc_id requester_did tool_call_id }} }}"#, escape_graphql_string(&intent.tool_call_doc_id));
            let result = txn.execute(&query).await?;
            let rows: Vec<Invocation> = serde_json::from_value(result["data"]["AgentToolCall"].clone())?;
            let [row] = rows.as_slice() else { return Ok(None); };
            if !signed_invocation_authorized(intent, actor.as_str(), &intent.action, row) { return Ok(None); }
            let created = txn.execute_client_signed_create(
                "mutation($input: AgentToolControlMutationInputArg!) { create_AgentToolControl(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "intent_id": intent.intent_id, "intent_json": signed, "client_did": intent.client_did,
                    "agent_did": intent.agent_did, "node_did": intent.node_did, "session_id": intent.session_id,
                    "session_doc_id": intent.session_doc_id, "requester_did": intent.requester_did,
                    "request_doc_id": intent.request_doc_id, "tool_call_id": intent.tool_call_id,
                    "tool_call_doc_id": intent.tool_call_doc_id, "action": intent.action, "state": "pending"
                }}),
                gents_protocol::schemas::AGENT_TOOL_CONTROL,
            ).await?;
            let doc_id = crate::store::graphql::created_doc_id(&created, "AgentToolControl")?;
            Ok::<_, anyhow::Error>(Some((doc_id, true)))
        })
    }).await.map_err(ClientControlError::Unavailable)?.ok_or(ClientControlError::NotFound)?;
    if !created {
        return Err(ClientControlError::Unconfirmed {
            intent_doc_id: document,
            source: anyhow::anyhow!("an existing signed intent requires observation"),
        });
    }
    access
        .add_actor_relationship(
            "AgentToolControl",
            &document,
            "working_agent",
            &identity::Did::new(&intent.agent_did)
                .map_err(|error| ClientControlError::Unavailable(error.into()))?,
        )
        .await
        .map_err(|error| ClientControlError::Unconfirmed {
            intent_doc_id: document.clone(),
            source: error.into(),
        })?;
    Ok(document)
}

#[derive(Debug, Deserialize)]
pub(crate) struct Intent {
    #[serde(rename = "_docID")]
    doc_id: String,
    intent_id: String,
    intent_json: String,
    client_did: String,
    agent_did: String,
    node_did: String,
    session_id: String,
    session_doc_id: String,
    request_doc_id: String,
    requester_did: Option<String>,
    tool_call_id: String,
    tool_call_doc_id: String,
    action: String,
    state: String,
    #[serde(skip)]
    claim_doc_id: String,
    #[serde(skip)]
    existing_receipt: Option<String>,
}

async fn signed_receipt(
    intent: &Intent,
    outcome: gents_protocol::process_control::ToolControlReceiptOutcome,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<String> {
    anyhow::ensure!(
        signer.did() == intent.agent_did,
        "tool control receipt signer differs from selected Agent"
    );
    use gents_protocol::process_control::ToolControlReceipt;
    let mut receipt = ToolControlReceipt {
        version: 1,
        intent_doc_id: intent.doc_id.clone(),
        client_did: intent.client_did.clone(),
        agent_did: intent.agent_did.clone(),
        node_did: intent.node_did.clone(),
        session_id: intent.session_id.clone(),
        session_doc_id: intent.session_doc_id.clone(),
        request_doc_id: intent.request_doc_id.clone(),
        tool_call_id: intent.tool_call_id.clone(),
        tool_call_doc_id: intent.tool_call_doc_id.clone(),
        action: intent.action.clone(),
        outcome,
        signature: Vec::new(),
    };
    receipt.signature = signer.sign(&receipt.signing_payload()?).await?;
    Ok(serde_json::to_string(&receipt)?)
}

async fn client_authority(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    intent: &Intent,
    agent_doc: &str,
) -> Result<Option<bool>> {
    let subject = identity::Did::new(&intent.client_did)?;
    let named = |permission: &str, node_permission| defra_node::DocumentAccessMode::Named {
        permission: permission.into(),
        node_permission,
    };
    let read = txn
        .observe_document_access(
            &subject,
            "AgentToolCall",
            std::slice::from_ref(&intent.tool_call_doc_id),
            named("read", acp::nac::NodePermission::DocumentRead),
        )
        .await?;
    let update = txn
        .observe_document_access(
            &subject,
            "AgentSession",
            std::slice::from_ref(&intent.session_doc_id),
            named("update", acp::nac::NodePermission::DocumentUpdate),
        )
        .await?;
    let invoke = txn
        .observe_document_access(
            &subject,
            "Agent",
            &[agent_doc.to_owned()],
            named("invoke", acp::nac::NodePermission::DocumentUpdate),
        )
        .await?;
    use defra_node::DocumentAccessDecision::{Allowed, Denied};
    let decisions = [read.as_slice(), update.as_slice(), invoke.as_slice()];
    if decisions
        .iter()
        .any(|decision| matches!(decision, [Denied]))
    {
        return Ok(Some(false));
    }
    if decisions
        .iter()
        .all(|decision| matches!(decision, [Allowed]))
    {
        return Ok(Some(true));
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) async fn claim(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    agent: &identity::Did,
    doc_id: &str,
) -> Result<Option<Intent>> {
    claim_inner(
        node,
        access,
        agent,
        doc_id,
        true,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) struct ClaimPause {
    pub observed: tokio::sync::Notify,
    pub resume: tokio::sync::Notify,
    pub armed: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
pub(crate) async fn claim_paused(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    agent: &identity::Did,
    doc_id: &str,
    pause: &ClaimPause,
) -> Result<Option<Intent>> {
    claim_inner(node, access, agent, doc_id, true, Some(pause)).await
}

#[cfg(test)]
pub(crate) async fn claim_unavailable(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    agent: &identity::Did,
    doc_id: &str,
) -> Result<Option<Intent>> {
    claim_inner(node, access, agent, doc_id, false, None).await
}

async fn claim_inner(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    agent: &identity::Did,
    doc_id: &str,
    allow_new: bool,
    #[cfg(test)] pause: Option<&ClaimPause>,
) -> Result<Option<Intent>> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent.as_str()),
        "tool control claim access differs from its selected agent"
    );
    access.transact_write("tool_control.claim", |txn| {
        Box::pin(async move {
            crate::store::graphql::fence_arrivals(txn, "AgentToolControlClaim").await?;
            let query = format!(r#"{{ AgentToolControl(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID intent_id intent_json client_did agent_did node_did session_id session_doc_id request_doc_id requester_did tool_call_id tool_call_doc_id action state }} AgentToolControlClaim(filter: {{ intent_doc_id: {{ _eq: "{}" }} }}) {{ _docID agent_did intent_json receipt_json }} }}"#, escape_graphql_string(doc_id), escape_graphql_string(doc_id));
            let response = txn.execute(&query).await?;
            let intents: Vec<Intent> = serde_json::from_value(response["data"]["AgentToolControl"].clone())?;
            let [intent] = intents.as_slice() else { return Ok(None); };
            if intent.agent_did != agent.as_str() || !matches!(intent.state.as_str(), "pending" | "claimed") {
                return Ok(None);
            }
            let signed: gents_protocol::process_control::ToolControlIntent = match serde_json::from_str(&intent.intent_json) {
                Ok(signed) => signed,
                Err(_) => return Ok(None),
            };
            if !signature_matches(&signed, &intent.client_did) || signed.intent_id != intent.intent_id
                || signed.client_did != intent.client_did || signed.agent_did != intent.agent_did
                || signed.node_did != intent.node_did || signed.session_id != intent.session_id
                || signed.session_doc_id != intent.session_doc_id || signed.request_doc_id != intent.request_doc_id
                || signed.requester_did != intent.requester_did || signed.tool_call_id != intent.tool_call_id
                || signed.tool_call_doc_id != intent.tool_call_doc_id || signed.action != intent.action {
                return Ok(None);
            }
            let owners = txn.document_owners("AgentToolControl", std::slice::from_ref(&intent.doc_id)).await?;
            if owners.len() != 1 || owners[0].owner.as_ref().map(identity::Did::as_str) != Some(intent.client_did.as_str()) {
                return Ok(None);
            }
            let target = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID node_did agent_did session_id session_doc_id request_doc_id requester_did tool_call_id }} }}"#, escape_graphql_string(&intent.tool_call_doc_id))).await?;
            let rows: Vec<Invocation> = serde_json::from_value(target["data"]["AgentToolCall"].clone())?;
            let [row] = rows.as_slice() else { return Ok(None); };
            if !signed_invocation_authorized(&signed, &intent.client_did, &intent.action, row) {
                return Ok(None);
            }
            let claims = response["data"]["AgentToolControlClaim"].as_array().context("control claim query omitted rows")?;
            let mut own_claim = None;
            for claim in claims {
                if claim["agent_did"].as_str() != Some(agent.as_str()) { continue; }
                let claim_doc = claim["_docID"].as_str().context("control claim lacks document ID")?;
                let owners = txn.document_owners("AgentToolControlClaim", &[claim_doc.to_owned()]).await?;
                if owners.len() == 1 && owners[0].owner.as_ref().map(identity::Did::as_str) == Some(agent.as_str()) {
                    anyhow::ensure!(claim["intent_json"].as_str() == Some(intent.intent_json.as_str()), "claim retained a different signed intent");
                    anyhow::ensure!(own_claim.replace(claim).is_none(), "selected agent has ambiguous control claims");
                }
            }
            let (claim_doc, existing_receipt) = if let Some(claim) = own_claim {
                let claim_doc = claim["_docID"].as_str().context("control claim lacks document ID")?;
                (claim_doc.to_owned(), claim["receipt_json"].as_str().map(str::to_owned))
            } else {
                let Some(session) = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, &intent.node_did, &intent.session_id).await? else { return Ok(None); };
                if session.doc_id != intent.session_doc_id || session.session.agent_did != intent.agent_did { return Ok(None); }
                let Some((agent_doc, agent_row)) = crate::store::config_client::read_desired_state_record_in_txn(txn, crate::Collection::Agent, &intent.node_did, &intent.agent_did).await? else { return Ok(None); };
                let Some(lifecycle) = crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id_in_txn(txn, node.clone(), &intent.tool_call_doc_id, &intent.node_did, &intent.session_id, intent.requester_did.as_deref()).await? else { return Ok(None); };
                if lifecycle.actor() != agent || lifecycle.tool_call_id() != intent.tool_call_id { return Ok(None); }
                let authority = client_authority(txn, intent, &agent_doc).await?;
                if authority.is_some() && (authority == Some(false) || !allow_new || agent_row["enabled"].as_bool() == Some(false)) {
                    let signer = txn.managed_agent_signer().context("tool control requires a managed Agent signer")?;
                    let receipt = signed_receipt(intent, gents_protocol::process_control::ToolControlReceiptOutcome::Rejected, &signer).await?;
                    txn.execute_with_variables(
                        "mutation($input: AgentToolControlClaimMutationInputArg!) { create_AgentToolControlClaim(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"intent_doc_id": intent.doc_id, "intent_json": intent.intent_json, "agent_did": agent.as_str(), "receipt_json": receipt}}),
                    ).await?;
                    txn.execute_with_variables(
                        &format!(r#"mutation($input: AgentToolControlMutationInputArg!) {{ update_AgentToolControl(docID: "{}", input: $input) {{ _docID }} }}"#, escape_graphql_string(doc_id)),
                        &serde_json::json!({"input": {"state": "denied", "outcome_json": receipt}}),
                    ).await?;
                    return Ok(None);
                }
                if authority != Some(true) { return Ok(None); }
                #[cfg(test)]
                if let Some(pause) = pause {
                    if pause.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        pause.observed.notify_one();
                        pause.resume.notified().await;
                    }
                }
                let created = txn.execute_with_variables(
                    "mutation($input: AgentToolControlClaimMutationInputArg!) { create_AgentToolControlClaim(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"intent_doc_id": intent.doc_id, "intent_json": intent.intent_json, "agent_did": agent.as_str()}}),
                ).await?;
                txn.execute(&format!(r#"mutation {{ update_AgentToolControl(docID: "{}", input: {{ state: "claimed" }}) {{ _docID }} }}"#, escape_graphql_string(doc_id))).await?;
                (crate::store::graphql::created_doc_id(&created, "AgentToolControlClaim")?, None)
            };
            Ok(Some(Intent {
                doc_id: intent.doc_id.clone(), intent_id: intent.intent_id.clone(), intent_json: intent.intent_json.clone(), client_did: intent.client_did.clone(),
                agent_did: intent.agent_did.clone(), node_did: intent.node_did.clone(),
                session_id: intent.session_id.clone(), session_doc_id: intent.session_doc_id.clone(),
                request_doc_id: intent.request_doc_id.clone(), requester_did: intent.requester_did.clone(),
                tool_call_id: intent.tool_call_id.clone(), tool_call_doc_id: intent.tool_call_doc_id.clone(),
                action: intent.action.clone(), state: "claimed".into(), claim_doc_id: claim_doc,
                existing_receipt,
            }))
        })
    }).await
}

pub(crate) async fn recover_claim(
    access: &ConfigAccess,
    agent: &identity::Did,
    node_did: &str,
    claim_doc_id: &str,
) -> Result<Option<Intent>> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent.as_str()),
        "claim recovery requires its selected agent"
    );
    access.transact_readonly("tool_control.recover_claim", |txn| {
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentToolControlClaim(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID intent_doc_id intent_json agent_did receipt_json }} }}"#, escape_graphql_string(claim_doc_id))).await?;
            let rows = response["data"]["AgentToolControlClaim"].as_array().context("claim recovery omitted rows")?;
            let [row] = rows.as_slice() else { return Ok(None); };
            let owners = txn.document_owners("AgentToolControlClaim", &[claim_doc_id.to_owned()]).await?;
            if owners.len() != 1 || owners[0].owner.as_ref().map(identity::Did::as_str) != Some(agent.as_str()) || row["agent_did"].as_str() != Some(agent.as_str()) || !row["receipt_json"].is_null() { return Ok(None); }
            let raw = row["intent_json"].as_str().context("claim has no retained signed intent")?;
            let signed: gents_protocol::process_control::ToolControlIntent = serde_json::from_str(raw)?;
            if signed.validate_shape().is_err() || signed.agent_did != agent.as_str() || signed.node_did != node_did
                || !crate::store::identity::verify_did_signature(&signed.client_did, &signed.signing_payload()?, &signed.signature).unwrap_or(false) { return Ok(None); }
            let intent_doc_id = row["intent_doc_id"].as_str().context("claim has no intent document")?;
            Ok(Some(Intent {
                doc_id: intent_doc_id.into(), intent_id: signed.intent_id, intent_json: raw.into(),
                client_did: signed.client_did, agent_did: signed.agent_did, node_did: signed.node_did,
                session_id: signed.session_id, session_doc_id: signed.session_doc_id, request_doc_id: signed.request_doc_id,
                requester_did: signed.requester_did, tool_call_id: signed.tool_call_id,
                tool_call_doc_id: signed.tool_call_doc_id, action: signed.action, state: "claimed".into(),
                claim_doc_id: claim_doc_id.into(), existing_receipt: None,
            }))
        })
    }).await
}

pub(crate) async fn execute_claimed(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    agent: &identity::Did,
    intent: Intent,
) -> Result<()> {
    use crate::tools::call::lifecycle::{ToolCallLifecycle, ToolRowKind};
    use gents_protocol::process_control::ToolControlReceiptOutcome;
    use gents_protocol::tool_call::AwaitMode;
    let signer = access
        .managed_agent_signer()
        .context("tool control requires a managed Agent signer")?;
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent.as_str()) && signer.did() == agent.as_str(),
        "tool control actor mismatch"
    );
    let receipt = if let Some(receipt) = intent.existing_receipt.as_ref() {
        receipt.clone()
    } else {
        let attempted: Result<ToolControlReceiptOutcome> = async {
            let mut lifecycle = ToolCallLifecycle::load_by_doc_id_as(
                node.clone(),
                agent.clone(),
                &intent.tool_call_doc_id,
                &intent.node_did,
                &intent.session_id,
                intent.requester_did.as_deref(),
            )
            .await?
            .context("claimed invocation is unavailable to its selected agent")?;
            lifecycle.bind_agent_signer(Some(signer.clone()))?;
            anyhow::ensure!(
                lifecycle.actor() == agent && lifecycle.tool_call_id() == intent.tool_call_id,
                "claimed invocation has a different execution owner"
            );
            Ok(match intent.action.as_str() {
                "process_stop"
                    if lifecycle.kind() == ToolRowKind::Native
                        && lifecycle.await_mode() == AwaitMode::Background =>
                {
                    ToolControlReceiptOutcome::Process(
                        super::cancel_loaded_background_tool_call(
                            node.clone(),
                            executions,
                            lifecycle,
                            "explicit_cancel",
                        )
                        .await?,
                    )
                }
                "job_stop" | "job_pause" if lifecycle.kind() == ToolRowKind::PluginJob => {
                    let action = if intent.action == "job_stop" {
                        gents_protocol::job::JobControlKind::Stop
                    } else {
                        gents_protocol::job::JobControlKind::Pause
                    };
                    let scope = crate::tools::background::status::ProcessControlScope {
                        request_id: String::new(),
                        session_id: intent.session_id.clone(),
                        node_did: intent.node_did.clone(),
                        requester_did: intent.requester_did.clone(),
                    };
                    ToolControlReceiptOutcome::Job(
                        crate::tools::background::job::control::control_lifecycle(
                            access, &scope, lifecycle, action,
                        )
                        .await?,
                    )
                }
                _ => ToolControlReceiptOutcome::Rejected,
            })
        }
        .await;
        let outcome = match attempted {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(tool_control_doc_id = %intent.doc_id, %error, "tool control outcome could not be confirmed");
                ToolControlReceiptOutcome::Unconfirmed
            }
        };
        let signed = signed_receipt(&intent, outcome, &signer).await?;
        access.transact_write("tool_control.receipt", |txn| {
            let signed = &signed;
            let intent = &intent;
            Box::pin(async move {
                let query = format!(r#"{{ AgentToolControlClaim(filter: {{ _docID: {{ _eq: "{}" }}, intent_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}, limit: 2) {{ _docID receipt_json }} }}"#, escape_graphql_string(&intent.claim_doc_id), escape_graphql_string(&intent.doc_id), escape_graphql_string(agent.as_str()));
                let response = txn.execute(&query).await?;
                let rows = response["data"]["AgentToolControlClaim"].as_array().context("receipt claim query omitted rows")?;
                let [row] = rows.as_slice() else { anyhow::bail!("receipt claim is unavailable"); };
                let owners = txn.document_owners("AgentToolControlClaim", std::slice::from_ref(&intent.claim_doc_id)).await?;
                anyhow::ensure!(owners.len() == 1 && owners[0].owner.as_ref().map(identity::Did::as_str) == Some(agent.as_str()), "receipt claim has a different native owner");
                if let Some(existing) = row["receipt_json"].as_str() { return Ok(existing.to_owned()); }
                txn.execute_with_variables(
                    &format!(r#"mutation($input: AgentToolControlClaimMutationInputArg!) {{ update_AgentToolControlClaim(docID: "{}", input: $input) {{ _docID }} }}"#, escape_graphql_string(&intent.claim_doc_id)),
                    &serde_json::json!({"input": {"receipt_json": signed}}),
                ).await?;
                Ok::<_, anyhow::Error>(signed.clone())
            })
        }).await?
    };
    access.transact_write("tool_control.publish_receipt", |txn| {
        let receipt = &receipt;
        let intent = &intent;
        Box::pin(async move {
            let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, &intent.node_did, &intent.session_id).await?.context("receipt session is unavailable")?;
            anyhow::ensure!(session.doc_id == intent.session_doc_id && session.session.agent_did == intent.agent_did, "receipt session scope changed");
            let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(txn, crate::Collection::Agent, &intent.node_did, &intent.agent_did).await?.context("receipt agent is unavailable")?;
            anyhow::ensure!(client_authority(txn, intent, &agent_doc).await? == Some(true), "receipt publication is not authorized");
            let response = txn.execute_with_variables(
                &format!(r#"mutation($input: AgentToolControlMutationInputArg!) {{ update_AgentToolControl(docID: "{}", input: $input) {{ _docID }} }}"#, escape_graphql_string(&intent.doc_id)),
                &serde_json::json!({"input": {"state": "done", "outcome_json": receipt}}),
            ).await?;
            anyhow::ensure!(response["data"]["update_AgentToolControl"].as_array().is_some_and(|rows| rows.len() == 1), "receipt mirror was not published");
            Ok::<_, anyhow::Error>(())
        })
    }).await?;
    Ok(())
}

/// The runtime sweeps with each active selected agent. Claimed work remains
/// durable until its existing process or job owner confirms an outcome.
pub(crate) async fn run(
    node: Arc<EmbeddedNode>,
    node_did: String,
    executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    mut agents: tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut changes = crate::subscribe_collections(&node, ["AgentToolControl"])?;
    let mut interval =
        tokio::time::interval(crate::runtime::agent::p2p_reconcile::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failed = rapidhash::RapidHashSet::default();
    loop {
        let (identities, active) = {
            let snapshot = agents.borrow();
            (
                snapshot.watcher_identities()?,
                snapshot
                    .agents
                    .keys()
                    .cloned()
                    .collect::<rapidhash::RapidHashSet<_>>(),
            )
        };
        for selected in identities {
            let agent = identity::Did::new(selected.did())?;
            let access = match ConfigAccess::local_managed_agent(
                node.clone(),
                agent.clone(),
                selected.as_ref(),
            ) {
                Ok(access) => access,
                Err(error) => {
                    tracing::warn!(agent_did = %agent, %error, "tool control signer unavailable");
                    continue;
                }
            };
            let claims_query = format!(
                r#"{{ AgentToolControlClaim(filter: {{ agent_did: {{ _eq: "{}" }}, receipt_json: {{ _eq: null }} }}) {{ _docID }} }}"#,
                escape_graphql_string(agent.as_str())
            );
            match access.execute(&claims_query).await {
                Ok(result) => {
                    // An Agent without the baseline (a disabled one is still
                    // watched) cannot read its claims; that Agent's sweep is
                    // unavailable, not the runtime's.
                    let Some(claims) = result["data"]["AgentToolControlClaim"].as_array() else {
                        tracing::warn!(agent_did = %agent, "tool control claim sweep omitted rows");
                        continue;
                    };
                    for id in claims.iter().filter_map(|row| row["_docID"].as_str()) {
                        if failed.contains(id) {
                            continue;
                        }
                        match recover_claim(&access, &agent, &node_did, id).await {
                            Ok(Some(intent)) => {
                                if let Err(error) =
                                    execute_claimed(&node, &access, &executions, &agent, intent)
                                        .await
                                {
                                    tracing::warn!(claim_doc_id = id, %error, "retained tool control could not complete");
                                    failed.insert(id.to_owned());
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                tracing::warn!(claim_doc_id = id, %error, "tool control claim recovery failed");
                                failed.insert(id.to_owned());
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(agent_did = %agent, %error, "tool control claim sweep failed")
                }
            }
            let query = format!(
                r#"{{ AgentToolControl(filter: {{ node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, state: {{ _in: ["pending", "claimed"] }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&node_did),
                escape_graphql_string(agent.as_str())
            );
            let result = match access.execute(&query).await {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(%error, "tool control sweep failed");
                    continue;
                }
            };
            let Some(rows) = result["data"]["AgentToolControl"].as_array() else {
                tracing::warn!(agent_did = %agent, "tool control sweep omitted rows");
                continue;
            };
            for id in rows.iter().filter_map(|row| row["_docID"].as_str()) {
                if failed.contains(id) {
                    continue;
                }
                match claim_inner(
                    &node,
                    &access,
                    &agent,
                    id,
                    active.contains(agent.as_str()),
                    #[cfg(test)]
                    None,
                )
                .await
                {
                    Ok(Some(intent)) => {
                        if let Err(error) =
                            execute_claimed(&node, &access, &executions, &agent, intent).await
                        {
                            tracing::warn!(tool_control_doc_id = id, %error, "claimed tool control could not complete");
                            failed.insert(id.to_owned());
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(tool_control_doc_id = id, %error, "tool control could not be claimed");
                        failed.insert(id.to_owned());
                    }
                }
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => { failed.clear(); },
            changed = changes.recv() => { if changed.is_none() { return Err(anyhow::anyhow!("tool control change subscription closed")); } },
            changed = agents.changed() => { if changed.is_err() { return Err(anyhow::anyhow!("active agent snapshot closed")); } },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientControlObservationError {
    #[error("control receipt is unavailable or invalid")]
    Unavailable(#[source] anyhow::Error),
}

/// Verify the selected agent's signature and physical invocation binding using
/// only the authenticated client's native reads.
pub async fn read_receipt(
    access: &ConfigAccess,
    node_did: &str,
    intent_doc_id: &str,
    tool_call_doc_id: &str,
    action: &str,
) -> Result<
    Option<gents_protocol::process_control::ToolControlReceipt>,
    ClientControlObservationError,
> {
    read_receipt_inner(access, node_did, intent_doc_id, tool_call_doc_id, action)
        .await
        .map_err(ClientControlObservationError::Unavailable)
}

async fn read_receipt_inner(
    access: &ConfigAccess,
    node_did: &str,
    intent_doc_id: &str,
    tool_call_doc_id: &str,
    action: &str,
) -> Result<Option<gents_protocol::process_control::ToolControlReceipt>> {
    use gents_protocol::process_control::ToolControlReceipt;
    let actor = access
        .actor_did()
        .context("control receipt requires an authenticated actor")?;
    let escape = crate::store::graphql::escape_graphql_string;
    let query = format!(
        r#"{{ AgentToolControl(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID client_did agent_did node_did session_id session_doc_id request_doc_id tool_call_id tool_call_doc_id action outcome_json }} AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID node_did agent_did session_id session_doc_id request_doc_id tool_call_id }} AgentToolControlClaim(filter: {{ intent_doc_id: {{ _eq: "{}" }} }}) {{ agent_did receipt_json }} }}"#,
        escape(intent_doc_id),
        escape(tool_call_doc_id),
        escape(intent_doc_id)
    );
    let response = access.execute(&query).await?;
    let intents = response["data"]["AgentToolControl"]
        .as_array()
        .context("control intent observation omitted rows")?;
    let tools = response["data"]["AgentToolCall"]
        .as_array()
        .context("control invocation observation omitted rows")?;
    let ([intent], [tool]) = (intents.as_slice(), tools.as_slice()) else {
        anyhow::bail!("control intent or invocation is no longer visible");
    };
    let field = |value: &serde_json::Value, name: &str| -> Result<String> {
        Ok(value[name]
            .as_str()
            .with_context(|| format!("control observation omitted {name}"))?
            .to_owned())
    };
    let claims = response["data"]["AgentToolControlClaim"]
        .as_array()
        .context("control claim observation omitted rows")?;

    anyhow::ensure!(
        field(intent, "_docID")? == intent_doc_id
            && field(intent, "client_did")? == actor.as_str()
            && field(intent, "node_did")? == node_did
            && field(intent, "tool_call_doc_id")? == tool_call_doc_id
            && field(intent, "action")? == action
            && field(tool, "_docID")? == tool_call_doc_id
            && field(tool, "node_did")? == node_did,
        "control intent is bound to a different request"
    );
    for key in [
        "agent_did",
        "session_id",
        "session_doc_id",
        "request_doc_id",
        "tool_call_id",
    ] {
        anyhow::ensure!(
            field(intent, key)? == field(tool, key)?,
            "control invocation scope changed"
        );
    }
    let validate = |raw: &str| -> Result<ToolControlReceipt> {
        let receipt: ToolControlReceipt = serde_json::from_str(raw)?;
        let expected = ToolControlReceipt {
            version: 1,
            intent_doc_id: intent_doc_id.into(),
            client_did: actor.clone(),
            agent_did: field(tool, "agent_did")?,
            node_did: node_did.into(),
            session_id: field(tool, "session_id")?,
            session_doc_id: field(tool, "session_doc_id")?,
            request_doc_id: field(tool, "request_doc_id")?,
            tool_call_id: field(tool, "tool_call_id")?,
            tool_call_doc_id: tool_call_doc_id.into(),
            action: action.into(),
            outcome: receipt.outcome.clone(),
            signature: Vec::new(),
        };
        receipt.validate_for(&expected)?;
        anyhow::ensure!(
            crate::store::identity::verify_did_signature(
                &receipt.agent_did,
                &receipt.signing_payload()?,
                &receipt.signature
            )?,
            "control receipt has no verified agent signature"
        );
        Ok(receipt)
    };
    if let Some(raw) = intent["outcome_json"].as_str() {
        return Ok(Some(validate(raw)?));
    }
    for claim in claims {
        if claim["agent_did"].as_str() != tool["agent_did"].as_str() {
            continue;
        }
        if let Some(receipt) = claim["receipt_json"]
            .as_str()
            .and_then(|raw| validate(raw).ok())
        {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod signed_conformance {
    use super::*;
    use gents_protocol::process_control::ToolControlIntent;

    #[tokio::test]
    async fn generated_signed_control_scope_binds_client_and_physical_targets() {
        let directory = tempfile::tempdir().unwrap();
        let client =
            crate::KeyIdentity::load_or_create(directory.path().join("client.key"), None).unwrap();
        let other =
            crate::KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
        let row = Invocation {
            doc_id: "tool-doc".into(),
            node_did: "node".into(),
            agent_did: "agent".into(),
            session_id: "session".into(),
            session_doc_id: "session-doc".into(),
            request_doc_id: "request-doc".into(),
            requester_did: Some("requester".into()),
            tool_call_id: "call".into(),
        };
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().client_signed_control_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let signer = if case["author"] == "client" {
                &client
            } else {
                &other
            };
            let mut intent = ToolControlIntent {
                version: 1,
                intent_id: "unique".into(),
                client_did: signer.did().into(),
                node_did: case["node"].as_str().unwrap().into(),
                agent_did: case["agent"].as_str().unwrap().into(),
                session_id: "session".into(),
                session_doc_id: case["session"].as_str().unwrap().into(),
                request_doc_id: case["request"].as_str().unwrap().into(),
                requester_did: Some("requester".into()),
                tool_call_id: case["call"].as_str().unwrap().into(),
                tool_call_doc_id: case["document"].as_str().unwrap().into(),
                action: case["action"].as_str().unwrap().into(),
                signature: Vec::new(),
            };
            intent.signature = signer
                .sign(&intent.signing_payload().unwrap())
                .await
                .unwrap();
            if case["signature_valid"] == false {
                intent.signature[0] ^= 1;
            }
            assert_eq!(
                signed_invocation_authorized(&intent, client.did(), "process_stop", &row),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
