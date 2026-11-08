use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use std::sync::Arc;

use crate::store::graphql::escape_graphql_string;
use gents_protocol::request_lifecycle::SessionMessageCause;

use super::plan::{Plan, PlannedWrite};

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub(crate) struct SessionMessageReceipt {
    pub ok: bool,
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    pub await_mode: String,
    pub status: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CommitError {
    #[error(transparent)]
    Steer(#[from] crate::request::lifecycle::queue::SteerAdmissionRefusal),
    #[error(transparent)]
    Access(#[from] crate::store::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    #[error("session-message commit is unconfirmed")]
    Unconfirmed(#[source] Box<CommitError>),
    #[error("session-message tool became {state:?} before dispatch")]
    ObservedTerminal {
        state: crate::tools::call::lifecycle::ToolCallState,
    },
}

impl crate::store::config_client::TransactionFailure for CommitError {
    fn config_client_error(&self) -> Option<&crate::store::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Other(error) => {
                crate::store::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Steer(_) => None,
            Self::Unconfirmed(error) => {
                crate::store::config_client::TransactionFailure::config_client_error(error.as_ref())
            }
            Self::ObservedTerminal { .. } => None,
        }
    }
}

pub(crate) async fn commit(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let new_child = agent_new_create(&plan).cloned();
    let expected_request = match &plan.write {
        PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => {
            (create.request_id.clone(), create.session_id.clone())
        }
    };
    let mut changes = new_child
        .as_ref()
        .map(|_| crate::subscribe_collections(node, ["AgentToolCall"]))
        .transpose()?;
    let error = match Box::pin(commit_once(node, cause, lifecycle, plan, report_delivery)).await {
        Ok(receipt) => {
            if new_child.is_some() {
                complete_new_child_relationships(node, lifecycle, &receipt).await;
            }
            return Ok(receipt);
        }
        Err(error) => error,
    };
    if let (CommitError::Unconfirmed(_), Some(expected)) = (&error, new_child.as_ref()) {
        let changes = changes
            .as_mut()
            .expect("new child subscribed before publication");
        return Box::pin(observe_unconfirmed_child(
            node, cause, lifecycle, expected, changes,
        ))
        .await;
    }
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let mut durable = crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
        node.clone(),
        lifecycle.actor().clone(),
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .context("session-message row disappeared after its commit failed")?;
    durable.inherit_signer(lifecycle)?;
    if durable.state() == crate::tools::call::lifecycle::ToolCallState::Pending {
        return Err(error);
    }
    let receipt = load_receipt(
        &lifecycle.access()?,
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .filter(|_| durable.is_running())
    .with_context(|| format!("session-message commit outcome is unknown after: {error:#}"))?;
    if receipt.request_id != expected_request.0
        || receipt.session_id != expected_request.1
        || receipt.tool_call_id != lifecycle.tool_call_id()
    {
        return Err(anyhow::anyhow!(
            "committed session-message receipt differs from the planned request"
        )
        .into());
    }
    if let Some(expected) = new_child {
        if receipt.request_id != expected.request_id || receipt.session_id != expected.session_id {
            return Err(anyhow::anyhow!(
                "committed child receipt differs from the planned request"
            )
            .into());
        }
        complete_new_child_relationships(node, lifecycle, &receipt).await;
    }
    *lifecycle = durable;
    Ok(receipt)
}

async fn observe_unconfirmed_child(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
    expected: &gents_protocol::request_admission::AgentRequestCreate,
    changes: &mut events::DocumentChangeSubscription,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    enum Observation {
        Receipt(
            Box<crate::tools::call::lifecycle::ToolCallLifecycle>,
            SessionMessageReceipt,
        ),
        Terminal(crate::tools::call::lifecycle::ToolCallState),
    }
    let access = lifecycle.access()?;
    let tool_doc_id = lifecycle
        .doc_id()
        .context("unconfirmed session-message lacks its tool document")?
        .to_owned();
    loop {
        let observed: anyhow::Result<_> = async {
            let durable = crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
                node.clone(),
                lifecycle.actor().clone(),
                &tool_doc_id,
                lifecycle.node_did(),
                lifecycle.session_id(),
                lifecycle.requester_did(),
            )
            .await?;
            let Some(mut durable) = durable else {
                return Ok(None);
            };
            durable.inherit_signer(lifecycle)?;
            if durable.started_at().is_none()
                && matches!(durable.state(),
                    crate::tools::call::lifecycle::ToolCallState::Failed
                        | crate::tools::call::lifecycle::ToolCallState::Cancelled
                        | crate::tools::call::lifecycle::ToolCallState::TimedOut)
            {
                return Ok(Some(Observation::Terminal(durable.state())));
            }
            let Some(receipt) = load_receipt(
                &access,
                &tool_doc_id,
                lifecycle.node_did(),
                lifecycle.session_id(),
                lifecycle.requester_did(),
            )
            .await?
            else {
                return Ok(None);
            };
            if durable.started_at().is_none()
                || receipt.request_id != expected.request_id
                || receipt.session_id != expected.session_id
                || receipt.tool_call_id != lifecycle.tool_call_id()
            {
                return Ok(None);
            }
            let doc = escape_graphql_string(&receipt.request_doc_id);
            let response = access.execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ {} }} }}"#,
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            )).await?;
            let Some([row]) = response["data"]["AgentRequest"].as_array().map(Vec::as_slice) else {
                return Ok(None);
            };
            let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
            if row.doc_id.as_deref() != Some(receipt.request_doc_id.as_str())
                || row.request_id != expected.request_id
                || row.node_did.as_deref() != Some(cause.caller_node_did.as_str())
                || row.session_id.as_deref() != Some(expected.session_id.as_str())
                || row.agent_did.as_deref() != Some(expected.agent_did.as_str())
                || row.requester_did.as_deref() != Some(cause.caller_agent_did.as_str())
                || row.admission_signer_did.as_deref() != Some(cause.caller_agent_did.as_str())
                || row.admission_kind.as_deref() != Some("client")
                || row.caused_by_parent_request_doc_id.as_deref() != Some(cause.caller_request_doc_id.as_str())
                || row.caused_by_parent_tool_call_doc_id.as_deref() != Some(tool_doc_id.as_str())
                || row.session_doc_id.as_deref().is_none_or(str::is_empty)
                || row.agent_doc_id.as_deref().is_none_or(str::is_empty)
            {
                return Ok(None);
            }
            let mut bound = expected.clone();
            bound.admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                &cause.caller_agent_did,
            );
            bound.session_doc_id.clone_from(&row.session_doc_id);
            bound.agent_doc_id.clone_from(&row.agent_doc_id);
            if !crate::store::identity::request_receipt::signed_request_matches_create(&row, &bound)? {
                return Ok(None);
            }
            let session_doc = row.session_doc_id.as_deref().expect("nonempty session binding");
            let agent_doc = row.agent_doc_id.as_deref().expect("nonempty agent binding");
            let exact_targets = access.transact_readonly("session_message.observe_unconfirmed_targets", |txn| {
                let request_doc = receipt.request_doc_id.as_str();
                Box::pin(async move {
                    let request_owners = txn.document_owners("AgentRequest", &[request_doc.to_owned()]).await?;
                    let session_owners = txn.document_owners("AgentSession", &[session_doc.to_owned()]).await?;
                    let owned = |owners: &[query::owners::DocumentOwnerObservation], doc: &str| {
                        owners.len() == 1 && owners[0].doc_id == doc && owners[0].visible
                            && owners[0].owner.as_ref().is_some_and(|owner| owner.as_str() == cause.caller_agent_did)
                    };
                    if !owned(&request_owners, request_doc) || !owned(&session_owners, session_doc) {
                        return Ok::<_, anyhow::Error>(false);
                    }
                    let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, &expected.node_did, &expected.session_id).await?;
                    let agent = crate::store::config_client::read_desired_state_record_in_txn(
                        txn, crate::Collection::Agent, &expected.node_did, &expected.agent_did,
                    ).await?;
                    Ok(session.is_some_and(|session| session.doc_id == session_doc
                        && session.session.requester_did.as_deref() == Some(expected.requester_did.as_str())
                        && session.session.agent_did == expected.agent_did)
                        && agent.is_some_and(|(doc, _)| doc == agent_doc))
                })
            }).await?;
            if !exact_targets {
                return Ok(None);
            }
            let Some(source) = expected.input.started_by.as_ref() else {
                return Ok(None);
            };
            let target = gents_protocol::session::SessionScope {
                node_did: expected.node_did.clone(),
                session_id: expected.session_id.clone(),
                requester_did: Some(expected.requester_did.clone()),
            };
            if !crate::session::origin::verified_agent_new_creation_for_opening(
                &access,
                &target,
                &gents_protocol::session::SessionScope::from(source),
                &receipt.request_doc_id,
            ).await? {
                return Ok(None);
            }
            Ok(Some(Observation::Receipt(Box::new(durable), receipt)))
        }
        .await;
        match observed {
            Ok(Some(Observation::Receipt(durable, receipt))) => {
                complete_new_child_relationships(node, lifecycle, &receipt).await;
                *lifecycle = *durable;
                return Ok(receipt);
            }
            Ok(Some(Observation::Terminal(state))) => {
                return Err(CommitError::ObservedTerminal { state });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, tool_call_doc_id = %tool_doc_id, "unconfirmed child receipt observation failed")
            }
        }
        if let Err(error) = crate::request::watcher::wait_for_change_or_resubscribe(changes, || {
            crate::subscribe_collections(node, ["AgentToolCall"])
        })
        .await
        {
            tracing::warn!(%error, "unconfirmed child change subscription unavailable");
        }
    }
}

