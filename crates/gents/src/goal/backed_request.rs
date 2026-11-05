use super::*;

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
    recovery: Result<Option<crate::request::lifecycle::materialize::EnqueuedAgentRequest>>,
) -> Result<GoalBackedRequestDisposition> {
    match recovery {
        Ok(Some(_)) => Ok(GoalBackedRequestDisposition::Idempotent),
        Ok(None) => Err(commit_error),
        Err(recovery_error) => Err(anyhow::anyhow!(
            "goal-backed request recovery failed after ambiguous commit: {recovery_error:#}; original commit error: {commit_error:#}"
        )),
    }
}

async fn stage_goal_backed_request(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
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
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;

    let existing = txn
        .execute(&format!(
            r#"{{
                    AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{ _docID
                        {GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS}
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
        (
            existing_request["_docID"]
                .as_str()
                .context("persisted Goal request lacks physical identity")?
                .to_owned(),
            GoalBackedRequestDisposition::Idempotent,
        )
    } else {
        let request_fields = request.graphql_input_fields().map_err(anyhow::Error::msg)?;
        let created = txn
            .execute(&format!(
                "mutation {{ create_AgentRequest(input: {{ {request_fields} }}) {{ _docID }} }}"
            ))
            .await?;
        (
            crate::store::graphql::created_doc_id(&created, "AgentRequest")?,
            GoalBackedRequestDisposition::Created,
        )
    };
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: ["{}"]) {{ {} }} }}"#,
            escape_graphql_string(&request_doc_id),
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("staged request lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "staged request is missing or ambiguous");
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
    let staged_request = crate::request::watcher::AgentRequest::try_from(row)?;
    let provenance = crate::request::lifecycle::materialize::created_session_provenance_in_txn(
        txn,
        &staged_request,
    )
    .await?;
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
) -> Result<crate::request::lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request_with_identity_in_txn(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        None,
    )
    .await
}

pub(crate) async fn stage_goal_backed_request_with_identity_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
) -> Result<crate::request::lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        identity,
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
    Ok(crate::request::lifecycle::EnqueuedAgentRequest {
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
) -> Result<crate::request::lifecycle::EnqueuedAgentRequest> {
    let access = crate::ConfigAccess::local_as(node.clone(), actor.clone());
    let staged = crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "goal.submit_backed_request_local",
        move |txn| {
            Box::pin(async move {
                stage_goal_backed_request(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                    None,
                )
                .await
                .and_then(authorize_goal_submission_commit)
            })
        },
    )
    .await;
    match staged {
        Ok(_) => {}
        Err(error) => {
            if let Some(recovered) = load_goal_backed_request_by_retry_key_from_access(
                &access,
                node_did,
                session_id,
                objective,
                token_budget,
                request,
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
        request,
    )
    .await?
    .context("committed goal-backed request was not found by retry key")
}
