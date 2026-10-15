mod recovery;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use anyhow::{ensure, Result};
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
pub(crate) use recovery::recover_outcomes;
#[cfg(test)]
pub(crate) use recovery::recover_outcomes_in_txn;

#[derive(Clone)]
pub(crate) struct PreparedFire {
    pub receipt: TriggerFire,
    pub target_existing: bool,
}

pub(crate) fn fire_key(identity: &FireIdentity) -> String {
    identity.fire_key()
}

pub(crate) fn resolve_session_id(
    identity: &FireIdentity,
    target: Option<&str>,
    owned: bool,
) -> Option<String> {
    match target {
        None => Some(identity.session_id()),
        Some(value) if !value.is_empty() && owned => Some(value.to_owned()),
        Some(_) => None,
    }
}

pub(crate) struct GoalOutcomeBinding {
    pub owner_did: String,
    pub session_id: String,
    pub assignment_request_id: String,
    pub status: String,
}

pub(crate) fn fire_outcome_reason(
    fire: &TriggerFire,
    request_terminal: bool,
    assignment_replaced: bool,
    goal: Option<&GoalOutcomeBinding>,
) -> Option<String> {
    if !fire.emit_outcome {
        return None;
    }
    if fire.goal_id.is_none() || !fire.goal_assignment_applied {
        return request_terminal.then(|| "request_terminal".into());
    }
    if assignment_replaced {
        return Some("superseded".into());
    }
    let goal = goal.filter(|goal| {
        goal.owner_did == fire.identity.owner_did
            && goal.session_id == fire.session_id
            && goal.assignment_request_id == fire.request_id
    })?;
    matches!(
        goal.status.as_str(),
        "complete" | "blocked" | "budget_limited"
    )
    .then(|| goal.status.clone())
}

async fn observe_goal_outcome_binding(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
) -> Result<Option<GoalOutcomeBinding>> {
    let Some(expected_goal_id) = &fire.goal_id else {
        return Ok(None);
    };
    let Some(goal) =
        crate::goal::load_canonical_goal_in_txn(txn, &fire.identity.owner_did, &fire.session_id)
            .await?
    else {
        return Ok(None);
    };
    ensure!(
        &goal.goal_id == expected_goal_id,
        "Task fire Goal identity changed during outcome observation"
    );
    let Some(assignment_request_id) = crate::goal::assignment_request_id_in_txn(txn, &goal).await?
    else {
        return Ok(None);
    };
    Ok(Some(GoalOutcomeBinding {
        owner_did: goal.node_did,
        session_id: goal.session_id,
        assignment_request_id,
        status: goal.status,
    }))
}

fn assignment_replaced(fire: &TriggerFire, binding: Option<&GoalOutcomeBinding>) -> bool {
    fire.goal_assignment_applied
        && binding.is_some_and(|goal| {
            goal.owner_did == fire.identity.owner_did
                && goal.session_id == fire.session_id
                && goal.assignment_request_id != fire.request_id
        })
}

#[cfg(test)]
pub(crate) async fn stage_fire_request(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
    request_mutation: &str,
) -> Result<bool> {
    if !stage_fire_receipt(txn, fire).await? {
        return Ok(false);
    }
    txn.execute(request_mutation).await?;
    Ok(true)
}

pub(crate) fn outcome_source_allowed(source_collection: &str, emit_outcome: bool) -> bool {
    source_collection != "FireOutcome" || !emit_outcome
}

pub(crate) async fn stage_fire_receipt(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
) -> Result<bool> {
    ensure!(
        outcome_source_allowed(&fire.identity.source_collection, fire.emit_outcome),
        "a Task sourced from FireOutcome cannot emit another FireOutcome"
    );
    ensure!(
        fire.fire_key == fire_key(&fire.identity),
        "noncanonical fire identity"
    );
    ensure!(
        fire.request_id == fire.identity.request_id(),
        "noncanonical fire request ID"
    );
    let prior = txn
        .execute(&format!(
            "{{ TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}) {{ request_id }} }}",
            escape_graphql_string(&fire.fire_key)
        ))
        .await?;
    if prior["data"]["TriggerFire"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(false);
    }
    txn.execute_with_variables("mutation($input: TriggerFireMutationInputArg!) { create_TriggerFire(input: $input) { _docID } }", &serde_json::json!({"input":fire})).await?;
    Ok(true)
}

/// Called in the terminal owner's transaction. Recovery uses the same unique
/// outcome key; an acknowledgement lost after commit cannot extend the chain.
pub(crate) async fn stage_outcome(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
    goal: Option<&GoalOutcomeBinding>,
    assignment_replaced: bool,
    request_terminal: bool,
    terminal_state: &str,
    reason: &str,
    now: &str,
) -> Result<bool> {
    let Some(classification) =
        fire_outcome_reason(fire, request_terminal, assignment_replaced, goal)
    else {
        return Ok(false);
    };
    let terminal_state = if classification == "request_terminal" {
        terminal_state
    } else {
        &classification
    };
    let reason = if classification == "superseded" {
        "superseded by a newer Task Goal assignment"
    } else {
        reason
    };
    ensure!(
        fire.fire_key == fire_key(&fire.identity),
        "noncanonical outcome fire identity"
    );
    let handoff_id = fire.identity.outcome_id();
    let prior = txn
        .execute(&format!(
            "{{ FireOutcome(filter: {{handoff_id: {{_eq: \"{}\"}}}}) {{ handoff_id }} }}",
            escape_graphql_string(&handoff_id)
        ))
        .await?;
    if prior["data"]["FireOutcome"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(false);
    }
    let source_handoff_id = fire
        .source_handoff_id
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("outcome-enabled fire lacks its source handoff identity"))?;
    let outcome = gents_protocol::trigger_delivery::FireOutcome {
        handoff_id,
        fire_key: fire.fire_key.clone(),
        identity: fire.identity.clone(),
        request_id: fire.request_id.clone(),
        session_id: fire.session_id.clone(),
        goal_id: fire.goal_id.clone(),
        terminal_state: terminal_state.into(),
        reason: reason.into(),
        source_handoff_id: source_handoff_id.clone(),
        reply_session_id: fire.reply_session_id.clone(),
        shard_id: fire.shard_id.clone(),
        attempt: fire.attempt,
        created_at: now.into(),
    };
    txn.execute_with_variables("mutation($input: FireOutcomeMutationInputArg!) { create_FireOutcome(input: $input) { _docID } }", &serde_json::json!({"input":outcome})).await?;
    Ok(true)
}