fn agent_new_create(plan: &Plan) -> Option<&gents_protocol::request_admission::AgentRequestCreate> {
    let create = match &plan.write {
        PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
    };
    (create.input.session_creation
        == Some(gents_protocol::request_input::SessionCreationIntent::AgentNew))
    .then_some(create)
}

async fn complete_new_child_relationships(
    _node: &Arc<EmbeddedNode>,
    lifecycle: &crate::tools::call::lifecycle::ToolCallLifecycle,
    receipt: &SessionMessageReceipt,
) {
    let access = match lifecycle.access() {
        Ok(access) => access,
        Err(error) => {
            tracing::warn!(request_doc_id = %receipt.request_doc_id, %error, "committed child relationships need recovery");
            return;
        }
    };
    if let Err(error) = crate::request::lifecycle::complete_client_request_relationships(
        &access,
        &receipt.request_doc_id,
    )
    .await
    {
        tracing::warn!(request_doc_id = %receipt.request_doc_id, %error, "committed child relationships need recovery");
    }
}

async fn commit_once(
    _node: &std::sync::Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let delivery = report_delivery.then(|| plan.delivery().as_str().to_owned());
    let tool_call_id = lifecycle.tool_call_id().to_owned();
    let receipt_for = move |enqueued: &gents_protocol::request_lifecycle::EnqueuedAgentRequest| {
        SessionMessageReceipt {
            ok: true,
            session_id: enqueued.session_id.clone(),
            request_id: enqueued.request_id.clone(),
            request_doc_id: enqueued.doc_id.clone(),
            tool_call_id: tool_call_id.clone(),
            delivery: delivery.clone(),
            await_mode: "background".to_owned(),
            status: "running".to_owned(),
        }
    };
    let binding = lifecycle.background_receipt_binding()?;
    let start = lifecycle.dispatch_start()?;
    let (start, binding, plan, receipt_for) = (&start, &binding, &plan, &receipt_for);
    if lifecycle.actor().as_str() != cause.caller_agent_did
        || lifecycle.node_did() != cause.caller_node_did
        || lifecycle.session_id() != cause.caller_session_id
        || lifecycle.requester_did() != cause.caller_requester_did.as_deref()
        || lifecycle.request_doc_id() != Some(cause.caller_request_doc_id.as_str())
        || lifecycle.doc_id() != Some(cause.tool_call_doc_id.as_str())
        || lifecycle.tool_call_id() != cause.tool_call_id
    {
        return Err(
            anyhow::anyhow!("session-message cause differs from the accepted tool call").into(),
        );
    }
    let label = match &plan.write {
        PlannedWrite::Request { .. } => "session_message.commit_request",
        PlannedWrite::Goal { .. } => "session_message.commit_goal",
    };
    let (receipt, started_at) =
        lifecycle.access()?.transact_write_with_finality(
            label,
            move |txn| {
                Box::pin(async move {
                    let started_at = start_dispatch(txn, start).await?;
                    let (enqueued, new_child) = if agent_new_create(plan).is_some() {
                        let (enqueued, create) = stage_agent_new_in_txn(txn, cause, plan).await?;
                        (enqueued, Some(create))
                    } else {
                        (stage_planned_write_in_txn(txn, cause, plan).await?, None)
                    };
                    let receipt = receipt_for(&enqueued);
                    crate::tools::call::lifecycle::publish_background_receipt_in_txn(
                        txn,
                        binding,
                        &serde_json::to_string(&receipt).map_err(anyhow::Error::from)?,
                    )
                    .await?;
                    if let Some(create) = &new_child {
                        crate::session::finalize_agent_new_session_in_txn(
                            txn,
                            create,
                            create
                                .session_doc_id
                                .as_deref()
                                .context("new child lacks session binding")?,
                            &enqueued.doc_id,
                        )
                        .await?;
                    }
                    Ok::<_, CommitError>((receipt, started_at))
                })
            },
        )
        .await
    .map_err(|failure| {
        let post_commit_or_unknown = matches!(
            crate::store::config_client::TransactionFailure::config_client_error(&failure.error),
            Some(crate::store::config_client::ConfigClientError::CommittedRelationshipsIncomplete { .. }
                | crate::store::config_client::ConfigClientError::ClientAgentPublicationUnconfirmed(_))
        );
        if failure.unconfirmed || post_commit_or_unknown {
                CommitError::Unconfirmed(Box::new(failure.error))
            } else {
                failure.error
            }
        })?;
    lifecycle.mark_started(started_at);
    crate::store::config_client::session_children::publish_agent_session_child_relationships_with_access(
        &lifecycle.access()?,
        lifecycle.node_did(),
        lifecycle.actor(),
        lifecycle.session_id(),
    )
    .await?;
    Ok(receipt)
}

