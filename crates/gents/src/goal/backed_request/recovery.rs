use super::*;

fn goal_creation_claim_selection(node_did: &str, session_id: &str, session_doc_id: &str) -> String {
    format!(
        r#"Goal(
            filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, session_doc_id: {{ _eq: "{}" }} }},
            order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
        ) {{ {GOAL_FIELDS} }}
        GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }}, session_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{
            goal_id node_did session_id objective token_budget
        }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
        escape_graphql_string(session_doc_id),
        escape_graphql_string(&deterministic_goal_creation_key(node_did, session_id)),
        escape_graphql_string(session_doc_id),
    )
}

fn goal_backed_request_recovery_query(
    node_did: &str,
    session_id: &str,
    session_doc_id: &str,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<String> {
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;
    Ok(format!(
        r#"{{
            AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}, limit: 2) {{
                {SIGNED_REQUEST_FIELDS}
            }}
            {}
        }}"#,
        escape_graphql_string(retry_key),
        goal_creation_claim_selection(node_did, session_id, session_doc_id),
    ))
}

fn decode_agent_request_by_retry_key(
    rows: &[serde_json::Value],
    expected: &GoalBackedRequestFingerprint,
) -> Result<Option<String>> {
    anyhow::ensure!(rows.len() <= 1, "AgentRequest retry key is not unique");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let persisted: GoalBackedRequestFingerprint =
        serde_json::from_value(row.clone()).context("decoding AgentRequest fingerprint")?;
    anyhow::ensure!(
        persisted == *expected,
        "AgentRequest retry key conflicts with a different immutable request"
    );
    let receipt: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
    crate::store::identity::request_receipt::verify_request_receipt_signature(&receipt)?;
    row.get("_docID")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .context("AgentRequest matched by retry key has no document ID")
        .map(Some)
}

fn verify_goal_creation_claim_cross_check(
    response: &serde_json::Value,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<()> {
    let expected_claim = GoalCreationFingerprint {
        owner: node_did.to_string(),
        session: session_id.to_string(),
        objective: objective.trim().to_string(),
        token_budget: token_budget.map(i128::from),
    };
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding committed goal-backed request Goal rows")?;
    sort_goals_canonical(&mut goals);
    let goal = goals
        .first()
        .context("goal-backed request exists without its durable goal")?;
    let claims = response
        .pointer("/data/GoalCreationClaim")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        claims.len() <= 1,
        "goal-backed request creation key is not unique"
    );
    let claim = claims
        .first()
        .context("goal-backed request exists without its durable goal claim")?;
    let claim_goal_id = claim
        .get("goal_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let claim = GoalCreationFingerprint {
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
    anyhow::ensure!(
        goal.node_did == node_did
            && goal.session_id == session_id
            && goal.objective.trim() == expected_claim.objective
            && goal.token_budget == token_budget
            && claim_goal_id == goal.goal_id
            && claim == expected_claim,
        "goal-backed request conflicts with its durable goal claim"
    );
    Ok(())
}

fn decode_goal_backed_request_recovery(
    response: &serde_json::Value,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<gents_protocol::request_lifecycle::EnqueuedAgentRequest>> {
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let expected = GoalBackedRequestFingerprint::from_create(request)?;
    let Some(doc_id) = decode_agent_request_by_retry_key(&rows, &expected)? else {
        return Ok(None);
    };
    verify_goal_creation_claim_cross_check(
        response,
        node_did,
        session_id,
        objective,
        token_budget,
    )?;
    Ok(Some(
        gents_protocol::request_lifecycle::EnqueuedAgentRequest {
            doc_id,
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
        },
    ))
}

pub(super) async fn load_goal_backed_request_by_retry_key_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<gents_protocol::request_lifecycle::EnqueuedAgentRequest>> {
    access
        .transact_readonly("goal.backed_request.recover", |txn| {
            Box::pin(async move {
                let Some(session) =
                    crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                        .await?
                else {
                    return Ok(None);
                };
                let query = goal_backed_request_recovery_query(
                    node_did,
                    session_id,
                    &session.doc_id,
                    request,
                )?;
                let response = txn.execute(&query).await?;
                let recovered = decode_goal_backed_request_recovery(
                    &response,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                )?;
                if let Some(found) = &recovered {
                    let owners = txn.document_owners("AgentRequest", std::slice::from_ref(&found.doc_id)).await?;
                    anyhow::ensure!(matches!(owners.as_slice(), [owner] if owner.doc_id == found.doc_id
                        && owner.visible && owner.owner.as_ref().is_some_and(|owner| owner.as_str() == request.admission.signer_did)),
                        "recovered Goal request differs from its native author");
                }
                Ok(recovered)
            })
        })
        .await
}