const FIRE_FIELDS: &str = "fire_key owner_did trigger_id source_collection source_doc_id task_id request_id session_id goal_id goal_objective goal_token_budget goal_assignment_applied emit_outcome queued_serial source_handoff_id reply_session_id shard_id attempt created_at";

pub(crate) async fn publish_request_outcome(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    request_id: &str,
    terminal_state: &str,
    reason: &str,
    now: &str,
) -> Result<()> {
    let response = txn.execute(&format!("{{ TriggerFire(filter: {{owner_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}) {{ {FIRE_FIELDS} }} }}", escape_graphql_string(owner), escape_graphql_string(request_id))).await?;
    let rows: Vec<TriggerFire> = serde_json::from_value(response["data"]["TriggerFire"].clone())?;
    for fire in rows {
        stage_outcome(txn, &fire, None, false, true, terminal_state, reason, now).await?;
    }
    Ok(())
}

pub(crate) async fn publish_goal_outcomes(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    goal_id: &str,
    reason: &str,
    now: &str,
) -> Result<()> {
    let response = txn.execute(&format!("{{ TriggerFire(filter: {{owner_did: {{_eq: \"{}\"}}, goal_id: {{_eq: \"{}\"}}}}) {{ {FIRE_FIELDS} }} }}", escape_graphql_string(owner), escape_graphql_string(goal_id))).await?;
    let rows: Vec<TriggerFire> = serde_json::from_value(response["data"]["TriggerFire"].clone())?;
    for fire in rows {
        let binding = observe_goal_outcome_binding(txn, &fire).await?;
        stage_outcome(
            txn,
            &fire,
            binding.as_ref(),
            assignment_replaced(&fire, binding.as_ref()),
            false,
            "",
            reason,
            now,
        )
        .await?;
    }
    Ok(())
}

/// Read the native first-arrival journal in the caller's claim snapshot.
/// Missing arrivals are not replaced with content-ID or wall-clock ordering.
pub async fn request_arrival_order(
    txn: &ConfigApplyTxn<'_>,
    doc_ids: &[String],
) -> Result<Vec<String>> {
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let slots = crate::lifecycle::queue::effective_slots(txn, doc_ids).await?;
    if slots.is_empty() {
        return Ok(Vec::new());
    }
    let slot_ids = slots
        .iter()
        .map(|(_, slot)| slot.as_str())
        .collect::<std::collections::HashSet<_>>();
    let ids = crate::graphql::graphql_string_list_literal(slot_ids.into_iter());
    let mut cursor = "0".to_owned();
    let mut ordered = Vec::new();
    loop {
        let response = txn.execute(&format!("{{ _documentArrivals(collection: \"AgentRequest\", after: \"{}\", limit: 256, docID: {ids}) {{ head next entries {{ cursor docID }} }} }}", escape_graphql_string(&cursor))).await?;
        let page = &response["data"]["_documentArrivals"];
        let entries = page["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("arrival query omitted entries"))?;
        for entry in entries {
            ordered.push(
                entry["docID"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("arrival entry omitted docID"))?
                    .to_owned(),
            );
        }
        let next = page["next"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("arrival query omitted next cursor"))?;
        let head = page["head"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("arrival query omitted snapshot head"))?;
        if next == head || entries.is_empty() {
            break;
        }
        ensure!(next != cursor, "arrival cursor did not advance");
        cursor = next.into();
    }
    let positions = ordered
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut slots = slots
        .into_iter()
        .filter(|(_, slot)| positions.contains_key(slot.as_str()))
        .collect::<Vec<_>>();
    slots.sort_by_key(|(_, slot)| positions[slot.as_str()]);
    Ok(slots.into_iter().map(|(id, _)| id).collect())
}

#[derive(Clone, serde::Deserialize)]
pub(crate) struct ClaimObservation {
    pub document: String,
    pub owner: String,
    pub session: String,
    pub trigger: String,
    pub serial: bool,
    pub receipt: bool,
    pub arrival: Option<u64>,
    pub running: bool,
    pub terminal: bool,
}

pub(crate) fn claim_conflict(candidate: &ClaimObservation, other: &ClaimObservation) -> bool {
    candidate.owner == other.owner
        && (candidate.session == other.session
            || (candidate.serial
                && candidate.receipt
                && other.receipt
                && candidate.trigger == other.trigger))
}

pub(crate) fn observed_claim_allowed(
    candidate: &ClaimObservation,
    rows: &[ClaimObservation],
) -> bool {
    !candidate.running
        && !candidate.terminal
        && !(candidate.receipt && candidate.arrival.is_none())
        && !rows.iter().any(|other| {
            other.document != candidate.document
                && !other.terminal
                && claim_conflict(candidate, other)
                && ((other.receipt && other.arrival.is_none())
                    || other.running
                    || match (candidate.arrival, other.arrival) {
                        (Some(_), None) => true,
                        (Some(next), Some(prior)) => prior < next,
                        (None, _) => false,
                    })
        })
}
