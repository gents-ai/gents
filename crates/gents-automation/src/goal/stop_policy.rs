use gents_protocol::goal::{GoalStopPolicy, OnBlocked};

/// Lean `GoalStopPolicy.Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedOutcome {
    Pause,
    Confirm,
    Continue,
}

impl BlockedOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Confirm => "confirm",
            Self::Continue => "continue",
        }
    }
}

/// Lean `GoalStopPolicy.onBlockedReport`. `confirming` holds when the
/// reporting turn is the confirmation turn a `confirm_once` report was given.
pub fn on_blocked_report(policy: OnBlocked, confirming: bool) -> BlockedOutcome {
    match (policy, confirming) {
        (OnBlocked::StopFirst, _) | (OnBlocked::ConfirmOnce, true) => BlockedOutcome::Pause,
        (OnBlocked::ConfirmOnce, false) => BlockedOutcome::Confirm,
        (OnBlocked::KeepTrying, _) => BlockedOutcome::Continue,
    }
}

/// Lean `GoalStopPolicy.effectiveBudget`: the smaller of the Goal's own
/// budget and the user's ceiling when both are set; a lifted policy enforces
/// the Goal's own budget only.
pub fn effective_goal_budget(policy: &GoalStopPolicy, goal_budget: Option<i64>) -> Option<i64> {
    let ceiling = policy.token_ceiling.filter(|_| !policy.lifted);
    match (goal_budget, ceiling) {
        (None, ceiling) => ceiling,
        (budget, None) => budget,
        (Some(budget), Some(ceiling)) => Some(budget.min(ceiling)),
    }
}

/// Lean `GoalStopPolicy.limitReached`: charged tokens reached the enforced
/// budget, or (unless lifted) goal turns reached the user's turn ceiling.
pub fn goal_limit_reached(
    policy: &GoalStopPolicy,
    goal_budget: Option<i64>,
    tokens_used: i64,
    turns: i64,
) -> bool {
    super::goal_budget_reached(effective_goal_budget(policy, goal_budget), tokens_used)
        || (!policy.lifted && policy.turn_ceiling.is_some_and(|ceiling| ceiling <= turns))
}

const POLICY_FIELDS: &str =
    "_docID goal_doc_id agent_did on_blocked token_ceiling turn_ceiling lifted";

fn decode_policy(row: &serde_json::Value) -> GoalStopPolicy {
    GoalStopPolicy {
        on_blocked: row["on_blocked"]
            .as_str()
            .and_then(OnBlocked::parse)
            .unwrap_or_default(),
        token_ceiling: row["token_ceiling"].as_i64(),
        turn_ceiling: row["turn_ceiling"].as_i64(),
        lifted: row["lifted"].as_bool().unwrap_or(false),
    }
}

/// The stop policy row of the Goal `goal_doc_id`, read by its unique key so
/// the transaction's read is fenced by the database. `None` when the Goal has
/// no policy document; its decisions then use the default policy.
async fn load_policy_row_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    goal_doc_id: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let response = txn
        .execute_with_variables(
            &format!(
                "query($goal: String!) {{ GoalStopPolicy(filter: {{goal_doc_id: {{_eq: $goal}}}}, limit: 2) {{ {POLICY_FIELDS} }} }}"
            ),
            &serde_json::json!({ "goal": goal_doc_id }),
        )
        .await?;
    let rows = response["data"]["GoalStopPolicy"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("GoalStopPolicy lookup omitted its rows"))?;
    anyhow::ensure!(rows.len() < 2, "a Goal has at most one stop policy");
    Ok(rows.first().cloned())
}

/// The Goal's stop policy, or the default when it has none.
pub async fn load_goal_stop_policy_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    goal_doc_id: &str,
) -> anyhow::Result<GoalStopPolicy> {
    Ok(load_policy_row_in_txn(txn, goal_doc_id)
        .await?
        .as_ref()
        .map(decode_policy)
        .unwrap_or_default())
}

/// The Goal a stop policy belongs to, and the working Agent's Agent document
/// that is the policy's parent.
pub struct PolicyTarget<'a> {
    pub goal_doc_id: &'a str,
    pub session_doc_id: &'a str,
    pub agent_did: &'a str,
    pub agent_doc_id: &'a str,
    pub node_did: &'a str,
}

/// Write `policy` as the Goal's stop policy within `txn`, creating the
/// document under the working Agent's Agent document when the Goal has none.
/// Authorization is ACP's: only the parent's `session_admin` creates or
/// updates it (`goal_policies`).
pub async fn write_goal_stop_policy_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    target: &PolicyTarget<'_>,
    policy: &GoalStopPolicy,
    updated_at: &str,
) -> anyhow::Result<()> {
    let goal_doc_id = target.goal_doc_id;
    let fields = serde_json::json!({
        "on_blocked": policy.on_blocked.as_str(),
        "token_ceiling": policy.token_ceiling,
        "turn_ceiling": policy.turn_ceiling,
        "lifted": policy.lifted,
        "updated_at": updated_at,
    });
    let schema = gents_protocol::schemas::GOAL_STOP_POLICY;
    match load_policy_row_in_txn(txn, goal_doc_id).await? {
        Some(row) => {
            let doc_id = row["_docID"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("GoalStopPolicy row has no document ID"))?;
            txn.execute_client_signed_update(
                "mutation($id: ID!, $input: GoalStopPolicyMutationInputArg!) { update_GoalStopPolicy(docID: $id, input: $input) { _docID } }",
                &serde_json::json!({ "id": doc_id, "input": fields }),
                schema,
            )
            .await?;
        }
        None => {
            let mut input = fields;
            input["goal_doc_id"] = goal_doc_id.into();
            input["_agentID"] = target.agent_doc_id.into();
            input["session_doc_id"] = target.session_doc_id.into();
            input["agent_did"] = target.agent_did.into();
            input["node_did"] = target.node_did.into();
            let created = txn
                .execute_client_signed_create(
                    "mutation($input: GoalStopPolicyMutationInputArg!) { create_GoalStopPolicy(input: $input) { _docID } }",
                    &serde_json::json!({ "input": input }),
                    schema,
                )
                .await?;
            let doc = gents_store::graphql::created_doc_id(&created, "GoalStopPolicy")?;
            txn.add_actor_relationship(
                "GoalStopPolicy",
                &doc,
                "reader",
                &identity::Did::new(target.agent_did)?,
            )
            .await?;
        }
    }
    Ok(())
}
