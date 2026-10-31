use super::*;

pub async fn load_goals_for_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }}
                }},
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals)
}

pub async fn load_canonical_goal(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    Ok(load_goals_for_session(node, node_did, session_id)
        .await?
        .into_iter()
        .next())
}

pub async fn load_goal_by_id(
    node: &EmbeddedNode,
    node_did: &str,
    goal_id: &str,
) -> Result<Option<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let goal_id = escape_graphql_string(goal_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ node_did: {{ _eq: "{node_did}" }}, goal_id: {{ _eq: "{goal_id}" }} }},
                showDeleted: true,
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}

pub(crate) fn sort_goals_canonical(goals: &mut [GoalDocument]) {
    goals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.goal_id.cmp(&right.goal_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
}

async fn decode_goal_rows(node: &EmbeddedNode, query: &str) -> Result<Vec<GoalDocument>> {
    let response = graphql_with_transaction_retry(node, query, "Goal query").await?;
    rows(&response, "Goal").context("decoding Goal rows")
}

pub async fn load_canonical_goal_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{ Goal(filter: {{
        node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session_id}" }}
    }}) {{ {GOAL_FIELDS} }} }}"#
        ))
        .await?;
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .context("transactional Goal query omitted rows")?,
    )
    .context("decoding transactional Goal rows")?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}
