mod recovery;
#[cfg(test)]
mod retained_tests;
use crate::config_client::{ConfigApplyTxn, ConfigClientError, TransactionFailure};
use crate::graphql::escape_graphql_string;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
pub(crate) use recovery::recover_outcomes;
#[cfg(test)]
pub(crate) use recovery::recover_outcomes_in_txn;

type Result<T, E = DurableTriggerError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum DurableTriggerError {
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Goal(#[from] crate::goal::GoalError),
    #[error(transparent)]
    GoalHistory(#[from] crate::goal::GoalHistoryError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("a Task sourced from FireOutcome cannot emit another FireOutcome")]
    OutcomeSourceNotAllowed,
    #[error("noncanonical fire identity")]
    NoncanonicalFireIdentity,
    #[error("noncanonical fire request ID")]
    NoncanonicalRequestId,
    #[error("noncanonical outcome fire identity")]
    NoncanonicalOutcomeIdentity,
    #[error("outcome-enabled fire lacks its source handoff identity")]
    OutcomeSourceHandoffMissing,
    #[error("Task outcome query omitted rows")]
    OutcomeRowsOmitted,
    #[error(transparent)]
    Queue(Box<crate::lifecycle::queue::QueueError>),
    #[error("arrival query omitted entries")]
    ArrivalEntriesMissing,
    #[error("arrival entry omitted cursor")]
    ArrivalCursorMissing,
    #[error("arrival entry has invalid cursor")]
    ArrivalCursorInvalid(#[source] std::num::ParseIntError),
    #[error("arrival entry omitted docID")]
    ArrivalDocIdMissing,
    #[error("arrival query omitted next cursor")]
    ArrivalNextMissing,
    #[error("arrival query omitted snapshot head")]
    ArrivalHeadMissing,
    #[error("arrival cursor did not advance")]
    ArrivalCursorStalled,
    #[error("Task fire has no unique admitted request during outcome recovery")]
    TaskFireHasNoUniqueAdmittedRequest,
    #[error("Task request is missing lifecycle state")]
    TaskRequestIsMissingLifecycleState,
    #[error("Task assignment request query omitted rows")]
    TaskAssignmentRequestQueryOmittedRows,
    #[error("Task assignment has no unique physical request in its owner/session")]
    TaskAssignmentHasNoUniquePhysicalRequest,
    #[error("Task assignment request has no physical identity")]
    TaskAssignmentRequestHasNoPhysicalIdentity,
    #[error("Task assignment has ambiguous physical Goal history")]
    TaskAssignmentHasAmbiguousPhysicalGoalHistory,
    #[error("Task assignment root is reused by a different Goal identity")]
    TaskAssignmentRootIsReusedByA,
}

impl TransactionFailure for DurableTriggerError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Queue(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::GoalHistory(error) => error.config_client_error(),
            Self::Graphql(error) => error.config_client_error(),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct PreparedFire {
    pub receipt: TriggerFire,
    pub target_existing: bool,
    pub replace_prior: bool,
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

#[derive(Clone)]
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
    goal_outcome_terminal(&goal.status).then(|| goal.status.clone())
}

fn goal_outcome_terminal(status: &str) -> bool {
    matches!(
        status,
        "complete" | "blocked" | "budget_limited" | "stopped"
    )
}

async fn observe_goal_outcome_binding(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
) -> Result<Option<GoalOutcomeBinding>> {
    let Some(expected_goal_id) = &fire.goal_id else {
        return Ok(None);
    };
    let current =
        crate::goal::load_canonical_goal_in_txn(txn, &fire.identity.owner_did, &fire.session_id)
            .await?;
    let current_binding = if let Some(goal) = &current {
        crate::goal::assignment_request_id_in_txn(txn, goal)
            .await?
            .map(|assignment_request_id| GoalOutcomeBinding {
                owner_did: goal.node_did.clone(),
                session_id: goal.session_id.clone(),
                assignment_request_id,
                status: goal.status.clone(),
            })
    } else {
        None
    };
    if current
        .as_ref()
        .is_some_and(|goal| &goal.goal_id == expected_goal_id)
        && current_binding
            .as_ref()
            .is_some_and(|binding| binding.assignment_request_id == fire.request_id)
    {
        return Ok(current_binding);
    }
    let response = txn.execute(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
        escape_graphql_string(&fire.identity.owner_did), escape_graphql_string(&fire.session_id),
        escape_graphql_string(&fire.request_id)
    )).await?;
    let requests = response["data"]["AgentRequest"]
        .as_array()
        .ok_or(DurableTriggerError::TaskAssignmentRequestQueryOmittedRows)?;
    if !(requests.len() == 1) {
        return Err(DurableTriggerError::TaskAssignmentHasNoUniquePhysicalRequest);
    }
    let root = requests[0]["_docID"]
        .as_str()
        .ok_or(DurableTriggerError::TaskAssignmentRequestHasNoPhysicalIdentity)?;
    let versions = crate::goal::load_goal_versions_for_session(
        txn,
        &fire.identity.owner_did,
        &fire.session_id,
    )
    .await?;
    let goals = crate::goal::latest_assignment_goals(&versions)?;
    let retained = goals
        .iter()
        .filter(|goal| {
            &goal.goal_id == expected_goal_id
                && goal.assignment_root_request_doc_id.as_deref() == Some(root)
        })
        .collect::<Vec<_>>();
    if !(retained.len() <= 1) {
        return Err(DurableTriggerError::TaskAssignmentHasAmbiguousPhysicalGoalHistory);
    }
    let retained_binding = retained.first().map(|goal| GoalOutcomeBinding {
        owner_did: goal.node_did.clone(),
        session_id: goal.session_id.clone(),
        assignment_request_id: fire.request_id.clone(),
        status: goal.status.clone(),
    });
    if !(current_binding
        .as_ref()
        .is_none_or(|binding| binding.assignment_request_id != fire.request_id))
    {
        return Err(DurableTriggerError::TaskAssignmentRootIsReusedByA);
    }
    Ok(select_goal_outcome_binding(
        fire,
        retained_binding.as_ref(),
        current_binding.as_ref(),
    ))
}

pub(super) fn select_goal_outcome_binding(
    fire: &TriggerFire,
    retained: Option<&GoalOutcomeBinding>,
    current: Option<&GoalOutcomeBinding>,
) -> Option<GoalOutcomeBinding> {
    let retained = retained.filter(|goal| {
        goal.owner_did == fire.identity.owner_did
            && goal.session_id == fire.session_id
            && goal.assignment_request_id == fire.request_id
    })?;
    if goal_outcome_terminal(&retained.status) {
        return Some(retained.clone());
    }
    Some(
        current
            .filter(|goal| {
                goal.owner_did == fire.identity.owner_did && goal.session_id == fire.session_id
            })
            .unwrap_or(retained)
            .clone(),
    )
}

pub(super) fn assignment_replaced(
    fire: &TriggerFire,
    binding: Option<&GoalOutcomeBinding>,
) -> bool {
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
    if !outcome_source_allowed(&fire.identity.source_collection, fire.emit_outcome) {
        return Err(DurableTriggerError::OutcomeSourceNotAllowed);
    }
    if fire.fire_key != fire_key(&fire.identity) {
        return Err(DurableTriggerError::NoncanonicalFireIdentity);
    }
    if fire.request_id != fire.identity.request_id() {
        return Err(DurableTriggerError::NoncanonicalRequestId);
    }
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

/// Called in the terminal owner's transaction; an acknowledgement lost after
/// commit cannot extend the chain because recovery reuses the unique outcome key.
pub(crate) struct OutcomeStage<'a> {
    pub(crate) goal: Option<&'a GoalOutcomeBinding>,
    pub(crate) assignment_replaced: bool,
    pub(crate) request_terminal: bool,
    pub(crate) terminal_state: &'a str,
    pub(crate) reason: &'a str,
}

pub(crate) async fn stage_outcome(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
    stage: OutcomeStage<'_>,
    now: &str,
) -> Result<bool> {
    let OutcomeStage {
        goal,
        assignment_replaced,
        request_terminal,
        terminal_state,
        reason,
    } = stage;
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
    if fire.fire_key != fire_key(&fire.identity) {
        return Err(DurableTriggerError::NoncanonicalOutcomeIdentity);
    }
    let handoff_id = fire.identity.outcome_id();
    if outcome_published(txn, fire).await? {
        return Ok(false);
    }
    let source_handoff_id = fire
        .source_handoff_id
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or(DurableTriggerError::OutcomeSourceHandoffMissing)?;
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

async fn outcome_published(txn: &ConfigApplyTxn<'_>, fire: &TriggerFire) -> Result<bool> {
    let response = txn
        .execute(&format!(
            "{{ FireOutcome(filter: {{handoff_id: {{_eq: \"{}\"}}}}) {{ handoff_id }} }}",
            escape_graphql_string(&fire.identity.outcome_id())
        ))
        .await?;
    let rows = response["data"]["FireOutcome"]
        .as_array()
        .ok_or(DurableTriggerError::OutcomeRowsOmitted)?;
    Ok(!rows.is_empty())
}

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
        let stage = OutcomeStage {
            goal: None,
            assignment_replaced: false,
            request_terminal: true,
            terminal_state,
            reason,
        };
        stage_outcome(txn, &fire, stage, now).await?;
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
        if outcome_published(txn, &fire).await? {
            continue;
        }
        let binding = observe_goal_outcome_binding(txn, &fire).await?;
        stage_outcome(
            txn,
            &fire,
            OutcomeStage {
                goal: binding.as_ref(),
                assignment_replaced: assignment_replaced(&fire, binding.as_ref()),
                request_terminal: false,
                terminal_state: "",
                reason,
            },
            now,
        )
        .await?;
    }
    Ok(())
}

/// Missing arrivals are not replaced with content-ID or wall-clock ordering.
pub async fn request_arrival_order(
    txn: &ConfigApplyTxn<'_>,
    doc_ids: &[String],
) -> Result<Vec<String>> {
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    const DOC_ID_BATCH_SIZE: usize = 512;
    let mut slots = Vec::new();
    for batch in doc_ids.chunks(DOC_ID_BATCH_SIZE) {
        slots.extend(
            crate::lifecycle::queue::effective_slots(txn, batch)
                .await
                .map_err(|error| DurableTriggerError::Queue(Box::new(error)))?,
        );
    }
    if slots.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen_slots = rapidhash::RapidHashSet::default();
    let slot_ids = slots
        .iter()
        .filter_map(|(_, slot)| seen_slots.insert(slot.as_str()).then_some(slot.as_str()))
        .collect::<Vec<_>>();
    let mut ordered = Vec::<(u64, String)>::new();
    for batch in slot_ids.chunks(DOC_ID_BATCH_SIZE) {
        let ids = crate::graphql::graphql_string_list_literal(batch.iter().copied());
        let mut cursor = "0".to_owned();
        loop {
            let response = txn.execute(&format!("{{ _documentArrivals(collection: \"AgentRequest\", after: \"{}\", limit: 256, docID: {ids}) {{ head next entries {{ cursor docID }} }} }}", escape_graphql_string(&cursor))).await?;
            let page = &response["data"]["_documentArrivals"];
            let entries = page["entries"]
                .as_array()
                .ok_or(DurableTriggerError::ArrivalEntriesMissing)?;
            for entry in entries {
                let arrival = entry["cursor"]
                    .as_str()
                    .ok_or(DurableTriggerError::ArrivalCursorMissing)?
                    .parse::<u64>()
                    .map_err(DurableTriggerError::ArrivalCursorInvalid)?;
                ordered.push((
                    arrival,
                    entry["docID"]
                        .as_str()
                        .ok_or(DurableTriggerError::ArrivalDocIdMissing)?
                        .to_owned(),
                ));
            }
            let next = page["next"]
                .as_str()
                .ok_or(DurableTriggerError::ArrivalNextMissing)?;
            let head = page["head"]
                .as_str()
                .ok_or(DurableTriggerError::ArrivalHeadMissing)?;
            if next == head || entries.is_empty() {
                break;
            }
            if next == cursor {
                return Err(DurableTriggerError::ArrivalCursorStalled);
            }
            cursor = next.into();
        }
    }
    ordered.sort_unstable_by_key(|(cursor, _)| *cursor);
    let ordered = ordered.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
    let positions = ordered
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<rapidhash::RapidHashMap<_, _>>();
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
