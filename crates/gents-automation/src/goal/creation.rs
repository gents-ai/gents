use super::*;

/// Stage the two native grants for a Goal or claim created by this transaction.
pub(super) async fn stage_goal_relationships_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    session_doc_id: &str,
    agent_did: &str,
) -> Result<()> {
    txn.add_resource_relationship(collection, doc_id, "parent", "sessions", session_doc_id)
        .await?;
    txn.add_actor_relationship(
        collection,
        doc_id,
        "working_agent",
        &identity::Did::new(agent_did)?,
    )
    .await?;
    Ok(())
}

pub(super) async fn create_goal_child_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    input: serde_json::Value,
    session_doc_id: &str,
    agent_did: &str,
) -> Result<String> {
    let schema = match collection {
        "Goal" => gents_protocol::schemas::GOAL,
        "GoalCreationClaim" => gents_protocol::schemas::GOAL_CREATION_CLAIM,
        _ => anyhow::bail!("unsupported Goal child collection"),
    };
    anyhow::ensure!(
        input["parent_linked"] == true
            && input["session_doc_id"].as_str() == Some(session_doc_id)
            && input["agent_did"].as_str() == Some(agent_did)
            && input["creator_did"].as_str() == txn.actor_did(),
        "Goal child publication binding differs from its transaction"
    );
    let mutation = format!("mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}");
    let created = txn
        .execute_client_signed_create(&mutation, &serde_json::json!({"input": input}), schema)
        .await?;
    let doc = gents_store::graphql::created_doc_id(&created, collection)?;
    stage_goal_relationships_in_txn(txn, collection, &doc, session_doc_id, agent_did).await?;
    Ok(doc)
}

/// Repair retained incomplete publications only for their native creator. The
/// marker and both grants commit together, so completed rows never replay a
/// later relationship revocation.
pub(crate) async fn finish_goal_relationships(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> Result<()> {
    let actor = access
        .actor_did()
        .context("Goal publication requires an authenticated actor")?;
    access.transact_write("goal.repair_relationships", |txn| {
        let actor = &actor;
        Box::pin(async move {
            let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                .await?.context("Goal publication session is unavailable")?;
            let parent = escape_graphql_string(&session.doc_id);
            let creator = escape_graphql_string(actor);
            let response = {
                use gents_protocol::document_query::{
                    fields::{goal, goal_creation_claim},
                    Filter, Query, QuerySet, Selection,
                };
                let rendered = QuerySet::new()
                    .plain(
                        &Query::select(Selection::doc_id().field(goal::AGENT_DID))
                            .filter(Filter::eq(goal::SESSION_DOC_ID, &session.doc_id))
                            .filter(Filter::eq(goal::CREATOR_DID, actor))
                            .filter(Filter::eq(goal::PARENT_LINKED, false)),
                    )
                    .plain(
                        &Query::select(Selection::doc_id().field(goal_creation_claim::AGENT_DID))
                            .filter(Filter::eq(goal_creation_claim::SESSION_DOC_ID, &session.doc_id))
                            .filter(Filter::eq(goal_creation_claim::CREATOR_DID, actor))
                            .filter(Filter::eq(goal_creation_claim::PARENT_LINKED, false)),
                    )
                    .render();
                txn.run(&rendered).await?
            };
            for (collection, schema) in [
                ("Goal", gents_protocol::schemas::GOAL),
                ("GoalCreationClaim", gents_protocol::schemas::GOAL_CREATION_CLAIM),
            ] {
                let rows = response["data"][collection].as_array().context("Goal relationship recovery omitted rows")?;
                for row in rows {
                    anyhow::ensure!(row["agent_did"] == session.session.agent_did, "Goal relationship selects another agent");
                    let doc = row["_docID"].as_str().context("Goal has no document ID")?;
                    let owners = txn.document_owners(collection, &[doc.to_owned()]).await?;
                    anyhow::ensure!(owners.len() == 1 && owners[0].doc_id == doc && owners[0].visible
                        && owners[0].owner.as_ref().is_some_and(|owner| owner.as_str() == actor.as_str()),
                        "Goal relationship repair requires its native creator");
                    stage_goal_relationships_in_txn(txn, collection, doc, &session.doc_id, &session.session.agent_did).await?;
                    let escaped_doc = escape_graphql_string(doc);
                    let mutation = format!(r#"mutation($input: {collection}MutationInputArg!) {{
                        update_{collection}(filter: {{_docID: {{_eq: "{escaped_doc}"}}, creator_did: {{_eq: "{creator}"}}, session_doc_id: {{_eq: "{parent}"}}, parent_linked: {{_eq: false}}}}, input: $input) {{_docID}}
                    }}"#);
                    let changed = txn.execute_client_signed_update(&mutation, &serde_json::json!({"input": {"parent_linked": true}}), schema).await?;
                    let changed_rows = changed["data"][format!("update_{collection}")].as_array().context("Goal relationship marker omitted rows")?;
                    anyhow::ensure!(changed_rows.len() == 1 && changed_rows[0]["_docID"] == doc,
                        "Goal relationship marker lost its binding");
                }
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await
}

pub(crate) async fn load_goal_creation_claim_fingerprint_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    load_goal_creation_claim_for_key_in_txn(txn, node_did, session_id, None).await
}

/// The claim under `creation_key`, or under the session's deterministic key.
pub(crate) async fn load_goal_creation_claim_for_key_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    creation_key: Option<&str>,
) -> Result<Option<GoalCreationFingerprint>> {
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?
    .context("goal claim requires a visible physical session")?;
    // Read through the transaction's own backend: client creates reconcile a
    // lost race over HTTP, where an embedded-only response is unavailable.
    let response = txn
        .execute(&goal_creation_claim_query(
            creation_key
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| deterministic_goal_creation_key(node_did, session_id)),
            &session.doc_id,
        ))
        .await?;
    goal_creation_claim_fingerprint(&serde_json::from_value(response)?)
}

fn goal_creation_claim_query(creation_key: String, session_doc_id: &str) -> String {
    let key = escape_graphql_string(&creation_key);
    let session_doc_id = escape_graphql_string(session_doc_id);
    format!(
        r#"{{ GoalCreationClaim(filter: {{ session_doc_id: {{ _eq: "{session_doc_id}" }}, creation_key: {{ _eq: "{key}" }} }}, limit: 2) {{
            node_did session_id objective token_budget
        }} }}"#
    )
}