async fn stage_agent_new_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    cause: &SessionMessageCause,
    plan: &Plan,
) -> anyhow::Result<(
    gents_protocol::request_lifecycle::EnqueuedAgentRequest,
    gents_protocol::request_admission::AgentRequestCreate,
)> {
    let mut create = agent_new_create(plan)
        .context("new child plan is missing its request")?
        .clone();
    anyhow::ensure!(
        create.node_did == cause.caller_node_did && create.requester_did == cause.caller_agent_did,
        "new child is not authored by its local parent agent"
    );
    let session_doc = crate::session::stage_agent_new_session_in_txn(txn, &create).await?;
    create.admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
        &cause.caller_agent_did,
    );
    crate::request::lifecycle::bind_client_targets_in_txn(txn, &mut create).await?;
    anyhow::ensure!(
        create.session_doc_id.as_deref() == Some(session_doc.as_str()),
        "new child session binding changed before signing"
    );
    let signer = txn
        .managed_agent_signer()
        .context("session creation requires a managed Agent signer")?;
    anyhow::ensure!(
        signer.did() == cause.caller_agent_did,
        "session creation signer differs from caller"
    );
    crate::sign_agent_request_create(&signer, &mut create).await?;
    ensure_planned_hop_current(txn, &create).await?;
    let enqueued = match &plan.write {
        PlannedWrite::Request { .. } => {
            let doc_id = crate::request::lifecycle::stage_pending_child_request_in_txn(
                txn,
                &create,
                &session_doc,
                create
                    .agent_doc_id
                    .as_deref()
                    .context("new child lacks Agent binding")?,
            )
            .await?;
            gents_protocol::request_lifecycle::EnqueuedAgentRequest {
                doc_id,
                request_id: create.request_id.clone(),
                session_id: create.session_id.clone(),
            }
        }
        PlannedWrite::Goal {
            objective,
            token_budget,
            ..
        } => {
            let enqueued = crate::goal::stage_goal_backed_request_in_txn(
                txn,
                &create.node_did,
                &create.session_id,
                objective,
                *token_budget,
                &create,
            )
            .await?;
            enqueued
        }
    };
    Ok((enqueued, create))
}

