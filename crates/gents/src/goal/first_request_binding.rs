use super::*;
use crate::config_client::ConfigApplyTxn;
use gents_protocol::request_input::GoalAssignmentInput;

/// Reading the canonical row here puts its binding in the storage transaction's
/// conflict set, including a Goal created earlier in this same transaction.
pub(crate) async fn bind_goal_first_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    request_doc_id: &str,
    reference: Option<&GoalAssignmentInput>,
) -> Result<()> {
    let observed = load_canonical_goal_in_txn(txn, &goal.node_did, &goal.session_id)
        .await?
        .context("Goal opening disappeared during publication")?;
    anyhow::ensure!(
        observed.doc_id == goal.doc_id && observed.goal_id == goal.goal_id,
        "Goal opening identity changed during publication"
    );
    let goal = &observed;
    anyhow::ensure!(
        !goal.goal_id.is_empty()
            && !request_doc_id.is_empty()
            && reference.is_none_or(|reference| {
                reference.goal_id == goal.goal_id && reference.root_request_doc_id.is_none()
            })
            && goal
                .assignment_root_request_doc_id
                .as_deref()
                .is_none_or(|root| root == request_doc_id),
        "Goal first request conflicts with its assignment binding"
    );
    if goal.assignment_root_request_doc_id.is_some() {
        return Ok(());
    }
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{ _docID: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}, assignment_root_request_doc_id: {{_eq: null}} }},
        input: {{ assignment_root_request_doc_id: "{}" }}) {{_docID}} }}"#,
            escape_graphql_string(&goal.doc_id),
            escape_graphql_string(&goal.node_did),
            escape_graphql_string(request_doc_id),
        ))
        .await?;
    anyhow::ensure!(
        response["data"]["update_Goal"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "Goal first request binding changed during publication"
    );
    Ok(())
}

pub fn task_assignment_goal_id(current: Option<&str>, node_did: &str, session_id: &str) -> String {
    current
        .map(str::to_owned)
        .unwrap_or_else(|| deterministic_goal_id(node_did, session_id))
}

pub(crate) async fn validate_task_goal_identity_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    goal_id: &str,
    reference: Option<&GoalAssignmentInput>,
) -> Result<()> {
    let current = load_canonical_goal_in_txn(txn, node_did, session_id).await?;
    anyhow::ensure!(
        goal_id
            == task_assignment_goal_id(
                current.as_ref().map(|goal| goal.goal_id.as_str()),
                node_did,
                session_id
            )
            && reference
                .is_none_or(|reference| reference.goal_id == goal_id
                    && reference.root_request_doc_id.is_none()),
        "Task Goal assignment changed before publication or claim"
    );
    Ok(())
}

#[cfg(test)]
mod contract_tests;
