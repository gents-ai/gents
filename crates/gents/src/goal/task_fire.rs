use super::*;

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireIdentity {
    pub session_id: String,
    pub request_id: String,
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskGoalRequestBinding {
    #[serde(default)]
    pub node_did: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGoalFireRecoveryDisposition {
    Absent,
    Recovered,
    Conflict,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireRecoveryDecision {
    pub disposition: TaskGoalFireRecoveryDisposition,
    pub recovered_request_id: Option<String>,
    pub checkpointable: bool,
}

#[doc(hidden)]
pub fn decide_task_goal_fire_recovery(
    expected: &TaskGoalRequestBinding,
    observed: Option<&TaskGoalRequestBinding>,
) -> TaskGoalFireRecoveryDecision {
    match observed {
        None => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Absent,
            recovered_request_id: None,
            checkpointable: false,
        },
        Some(observed) if observed == expected => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Recovered,
            recovered_request_id: Some(observed.request_id.clone()),
            checkpointable: true,
        },
        Some(_) => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Conflict,
            recovered_request_id: None,
            checkpointable: false,
        },
    }
}

/// Length-prefixing matches `GoalAutomation.taskFireIdentity`.
#[doc(hidden)]
pub fn task_goal_fire_identity(
    node_did: &str,
    task_id: &str,
    fire_key: &str,
) -> TaskGoalFireIdentity {
    let scope = format!(
        "{}:{node_did}:{}:{task_id}:{}:{fire_key}",
        node_did.chars().count(),
        task_id.chars().count(),
        fire_key.chars().count()
    );
    TaskGoalFireIdentity {
        session_id: format!("task-goal-session:{scope}"),
        request_id: format!("task-goal-request:{scope}"),
        retry_key: format!("task-goal-retry:{scope}"),
    }
}

pub(crate) fn task_goal_fire_key<'a>(request_id: &'a str, expected_owner: &str) -> Option<&'a str> {
    fn component(input: &str) -> Option<(&str, &str)> {
        let (length, rest) = input.split_once(':')?;
        let length = length.parse::<usize>().ok()?;
        let end = rest
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(rest.len()))
            .nth(length)?;
        Some((&rest[..end], &rest[end..]))
    }
    let scope = request_id.strip_prefix("task-goal-request:")?;
    let (owner, tail) = component(scope)?;
    let (task, tail) = component(tail.strip_prefix(':')?)?;
    let (fire, tail) = component(tail.strip_prefix(':')?)?;
    if owner != expected_owner
        || !tail.is_empty()
        || task_goal_fire_identity(owner, task, fire).request_id != request_id
    {
        return None;
    }
    Some(fire)
}

#[doc(hidden)]
pub fn validate_task_goal_declaration(
    objective: Option<&str>,
    token_budget: Option<i64>,
) -> Result<()> {
    match objective {
        None => anyhow::ensure!(
            token_budget.is_none(),
            "goal token budget requires a goal objective template"
        ),
        Some(objective) => {
            anyhow::ensure!(
                !objective.trim().is_empty(),
                "goal objective must be non-empty"
            );
            anyhow::ensure!(
                token_budget.is_none_or(|budget| budget > 0),
                "goal token budget must be positive"
            );
        }
    }
    Ok(())
}

