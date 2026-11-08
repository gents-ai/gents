use super::*;
use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;

pub(super) mod fingerprint;
pub(super) mod recovery;
use recovery::load_goal_backed_request_by_retry_key_from_access;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalBackedRequestDisposition {
    Created,
    Idempotent,
}

struct StagedGoalBackedRequest {
    disposition: GoalBackedRequestDisposition,
    state: GoalSubmissionState,
}

fn authorize_goal_submission_commit(
    staged: StagedGoalBackedRequest,
) -> Result<GoalBackedRequestDisposition> {
    let committed = goal_submission_step(staged.state, GoalSubmissionAction::Commit);
    anyhow::ensure!(
        committed.durable_goal
            && committed.runnable_request
            && !committed.staged_goal
            && !committed.staged_request,
        "goal-backed request transaction is not ready to commit"
    );
    Ok(staged.disposition)
}

fn resolve_ambiguous_goal_submission_commit(
    commit_error: anyhow::Error,
    recovery: Result<Option<gents_protocol::request_lifecycle::EnqueuedAgentRequest>>,
) -> Result<GoalBackedRequestDisposition> {
    match recovery {
        Ok(Some(_)) => Ok(GoalBackedRequestDisposition::Idempotent),
        Ok(None) => Err(commit_error),
        Err(recovery_error) => Err(anyhow::anyhow!(
            "goal-backed request recovery failed after ambiguous commit: {recovery_error:#}; original commit error: {commit_error:#}"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn stage_goal_backed_request(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
    client_graph: Option<&std::sync::Arc<db::block::builder::VerifiedClientCreate>>,
    reserved_runtime_session: bool,
) -> Result<StagedGoalBackedRequest> {
    let objective = objective.trim();
    anyhow::ensure!(!objective.is_empty(), "goal objective must be non-empty");
    anyhow::ensure!(
        token_budget.is_none_or(|budget| budget > 0),
        "goal token budget must be positive"
    );
    anyhow::ensure!(request.node_did == node_did, "goal/request DID mismatch");
    anyhow::ensure!(
        request.session_id == session_id,
        "goal/request session mismatch"
    );
    let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        node_did,
        session_id,
        Some(&request.requester_did),
    )
    .await?
    .context("goal request session is unavailable")?;
    anyhow::ensure!(
        session.session.agent_did == request.agent_did,
        "goal request session selects a different Agent"
    );
    let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
        node_did,
        &request.agent_did,
    )
    .await?
    .context("goal request Agent is unavailable")?;
    anyhow::ensure!(
        request.session_doc_id.as_deref() == Some(session.doc_id.as_str())
            && request.agent_doc_id.as_deref() == Some(agent_doc.as_str()),
        "goal request differs from its signed physical targets"
    );
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;

    let existing = txn
        .execute(&format!(
            r#"{{
                    AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
                        {SIGNED_REQUEST_FIELDS}
                    }}
                }}"#,
            escape_graphql_string(retry_key),
        ))
        .await?;
    let requests = existing
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(requests.len() <= 1, "goal request retry key is not unique");
    let (request_doc_id, disposition) = if let Some(existing_request) = requests.first() {
        let persisted: GoalBackedRequestFingerprint =
            serde_json::from_value(existing_request.clone())
                .context("decoding goal-backed request fingerprint")?;
        anyhow::ensure!(
            persisted == GoalBackedRequestFingerprint::from_create(request)?,
            "goal request retry key conflicts with different immutable request fields"
        );
        let existing_doc = existing_request["_docID"]
            .as_str()
            .context("persisted Goal request lacks physical identity")?;
        let receipt: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(existing_request.clone())?;
        crate::store::identity::request_receipt::verify_request_receipt_signature(&receipt)?;
        let owners = txn
            .document_owners("AgentRequest", &[existing_doc.to_owned()])
            .await?;
        if let Some(graph) = client_graph {
            let expected_signature = bs58::encode(&request.admission.signature).into_string();
            anyhow::ensure!(
                graph.doc_id() == existing_doc
                    && existing_request["admission_signature"].as_str()
                        == Some(expected_signature.as_str())
                    && matches!(owners.as_slice(), [owner] if owner.doc_id == existing_doc
                    && owner.visible && owner.owner.as_ref().is_some_and(|owner| owner.as_str() == graph.signer_did())),
                "persisted Goal request differs from its signed native graph or owner"
            );
        }
        anyhow::ensure!(
            matches!(owners.as_slice(), [owner] if owner.doc_id == existing_doc
            && owner.visible && owner.owner.as_ref().is_some_and(|owner| owner.as_str() == request.admission.signer_did)),
            "persisted Goal request differs from its native author"
        );
        (
            existing_doc.to_owned(),
            GoalBackedRequestDisposition::Idempotent,
        )
    } else {
        let request_doc = if let Some(graph) = client_graph {
            let session = request
                .session_doc_id
                .as_deref()
                .context("client Goal request has no physical session")?;
            let agent = request
                .agent_doc_id
                .as_deref()
                .context("client Goal request has no physical Agent")?;
            txn.stage_verified_edit_request(request, graph.clone(), session, agent)
                .await?
        } else {
            use gents_protocol::request_admission::AgentRequestAdmissionKind;
            match request.admission.kind {
                AgentRequestAdmissionKind::RuntimeInternal => {
                    crate::request::lifecycle::stage_runtime_request_in_txn(
                        txn,
                        request,
                        &session.doc_id,
                        &agent_doc,
                    )
                    .await?
                }
                AgentRequestAdmissionKind::Client | AgentRequestAdmissionKind::LocalSelf => {
                    let escaped = escape_graphql_string(&session.doc_id);
                    let response = txn.execute(&format!(
                        r#"{{ AgentSession(filter: {{_docID: {{_eq: "{escaped}"}}}}, limit: 2) {{relationships_pending}} }}"#
                    )).await?;
                    let rows = response["data"]["AgentSession"]
                        .as_array()
                        .context("goal session publication state unavailable")?;
                    let [row] = rows.as_slice() else {
                        anyhow::bail!("goal session publication state ambiguous")
                    };
                    if row["relationships_pending"] == true {
                        crate::request::lifecycle::stage_pending_child_request_in_txn(
                            txn,
                            request,
                            &session.doc_id,
                            &agent_doc,
                        )
                        .await?
                    } else {
                        crate::request::lifecycle::stage_client_request_in_txn(
                            txn,
                            request,
                            &session.doc_id,
                            &agent_doc,
                        )
                        .await?
                    }
                }
                _ => anyhow::bail!("unsupported Goal-backed request admission"),
            }
        };
        (request_doc, GoalBackedRequestDisposition::Created)
    };
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: ["{}"]) {{ {} }} }}"#,
            escape_graphql_string(&request_doc_id),
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("staged request lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "staged request is missing or ambiguous");
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
    let staged_request = gents_protocol::row::AgentRequest::try_from(row)?;
    let provenance = crate::request::lifecycle::materialize::created_session_provenance_in_txn(
        txn,
        &staged_request,
    )
    .await?;
    if reserved_runtime_session {
        crate::session::finalize_reserved_runtime_session_in_txn(
            txn,
            &session.doc_id,
            &staged_request,
            provenance.clone(),
        )
        .await?;
    }
    crate::session::ensure_session_in_txn(
        txn,
        session_id,
        node_did,
        &request.agent_did,
        Some(&request.requester_did),
        request.input.initial_title.clone(),
        provenance,
        &request.created_at,
    )
    .await?;
    let staged_goal = stage_goal_and_claim(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        GoalState {
            status: GoalStatus::Active,
            blocked_audits: 0,
            wrapup_requested: false,
            wrapup_completed: false,
        },
        &Utc::now().to_rfc3339(),
        identity,
        Some(&request_doc_id),
    )
    .await?;
    if disposition == GoalBackedRequestDisposition::Created {
        anyhow::ensure!(
            staged_goal.goal.parsed_status() == Some(GoalStatus::Active),
            "the session already has a non-active goal"
        );
    }
    bind_goal_first_request_in_txn(
        txn,
        &staged_goal.goal,
        &request_doc_id,
        request.input.goal_assignment.as_ref(),
    )
    .await?;
    let submission_state = goal_submission_step(
        goal_submission_step(
            GoalSubmissionState {
                durable_goal: false,
                runnable_request: false,
                staged_goal: false,
                staged_request: false,
            },
            GoalSubmissionAction::StageRequest,
        ),
        GoalSubmissionAction::StageGoal,
    );
    anyhow::ensure!(
        submission_state.staged_request,
        "created goal-backed request could not be staged"
    );
    Ok(StagedGoalBackedRequest {
        disposition,
        state: submission_state,
    })
}