fn goal_creation_claim_fingerprint(
    response: &defra_node::QueryResponse,
) -> Result<Option<GoalCreationFingerprint>> {
    #[derive(Deserialize)]
    struct ClaimRow {
        node_did: String,
        session_id: String,
        objective: String,
        token_budget: Option<i64>,
    }
    let claims: Vec<ClaimRow> = rows(response, "GoalCreationClaim")?;
    anyhow::ensure!(
        claims.len() <= 1,
        "goal creation key resolved to multiple claims"
    );
    Ok(claims
        .into_iter()
        .next()
        .map(|claim| GoalCreationFingerprint {
            owner: claim.node_did,
            session: claim.session_id,
            objective: claim.objective,
            token_budget: claim.token_budget.map(i128::from),
        }))
}

#[derive(Debug, Clone)]
pub enum CreateGoalForSessionOutcome {
    Created(GoalDocument),
    Idempotent(GoalDocument),
}

impl CreateGoalForSessionOutcome {
    pub fn goal(&self) -> &GoalDocument {
        match self {
            Self::Created(goal) | Self::Idempotent(goal) => goal,
        }
    }

    pub fn disposition(&self) -> &'static str {
        match self {
            Self::Created(_) => "created",
            Self::Idempotent(_) => "idempotent",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateGoalForSessionError {
    #[error("goal objective must be non-empty")]
    InvalidObjective,
    #[error("goal token budget must be positive")]
    InvalidBudget,
    #[error("the current session already has a goal with a different objective or budget")]
    Conflict,
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

impl From<gents_store::config_client::ConfigClientError> for CreateGoalForSessionError {
    fn from(error: gents_store::config_client::ConfigClientError) -> Self {
        Self::Storage(anyhow::Error::new(error))
    }
}

impl gents_store::config_client::TransactionFailure for CreateGoalForSessionError {
    fn config_client_error(&self) -> Option<&gents_store::config_client::ConfigClientError> {
        match self {
            Self::Storage(error) => {
                gents_store::config_client::TransactionFailure::config_client_error(error)
            }
            Self::InvalidObjective | Self::InvalidBudget | Self::Conflict => None,
        }
    }

    fn rerun_requested(&self) -> Option<&gents_store::config_client::RerunNote> {
        match self {
            Self::Storage(error) => {
                gents_store::config_client::TransactionFailure::rerun_requested(error)
            }
            Self::InvalidObjective | Self::InvalidBudget | Self::Conflict => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedGoalDisposition {
    Created,
    Idempotent,
}

#[derive(Debug, Clone)]
pub(super) struct StagedGoal {
    disposition: StagedGoalDisposition,
    pub(super) goal: GoalDocument,
}

#[doc(hidden)]
pub struct GoalCreationIdentity {
    pub goal_id: String,
    pub creation_key: String,
}

/// IDs may contain separators; each component is framed by its UTF-8 byte
/// length so the client and transaction owner name the same fresh assignment.
#[doc(hidden)]
pub fn replacement_goal_identity(
    node_did: &str,
    session_id: &str,
    command_id: &str,
) -> GoalCreationIdentity {
    let scope = format!(
        "{}:{node_did}:{}:{session_id}:{}:{command_id}",
        node_did.len(),
        session_id.len(),
        command_id.len()
    );
    GoalCreationIdentity {
        goal_id: format!("goal-assignment:{scope}"),
        creation_key: format!("goal-assignment-create:{scope}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stage_goal_and_claim(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    initial_state: GoalState,
    now: &str,
    identity: Option<&GoalCreationIdentity>,
    first_request_doc_id: Option<&str>,
) -> Result<StagedGoal> {
    stage_goal_and_claim_typed(
        txn,
        GoalStaging {
            node_did,
            session_id,
            objective,
            token_budget,
            initial_state,
            now,
            identity,
            first_request_doc_id,
        },
    )
    .await
    .map_err(|error| match error {
        CreateGoalForSessionError::Storage(error) => error,
        refusal => anyhow::Error::new(refusal),
    })
}

struct GoalStaging<'a> {
    node_did: &'a str,
    session_id: &'a str,
    objective: &'a str,
    token_budget: Option<i64>,
    initial_state: GoalState,
    now: &'a str,
    identity: Option<&'a GoalCreationIdentity>,
    first_request_doc_id: Option<&'a str>,
}

async fn stage_goal_and_claim_typed(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    staging: GoalStaging<'_>,
) -> std::result::Result<StagedGoal, CreateGoalForSessionError> {
    let GoalStaging {
        node_did,
        session_id,
        objective,
        token_budget,
        initial_state,
        now,
        identity,
        first_request_doc_id,
    } = staging;
    let create_request = GoalCreateRequest {
        caller: node_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: node_did.to_string(),
        requested_session: session_id.to_string(),
        objective: objective.to_string(),
        objective_nonempty: !objective.is_empty(),
        token_budget: token_budget.map(i128::from),
        goal_tools: true,
        goal_create: true,
    };
    if decide_model_goal_create(&create_request, None) != GoalCreateDisposition::Fresh {
        return Err(anyhow::anyhow!("invalid goal objective or token budget").into());
    }
    let fingerprint = goal_creation_fingerprint(&create_request);
    let objective = fingerprint.objective.as_str();
    let goal_id = identity
        .map(|identity| identity.goal_id.clone())
        .unwrap_or_else(|| deterministic_goal_id(node_did, session_id));
    let creation_key = identity
        .map(|identity| identity.creation_key.clone())
        .unwrap_or_else(|| deterministic_goal_creation_key(node_did, session_id));
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?
    .context("goal creation requires a visible physical session")?;
    let creator = txn
        .actor_did()
        .context("goal creation requires an authenticated actor")?;
    let response = {
        use gents_protocol::document_query::{
            fields::{goal, goal_creation_claim},
            Filter, Query, QuerySet, Selection,
        };
        let rendered = QuerySet::new()
            .plain(
                &Query::select(Selection::owned(GOAL_FIELDS))
                    .filter(Filter::eq(goal::NODE_DID, node_did))
                    .filter(Filter::eq(goal::SESSION_ID, session_id))
                    .filter(Filter::eq(goal::SESSION_DOC_ID, &session.doc_id)),
            )
            .plain(
                &Query::select(
                    Selection::new()
                        .field(goal_creation_claim::GOAL_ID)
                        .field(goal_creation_claim::NODE_DID)
                        .field(goal_creation_claim::SESSION_ID)
                        .field(goal_creation_claim::OBJECTIVE)
                        .field(goal_creation_claim::TOKEN_BUDGET),
                )
                .filter(Filter::eq(
                    goal_creation_claim::SESSION_DOC_ID,
                    &session.doc_id,
                ))
                .filter(Filter::eq(goal_creation_claim::CREATION_KEY, &creation_key)),
            )
            .render();
        txn.run(&rendered).await?
    };
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding transactional Goal rows")?;
    sort_goals_canonical(&mut goals);
    let claims = response
        .pointer("/data/GoalCreationClaim")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if claims.len() > 1 {
        return Err(anyhow::anyhow!("goal creation key resolved to multiple claims").into());
    }
    if let Some(claim) = claims.first() {
        let claim_fingerprint = GoalCreationFingerprint {
            owner: claim
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            session: claim
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            objective: claim
                .get("objective")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            token_budget: claim
                .get("token_budget")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from),
        };
        if claim.get("goal_id").and_then(serde_json::Value::as_str) != Some(goal_id.as_str())
            || decide_model_goal_create(&create_request, Some(&claim_fingerprint))
                != GoalCreateDisposition::Idempotent
        {
            return Err(CreateGoalForSessionError::Conflict);
        }
    } else {
        create_goal_child_in_txn(
            txn,
            "GoalCreationClaim",
            serde_json::json!({
                "creation_key": creation_key,
                "goal_id": goal_id,
                "node_did": node_did,
                "session_id": session_id,
                "session_doc_id": session.doc_id,
                "agent_did": session.session.agent_did,
                "creator_did": creator,
                "parent_linked": true,
                "requester_did": session.session.requester_did,
                "objective": objective,
                "token_budget": token_budget,
                "created_at": now,
            }),
            &session.doc_id,
            &session.session.agent_did,
        )
        .await?;
    }

    if let Some(goal) = goals.first().cloned() {
        if !goals.iter().all(|candidate| {
            candidate.objective.trim() == objective && candidate.token_budget == token_budget
        }) {
            return Err(CreateGoalForSessionError::Conflict);
        }
        return Ok(StagedGoal {
            disposition: StagedGoalDisposition::Idempotent,
            goal,
        });
    }

    let status = initial_state.status;
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    let doc_id = create_goal_child_in_txn(
        txn,
        "Goal",
        serde_json::json!({
            "goal_id": goal_id,
            "creation_key": creation_key,
            "session_id": session_id,
            "node_did": node_did,
            "session_doc_id": session.doc_id,
            "agent_did": session.session.agent_did,
            "creator_did": creator,
            "parent_linked": true,
            "requester_did": session.session.requester_did,
            "objective": objective,
            "status": status.as_str(),
            "token_budget": token_budget,
            "tokens_used": 0,
            "token_usage_baseline": baseline,
            "assignment_root_request_doc_id": first_request_doc_id,
            "active_time_seconds": 0,
            "active_started_at": status.accrues_active_time().then_some(now),
            "consecutive_blocked_audits": initial_state.blocked_audits,
            "continuation_sequence": 0,
            "wrapup_requested": initial_state.wrapup_requested,
            "wrapup_completed": initial_state.wrapup_completed,
            "infrastructure_retry_count": 0,
            "created_at": now,
            "updated_at": now,
        }),
        &session.doc_id,
        &session.session.agent_did,
    )
    .await?;
    let staged = StagedGoal {
        disposition: StagedGoalDisposition::Created,
        goal: GoalDocument {
            doc_id,
            goal_id,
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            objective: objective.to_string(),
            status: status.as_str().to_string(),
            token_budget,
            tokens_used: Some(0),
            token_usage_baseline: Some(baseline),
            assignment_root_request_doc_id: first_request_doc_id.map(str::to_owned),
            active_time_seconds: Some(0),
            active_started_at: status.accrues_active_time().then(|| now.to_string()),
            consecutive_blocked_audits: Some(initial_state.blocked_audits),
            last_blocked_request_id: None,
            last_blocked_reason: None,
            last_continued_from_request_id: None,
            continuation_sequence: Some(0),
            wrapup_requested: Some(initial_state.wrapup_requested),
            wrapup_completed: Some(initial_state.wrapup_completed),
            infrastructure_retry_count: Some(0),
            last_failure: None,
            completion_evidence: None,
            auto_resume_at_reset: None,
            created_at: Some(now.to_string()),
            updated_at: Some(now.to_string()),
        },
    };
    Ok(staged)
}

/// Create through the authenticated actor and signer bound to this access.
/// Ownership is deliberately not accepted as model input: both values come
/// from the authenticated session hook.
pub async fn create_goal_for_session_from_access(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let actor = access.actor_did().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!(
            "Goal creation requires an authenticated actor"
        ))
    })?;
    create_goal_for_session_inner(
        access,
        &actor,
        node_did,
        session_id,
        objective,
        token_budget,
        None,
    )
    .await
}

pub async fn create_goal_for_request_from_access(
    node: &std::sync::Arc<EmbeddedNode>,
    access: Option<&gents_store::config_client::ConfigAccess>,
    caller: &gents_protocol::row::AgentRequestRow,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let owner = caller.node_did.as_deref().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!("Goal tool caller has no owner"))
    })?;
    let session = caller.session_id.as_deref().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!("Goal tool caller has no session"))
    })?;
    let actor = caller.agent_did.as_deref().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!(
            "Goal tool caller has no selected agent"
        ))
    })?;
    let actor_identity = identity::Did::new(actor)
        .map_err(|error| CreateGoalForSessionError::Storage(error.into()))?;
    let fallback_access =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), actor_identity);
    create_goal_for_session_inner(
        access.unwrap_or(&fallback_access),
        actor,
        owner,
        session,
        objective,
        token_budget,
        Some(caller),
    )
    .await
}