pub(crate) async fn apply_claimed_task_goal_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &crate::request::watcher::AgentRequest,
    now: &str,
) -> Result<()> {
    let owner = escape_graphql_string(&request.node_did);
    let request_id = escape_graphql_string(&request.request_id);
    let response = txn.execute_local_response(&format!(
        r#"{{ TriggerFire(filter: {{owner_did: {{_eq: "{owner}"}}, request_id: {{_eq: "{request_id}"}}}}) {{
            _docID session_id goal_id goal_objective goal_token_budget goal_assignment_applied
        }} }}"#,
    )).await?;
    #[derive(Deserialize)]
    struct Assignment {
        #[serde(rename = "_docID")]
        doc_id: String,
        session_id: String,
        goal_id: Option<String>,
        goal_objective: Option<String>,
        goal_token_budget: Option<i64>,
        #[serde(default)]
        goal_assignment_applied: bool,
    }
    let assignments: Vec<Assignment> = rows(&response, "TriggerFire")?;
    anyhow::ensure!(
        assignments.len() <= 1,
        "request has multiple Task fire receipts"
    );
    let Some(assignment) = assignments.into_iter().next() else {
        return Ok(());
    };
    if assignment.goal_id.is_none() || assignment.goal_assignment_applied {
        return Ok(());
    }
    anyhow::ensure!(
        assignment.session_id == request.session_id,
        "Task receipt session differs from claimed request"
    );
    validate_task_goal_identity_in_txn(
        txn,
        &request.node_did,
        &request.session_id,
        assignment
            .goal_id
            .as_deref()
            .context("Task assignment lost Goal identity")?,
        request.input.goal_assignment.as_ref(),
    )
    .await?;
    let objective = assignment
        .goal_objective
        .as_deref()
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .context("Goal-backed Task has no objective")?;
    anyhow::ensure!(
        assignment.goal_token_budget.is_none_or(|budget| budget > 0),
        "Task Goal budget must be positive"
    );
    let previous = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id).await?;
    if let Some(goal) = &previous {
        anyhow::ensure!(
            goal.parsed_status().is_some(),
            "existing Task Goal has an unknown status"
        );
        crate::triggers::engine::durable::publish_goal_outcomes(
            txn,
            &goal.node_did,
            &goal.goal_id,
            goal.last_blocked_reason.as_deref().unwrap_or(&goal.status),
            now,
        )
        .await?;
    }
    let was_active = previous
        .as_ref()
        .is_some_and(|goal| goal.parsed_status() == Some(GoalStatus::Active));
    let next_sequence = previous
        .as_ref()
        .map(|goal| {
            goal.continuation_sequence()
                .checked_add(1)
                .context("Task Goal continuation sequence exhausted")
        })
        .transpose()?
        .unwrap_or(0);
    if previous.is_none() || was_active {
        set_goal_in_txn(
            txn,
            &request.node_did,
            &request.session_id,
            Some(objective),
            Some(GoalStatus::Active),
            Some(assignment.goal_token_budget),
            None,
        )
        .await?;
    }
    let goal = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id)
        .await?
        .context("Task Goal assignment has no canonical row")?;
    let goal_doc_id = escape_graphql_string(&goal.doc_id);
    let expected_sequence = goal.continuation_sequence();
    let objective = escape_graphql_string(objective);
    let budget = optional_int_graphql_field("token_budget", assignment.goal_token_budget);
    let timestamp = escape_graphql_string(now);
    let assignment_root = escape_graphql_string(&request.doc_id);
    let reset = if was_active {
        String::new()
    } else {
        let baseline =
            session_token_usage_in_txn(txn, &request.node_did, &request.session_id).await?;
        format!(
            r#"tokens_used: 0, token_usage_baseline: {baseline},
            active_time_seconds: 0, active_started_at: "{timestamp}",
            consecutive_blocked_audits: 0, last_blocked_request_id: null,
            last_blocked_reason: null, last_continued_from_request_id: null,
            wrapup_requested: false, wrapup_completed: false,
            infrastructure_retry_count: 0, last_failure: null, completion_evidence: null,"#
        )
    };
    let response = txn
        .execute_local_response(&format!(
            r#"mutation {{
        update_Goal(filter: {{_docID: {{_eq: "{goal_doc_id}"}}, node_did: {{_eq: "{owner}"}},
            continuation_sequence: {{_eq: {expected_sequence}}}}}, input: {{
            objective: "{objective}", status: "active", {budget}
            assignment_root_request_doc_id: "{assignment_root}",
            continuation_sequence: {next_sequence}, {reset} updated_at: "{timestamp}"
        }}) {{_docID}}
    }}"#
        ))
        .await?;
    anyhow::ensure!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("update_Goal"))
            .is_some_and(mutation_returned_rows),
        "Task Goal assignment lost its continuation fence"
    );
    let updated = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id)
        .await?
        .context("assigned Task Goal disappeared")?;
    request_head::retire_stale_assignment_continuations_in_txn(txn, &updated, None, now).await?;
    let receipt_doc_id = escape_graphql_string(&assignment.doc_id);
    txn.execute(&format!(
        r#"mutation {{ update_TriggerFire(filter: {{_docID: {{_eq: "{receipt_doc_id}"}}}},
        input: {{goal_assignment_applied: true}}) {{_docID}} }}"#
    ))
    .await?;
    crate::triggers::engine::durable::publish_goal_outcomes(
        txn,
        &updated.node_did,
        &updated.goal_id,
        "Task Goal assignment replaced",
        now,
    )
    .await?;
    Ok(())
}