pub async fn submit_goal_backed_request(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<GoalBackedRequestDisposition> {
    let result = access
        .transact_write("goal.submit_backed_request", move |txn| {
            Box::pin(async move {
                stage_goal_backed_request(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                    None,
                    None,
                    false,
                )
                .await
                .and_then(authorize_goal_submission_commit)
            })
        })
        .await;
    match result {
        Ok(disposition) => {
            super::creation::finish_goal_relationships(access, node_did, session_id).await?;
            Ok(disposition)
        }
        Err(error) => {
            let recovery = load_goal_backed_request_by_retry_key_from_access(
                access,
                node_did,
                session_id,
                objective,
                token_budget,
                request,
            )
            .await;
            let disposition = resolve_ambiguous_goal_submission_commit(error, recovery)?;
            super::creation::finish_goal_relationships(access, node_did, session_id).await?;
            Ok(disposition)
        }
    }
}

pub(crate) async fn stage_goal_backed_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request_with_identity_in_txn(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stage_goal_backed_request_with_identity_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
    client_graph: Option<&std::sync::Arc<db::block::builder::VerifiedClientCreate>>,
) -> Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        identity,
        client_graph,
        false,
    )
    .await
    .and_then(authorize_goal_submission_commit)?;
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;
    let staged = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id session_id }} }}"#,
            escape_graphql_string(retry_key),
        ))
        .await?;
    let rows = staged
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("staged goal request lookup omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "staged goal request is missing or ambiguous"
    );
    Ok(gents_protocol::request_lifecycle::EnqueuedAgentRequest {
        doc_id: rows[0]["_docID"]
            .as_str()
            .context("staged goal request lacks _docID")?
            .to_owned(),
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
    })
}