/// Create the planned request, or stage the Goal with it, in the caller's
/// transaction. It writes no tool row.
pub(crate) async fn stage_planned_write_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    cause: &SessionMessageCause,
    plan: &Plan,
) -> std::result::Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest, CommitError> {
    if plan.interrupt_before_publish {
        let create = match &plan.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        let active = crate::request::interrupt::interrupt_active_session_in_txn(
            txn,
            &create.session_id,
            &create.node_did,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if active.is_none() && create.node_did == cause.caller_node_did {
            crate::goal::pause_goal_for_interrupt_in_txn(txn, &create.node_did, &create.session_id)
                .await?;
        }
    }
    match &plan.write {
        PlannedWrite::Request { create, .. } => {
            let (create, doc_id) = async {
                anyhow::ensure!(
                    create.node_did == cause.caller_node_did
                        && create.requester_did == cause.caller_agent_did,
                    "session message must be authored by its local calling Agent"
                );
                let signer = txn
                    .managed_agent_signer()
                    .context("session message requires the held calling Agent")?;
                anyhow::ensure!(
                    signer.did() == cause.caller_agent_did,
                    "session message signer differs from caller"
                );
                let mut create = create.clone();
                let job_session = if create.input.session_creation
                    == Some(gents_protocol::request_input::SessionCreationIntent::PluginJob)
                {
                    Some(crate::session::stage_plugin_job_session_in_txn(txn, &create).await?)
                } else {
                    None
                };
                create.admission =
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                        &cause.caller_agent_did,
                    );
                crate::request::lifecycle::bind_client_targets_in_txn(txn, &mut create).await?;
                crate::sign_agent_request_create(&signer, &mut create).await?;
                ensure_planned_hop_current(txn, &create).await?;
                let session = create
                    .session_doc_id
                    .as_deref()
                    .context("session message lacks a visible session")?;
                let agent = create
                    .agent_doc_id
                    .as_deref()
                    .context("session message lacks a visible Agent")?;
                let doc_id = crate::request::lifecycle::stage_client_request_in_txn(
                    txn, &create, session, agent,
                )
                .await?;
                if let Some(session_doc) = job_session {
                    anyhow::ensure!(session == session_doc, "job child session binding changed");
                    crate::session::finalize_plugin_job_session_in_txn(
                        txn,
                        &create,
                        &session_doc,
                        &doc_id,
                    )
                    .await?;
                }
                let response = txn.execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {} }} }}"#,
                escape_graphql_string(&doc_id), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            )).await?;
                let [row] = response["data"]["AgentRequest"]
                    .as_array()
                    .context("session message observation unavailable")?
                    .as_slice()
                else {
                    anyhow::bail!("session message observation ambiguous")
                };
                let row: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone())?;
                let source = crate::session::origin::verified_agent_source(txn, &row)
                    .await?
                    .context("session message source is unavailable")?;
                anyhow::ensure!(
                    source.node_did == cause.caller_node_did
                        && source.session_id == cause.caller_session_id
                        && source.requester_did == cause.caller_requester_did,
                    "session message source differs from the accepted call"
                );
                Ok::<_, anyhow::Error>((create, doc_id))
            }
            .await?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, &create, &doc_id, expected_head.as_deref()).await??;
            }
            Ok(gents_protocol::request_lifecycle::EnqueuedAgentRequest {
                doc_id,
                request_id: create.request_id.clone(),
                session_id: create.session_id.clone(),
            })
        }
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        } => {
            ensure_planned_hop_current(txn, create).await?;
            let enqueued = crate::goal::stage_goal_backed_request_in_txn(
                txn,
                &create.node_did,
                &plan.session_id,
                objective,
                *token_budget,
                create,
            )
            .await?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, create, &enqueued.doc_id, expected_head.as_deref())
                    .await??;
            }
            Ok(enqueued)
        }
    }
}

