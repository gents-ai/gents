use super::*;

pub(crate) async fn load_goal_creation_claim_fingerprint(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    let query = goal_creation_claim_query(node_did, session_id);
    let response = graphql_with_transaction_retry(node, &query, "load goal creation claim").await?;
    goal_creation_claim_fingerprint(&response)
}

pub(crate) async fn load_goal_creation_claim_fingerprint_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    let response = txn
        .execute_local_response(&goal_creation_claim_query(node_did, session_id))
        .await?;
    goal_creation_claim_fingerprint(&response)
}

fn goal_creation_claim_query(node_did: &str, session_id: &str) -> String {
    let key = escape_graphql_string(&deterministic_goal_creation_key(node_did, session_id));
    format!(
        r#"{{ GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{key}" }} }}, limit: 2) {{
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    initial_state: GoalState,
    now: &str,
    identity: Option<&GoalCreationIdentity>,
) -> Result<StagedGoal> {
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
    anyhow::ensure!(
        decide_model_goal_create(&create_request, None) == GoalCreateDisposition::Fresh,
        "invalid goal objective or token budget"
    );
    let fingerprint = goal_creation_fingerprint(&create_request);
    let objective = fingerprint.objective.as_str();
    let goal_id = identity
        .map(|identity| identity.goal_id.clone())
        .unwrap_or_else(|| deterministic_goal_id(node_did, session_id));
    let creation_key = identity
        .map(|identity| identity.creation_key.clone())
        .unwrap_or_else(|| deterministic_goal_creation_key(node_did, session_id));
    let escaped_did = escape_graphql_string(node_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{
                Goal(filter: {{
                    node_did: {{ _eq: "{escaped_did}" }},
                    session_id: {{ _eq: "{escaped_session}" }}
                }}) {{ {GOAL_FIELDS} }}
                GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}) {{
                    goal_id node_did session_id objective token_budget
                }}
            }}"#,
            escape_graphql_string(&creation_key),
        ))
        .await?;
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
    anyhow::ensure!(
        claims.len() <= 1,
        "goal creation key resolved to multiple claims"
    );
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
            return Err(anyhow::Error::new(CreateGoalForSessionError::Conflict));
        }
    } else {
        let budget_field = optional_int_graphql_field("token_budget", token_budget);
        txn.execute(&format!(
            r#"mutation {{ create_GoalCreationClaim(input: {{
                creation_key: "{}", goal_id: "{}",
                node_did: "{escaped_did}", session_id: "{escaped_session}",
                objective: "{}", {budget_field} created_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&creation_key),
            escape_graphql_string(&goal_id),
            escape_graphql_string(objective),
            escape_graphql_string(now),
        ))
        .await?;
    }

    if let Some(goal) = goals.first().cloned() {
        if !goals.iter().all(|candidate| {
            candidate.objective.trim() == objective && candidate.token_budget == token_budget
        }) {
            return Err(anyhow::Error::new(CreateGoalForSessionError::Conflict));
        }
        return Ok(StagedGoal {
            disposition: StagedGoalDisposition::Idempotent,
            goal,
        });
    }

    let status = initial_state.status;
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now),
    );
    let created = txn.execute(&format!(
        r#"mutation {{ create_Goal(input: {{
            goal_id: "{}", creation_key: "{}",
            session_id: "{escaped_session}", node_did: "{escaped_did}",
            objective: "{}", status: "{}", {}
            tokens_used: 0, token_usage_baseline: {baseline}, active_time_seconds: 0, {active_started_field}
            consecutive_blocked_audits: {}, continuation_sequence: 0,
            wrapup_requested: {}, wrapup_completed: {},
            infrastructure_retry_count: 0, created_at: "{}", updated_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&goal_id),
        escape_graphql_string(&creation_key),
        escape_graphql_string(objective),
        status.as_str(),
        optional_int_graphql_field("token_budget", token_budget),
        initial_state.blocked_audits,
        initial_state.wrapup_requested,
        initial_state.wrapup_completed,
        escape_graphql_string(now),
        escape_graphql_string(now),
    ))
    .await?;
    let doc_id = crate::store::graphql::created_doc_id(&created, "Goal")?;
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
            assignment_root_request_doc_id: None,
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

/// Ownership is deliberately not accepted as model input: both values come
/// from the authenticated session hook.
pub async fn create_goal_for_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    create_goal_for_session_inner(node, node_did, session_id, objective, token_budget, None).await
}

pub(crate) async fn create_goal_for_request(
    node: &std::sync::Arc<EmbeddedNode>,
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
    create_goal_for_session_inner(node, owner, session, objective, token_budget, Some(caller)).await
}

async fn create_goal_for_session_inner(
    node: &std::sync::Arc<EmbeddedNode>,
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

    let result = crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.create_for_session",
        move |txn| {
            Box::pin(async move {
                let context = if let Some(caller) = caller {
                    let context = tool_context::load_goal_tool_context_in_txn(
                        txn, node_did, session_id,
                        &caller.request_id,
                        caller.doc_id.as_deref().context("Goal tool caller has no physical identity")?,
                    ).await?;
                    anyhow::ensure!(context.can_create(), "Goal creation requires an unassigned caller with no current Goal, or the exact current assignment");
                    Some(context)
                } else { None };
                let identity = if let Some(tool_context::GoalToolContext { assignment: RequestAssignmentObservation::Assigned(goal), .. }) = &context {
                    let response = txn.execute(&format!(r#"{{ Goal(filter: {{ _docID: {{_eq: "{}"}} }}) {{creation_key}} }}"#,
                        escape_graphql_string(&goal.doc_id))).await?;
                    let creation_key = response.pointer("/data/Goal/0/creation_key").and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty()).context("assigned Goal has no creation identity")?;
                    Some(GoalCreationIdentity { goal_id: goal.goal_id.clone(), creation_key: creation_key.to_string() })
                } else { None };
                let mut staged = stage_goal_and_claim(
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
                    identity.as_ref(),
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
                Ok(staged)
            })
        },
    )
    .await;

    match result {
        Ok(outcome) => {
            let goal = if caller.is_some() {
                outcome.goal
            } else {
                load_canonical_goal(node, node_did, session_id)
                    .await
                    .map_err(CreateGoalForSessionError::Storage)?
                    .context("committed Goal row not found")
                    .map_err(CreateGoalForSessionError::Storage)?
            };
            Ok(match outcome.disposition {
                StagedGoalDisposition::Created => CreateGoalForSessionOutcome::Created(goal),
                StagedGoalDisposition::Idempotent => CreateGoalForSessionOutcome::Idempotent(goal),
            })
        }
        Err(error) if caller.is_some() => match error.downcast::<CreateGoalForSessionError>() {
            Ok(error) => Err(error),
            Err(error) => Err(CreateGoalForSessionError::Storage(error)),
        },
        Err(error) => {
            if let Some(existing) = load_canonical_goal(node, node_did, session_id)
                .await
                .map_err(CreateGoalForSessionError::Storage)?
            {
                if existing.objective.trim() == objective
                    && existing.token_budget == token_budget
                    && load_goal_creation_claim_fingerprint(node, node_did, session_id)
                        .await
                        .map_err(CreateGoalForSessionError::Storage)?
                        == Some(requested_fingerprint.clone())
                {
                    return Ok(CreateGoalForSessionOutcome::Idempotent(existing));
                }
                return Err(CreateGoalForSessionError::Conflict);
            }
            match error.downcast::<CreateGoalForSessionError>() {
                Ok(error) => Err(error),
                Err(error) => Err(CreateGoalForSessionError::Storage(error)),
            }
        }
    }
}