async fn create_goal_for_session_inner(
    access: &gents_store::config_client::ConfigAccess,
    actor_did: &str,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    caller: Option<&gents_protocol::row::AgentRequestRow>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let create_request = GoalCreateRequest {
        caller: node_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: node_did.to_string(),
        requested_session: session_id.to_string(),
        objective: objective.to_string(),
        objective_nonempty: !objective.is_empty(),
        token_budget: token_budget.map(i128::from),
        goal_tools: true,
        goal_create: true,
    };
    match decide_model_goal_create(&create_request, None) {
        GoalCreateDisposition::Invalid if objective.trim().is_empty() => {
            return Err(CreateGoalForSessionError::InvalidObjective);
        }
        GoalCreateDisposition::Invalid => return Err(CreateGoalForSessionError::InvalidBudget),
        GoalCreateDisposition::Fresh => {}
        disposition => unreachable!("owned authorized create preflight returned {disposition:?}"),
    }
    let requested_fingerprint = goal_creation_fingerprint(&create_request);
    let objective = requested_fingerprint.objective.as_str();

    if access.actor_did().as_deref() != Some(actor_did) {
        return Err(CreateGoalForSessionError::Storage(anyhow::anyhow!(
            "Goal tool actor differs from transaction identity"
        )));
    }
    let result = access.transact_write(
        "goal.create_for_session",
        move |txn| {
            Box::pin(async move {
                let context = if let Some(caller) = caller {
                    let context = tool_context::load_goal_tool_context_in_txn(
                        txn, node_did, session_id,
                        &caller.request_id,
                        caller.doc_id.as_deref().context("Goal tool caller has no physical identity")?,
                    ).await?;
                    if !context.can_create() {
                        return Err(CreateGoalForSessionError::Storage(anyhow::anyhow!("Goal creation requires an unassigned caller with no current Goal, or the exact current assignment")));
                    }
                    Some(context)
                } else { None };
                let identity = if let Some(tool_context::GoalToolContext { assignment: RequestAssignmentObservation::Assigned(goal), .. }) = &context {
                    let response = {
                        use gents_protocol::document_query::{fields::goal as fields, Filter, Query, Selection};
                        txn.run(&Query::select(Selection::new().field(fields::CREATION_KEY)).filter(Filter::eq(fields::DOC_ID, &goal.doc_id)).render()).await?
                    };
                    let creation_key = response.pointer("/data/Goal/0/creation_key").and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty()).context("assigned Goal has no creation identity")?;
                    Some(GoalCreationIdentity { goal_id: goal.goal_id.clone(), creation_key: creation_key.to_string() })
                } else { None };
                let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                    .await?
                    .context("goal creation requires a visible physical session")?;
                let actor = crate::operations::goal::acting_in_txn(txn, node_did, &session.session.agent_did).await?;
                if let Err(refusal) = crate::operations::goal::admit_start_in_txn(
                    txn, actor, node_did, session_id, objective, token_budget, None,
                    identity.as_ref().map(|identity| identity.creation_key.as_str()),
                ).await? {
                    use gents_protocol::operations::goal::GoalOperationError;
                    return Err(match refusal {
                        GoalOperationError::Conflict => CreateGoalForSessionError::Conflict,
                        GoalOperationError::InvalidObjective => CreateGoalForSessionError::InvalidObjective,
                        GoalOperationError::InvalidBudget => CreateGoalForSessionError::InvalidBudget,
                        refusal => CreateGoalForSessionError::Storage(anyhow::Error::new(refusal)),
                    });
                }
                let mut staged = stage_goal_and_claim_typed(
                    txn,
                    GoalStaging {
                        node_did,
                        session_id,
                        objective,
                        token_budget,
                        initial_state: GoalState {
                            status: GoalStatus::Active,
                            blocked_audits: 0,
                            wrapup_requested: false,
                            wrapup_completed: false,
                        },
                        now: &Utc::now().to_rfc3339(),
                        identity: identity.as_ref(),
                        first_request_doc_id: context.as_ref().and_then(|context| context.request.doc_id.as_deref()),
                    },
                ).await?;
                if let Some(context) = context {
                    if matches!(staged.disposition, StagedGoalDisposition::Created) {
                        let goal = load_canonical_goal_in_txn(txn, node_did, session_id).await?
                            .context("created Goal row disappeared before caller binding")?;
                        bind_goal_first_request_in_txn(txn, &goal,
                            context.request.doc_id.as_deref().context("Goal tool caller has no physical identity")?,
                            context.request.input.as_ref().and_then(|input| input.goal_assignment.as_ref()),
                        ).await?;
                        staged.goal = load_canonical_goal_in_txn(txn, node_did, session_id).await?
                            .context("bound Goal row disappeared")?;
                    }
                }
                Ok::<_, CreateGoalForSessionError>(staged)
            })
        },
    )
    .await;

    match result {
        Ok(outcome) => {
            finish_goal_relationships(access, node_did, session_id)
                .await
                .map_err(CreateGoalForSessionError::Storage)?;
            let goal = outcome.goal;
            Ok(match outcome.disposition {
                StagedGoalDisposition::Created => CreateGoalForSessionOutcome::Created(goal),
                StagedGoalDisposition::Idempotent => CreateGoalForSessionOutcome::Idempotent(goal),
            })
        }
        Err(error) if caller.is_some() => Err(error),
        Err(error) => {
            let (existing, claim) = access
                .transact_readonly("goal.reconcile_creation", |txn| {
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>((
                            load_canonical_goal_in_txn(txn, node_did, session_id).await?,
                            load_goal_creation_claim_fingerprint_in_txn(txn, node_did, session_id)
                                .await?,
                        ))
                    })
                })
                .await
                .map_err(CreateGoalForSessionError::Storage)?;
            if let Some(existing) = existing {
                if existing.objective.trim() == objective
                    && existing.token_budget == token_budget
                    && claim == Some(requested_fingerprint.clone())
                {
                    finish_goal_relationships(access, node_did, session_id)
                        .await
                        .map_err(CreateGoalForSessionError::Storage)?;
                    return Ok(CreateGoalForSessionOutcome::Idempotent(existing));
                }
                return Err(CreateGoalForSessionError::Conflict);
            }
            Err(error)
        }
    }
}