async fn validate_steer_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    candidate_doc_id: &str,
    expected_head: Option<&str>,
) -> Result<std::result::Result<(), crate::request::lifecycle::queue::SteerAdmissionRefusal>> {
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }},
            purpose: {{ _eq: "normal" }}, lifecycle_state: {{ _in: {claimed_or_processing} }} }}, limit: 2) {{
            {} execution_lease_secs interrupt_requested_at
        }} }}"#,
        escape_graphql_string(&create.node_did),
        escape_graphql_string(&create.session_id),
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS, claimed_or_processing = RequestLifecycleState::graphql_list([RequestLifecycleState::Claimed, RequestLifecycleState::Processing])
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active requests in target session"
    );
    anyhow::ensure!(
        rows.first().and_then(|row| row.doc_id.as_deref()) == expected_head,
        "the target session changed while the steer was planned"
    );
    let Some(row) = rows.into_iter().next() else {
        return Ok(Ok(()));
    };
    let head = gents_protocol::row::AgentRequest::try_from(row)?;
    let pending = crate::request::lifecycle::queue::pending_with_new_candidate_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
        candidate_doc_id,
    )
    .await?;
    Ok(crate::request::lifecycle::queue::steer_reachable(
        &head,
        pending,
        candidate_doc_id,
        chrono::Utc::now(),
    ))
}

async fn ensure_planned_hop_current(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let current =
        crate::session::load_session_current_hop_in_txn(txn, &create.node_did, &create.session_id)
            .await?;
    anyhow::ensure!(
        create.request_hop >= current,
        "the target session moved to hop {current} while this message was planned"
    );
    Ok(())
}

async fn start_dispatch(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    start: &crate::tools::call::lifecycle::delivery::DispatchStart,
) -> Result<chrono::DateTime<chrono::Utc>> {
    crate::tools::call::lifecycle::delivery::start_running_in_txn(txn, start, None)
        .await?
        .context("session-message row was altered or is no longer pending")
}

pub(crate) async fn load_receipt(
    access: &crate::store::config_client::ConfigAccess,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionMessageReceipt>> {
    let read = crate::tools::call::lifecycle::query::load_tool_call_read(
        access,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    let Some(message) = read.result else {
        return Ok(None);
    };
    let text = crate::tools::call::lifecycle::query::render_tool_result(&message)?;
    Ok(serde_json::from_str::<SessionMessageReceipt>(&text)
        .ok()
        .filter(|receipt| receipt.ok))
}