pub async fn submit_goal_backed_request_local(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
    submit_goal_backed_request_with_identity(
        node,
        actor,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit_goal_backed_request_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&dyn crate::store::identity::NodeIdentity>,
) -> Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
    let access = crate::ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        actor,
        identity.and_then(|identity| identity.managed_agent_signer()),
    )?;
    let bound_request = std::sync::Mutex::new(None);
    let bound_ref = &bound_request;
    let staged = access
        .transact_write("goal.submit_backed_request_local", move |txn| {
            Box::pin(async move {
                let mut create = request.clone();
                let reserved = if identity.is_some()
                    && matches!(create.admission.kind,
                    gents_protocol::request_admission::AgentRequestAdmissionKind::RuntimeInternal
                    | gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf)
                {
                    let identity = identity
                        .context("runtime Goal opening requires the selected Agent signer")?;
                    anyhow::ensure!(
                        txn.actor_did() == Some(identity.did())
                            && identity.did() == create.agent_did
                            && create.admission.signer_did == identity.did()
                            && txn
                                .managed_agent_signer()
                                .is_some_and(|signer| signer.did() == identity.did()),
                        "runtime Goal opening differs from its held Agent"
                    );
                    anyhow::ensure!(
                        crate::store::identity::verify_did_signature(
                            &create.admission.signer_did,
                            &create.signing_payload(),
                            &create.admission.signature,
                        )?,
                        "runtime Goal opening requires its original signed intent"
                    );
                    let (session, agent, created) =
                        crate::request::lifecycle::ensure_request_session_in_txn(
                            txn,
                            &create,
                            identity.did(),
                        )
                        .await?;
                    anyhow::ensure!(
                        create
                            .session_doc_id
                            .as_ref()
                            .is_none_or(|doc| doc == &session)
                            && create.agent_doc_id.as_ref().is_none_or(|doc| doc == &agent),
                        "runtime Goal opening signed different physical targets"
                    );
                    create.session_doc_id = Some(session);
                    create.agent_doc_id = Some(agent);
                    crate::sign_agent_request_create(identity, &mut create).await?;
                    created
                } else {
                    false
                };
                *bound_ref
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(create.clone());
                stage_goal_backed_request(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    &create,
                    None,
                    None,
                    reserved,
                )
                .await
                .and_then(authorize_goal_submission_commit)
            })
        })
        .await;
    let bound = bound_request
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner());
    let recovery_request = bound.as_ref().unwrap_or(request);
    match staged {
        Ok(_) => {}
        Err(error) => {
            if identity.is_some()
                && matches!(
                    request.admission.kind,
                    gents_protocol::request_admission::AgentRequestAdmissionKind::RuntimeInternal
                        | gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf
                )
                && bound.is_none()
            {
                return Err(error);
            }
            if let Some(recovered) = load_goal_backed_request_by_retry_key_from_access(
                &access,
                node_did,
                session_id,
                objective,
                token_budget,
                recovery_request,
            )
            .await?
            {
                super::creation::finish_goal_relationships(&access, node_did, session_id).await?;
                return Ok(recovered);
            }
            return Err(error);
        }
    }
    super::creation::finish_goal_relationships(&access, node_did, session_id).await?;
    load_goal_backed_request_by_retry_key_from_access(
        &access,
        node_did,
        session_id,
        objective,
        token_budget,
        recovery_request,
    )
    .await?
    .context("committed goal-backed request was not found by retry key")
}
