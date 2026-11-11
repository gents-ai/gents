mod recovery;
#[cfg(test)]
mod retained_tests;
#[cfg(test)]
mod task_delivery_tests;
use crate::request::lifecycle::materialize::{
    created_session_provenance_in_txn, refuse_retired_reserved_cause, sign_request, RequestSigner,
};
use crate::request::lifecycle::{
    bind_client_targets_in_txn, stage_client_request_in_txn,
    stage_revoke_execution_preserving_output, RequestTerminalOutcome, TerminalizeResult,
};
use crate::session;
use crate::store::config_client::ConfigApplyTxn;
use anyhow::{ensure, Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequest;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
#[cfg(test)]
pub(crate) use recovery::recover_outcomes;
#[cfg(test)]
pub(crate) use recovery::recover_outcomes_in_txn;
pub(crate) use recovery::recover_outcomes_with_access;

#[derive(Clone)]
pub(crate) struct PreparedFire {
    pub receipt: TriggerFire,
    pub target_existing: bool,
    pub replace_prior: bool,
    pub event_source_generation: Option<serde_json::Value>,
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
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::NODE_DID, &fire.identity.owner_did))
                .filter(Filter::eq(fields::SESSION_ID, &fire.session_id))
                .filter(Filter::eq(fields::REQUEST_ID, &fire.request_id))
                .render(),
        )
        .await?
    };
    let requests = response["data"]["AgentRequest"]
        .as_array()
        .context("Task assignment request query omitted rows")?;
    ensure!(
        requests.len() == 1,
        "Task assignment has no unique physical request in its owner/session"
    );
    let root = requests[0]["_docID"]
        .as_str()
        .context("Task assignment request has no physical identity")?;
    let versions = crate::goal::load_assignment_versions_for_session(
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
    ensure!(
        retained.len() <= 1,
        "Task assignment has ambiguous physical Goal history"
    );
    let retained_binding = retained.first().map(|goal| GoalOutcomeBinding {
        owner_did: goal.node_did.clone(),
        session_id: goal.session_id.clone(),
        assignment_request_id: fire.request_id.clone(),
        status: goal.status.clone(),
    });
    ensure!(
        current_binding
            .as_ref()
            .is_none_or(|binding| binding.assignment_request_id != fire.request_id),
        "Task assignment root is reused by a different Goal identity"
    );
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

pub(crate) async fn stage_fire_receipt(
    txn: &ConfigApplyTxn<'_>,
    fire: &TriggerFire,
) -> Result<bool> {
    ensure!(
        crate::documents::event_trigger::outcome_source_allowed(
            &fire.identity.source_collection,
            fire.emit_outcome,
        ),
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
    let prior = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::new().field(fields::REQUEST_ID))
                .filter(Filter::eq(fields::FIRE_KEY, &fire.fire_key))
                .render(),
        )
        .await?
    };
    if prior["data"]["TriggerFire"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(false);
    }
    txn.execute_client_signed_create(
        "mutation($input: TriggerFireMutationInputArg!) { create_TriggerFire(input: $input) { _docID } }",
        &serde_json::json!({"input":fire}),
        gents_protocol::schemas::TRIGGER_FIRE,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn stage_session_child_relationships(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    session_doc_id: &str,
    agent_did: &str,
) -> Result<()> {
    ensure!(
        matches!(collection, "TriggerFire" | "FireOutcome"),
        "unsupported Task receipt collection"
    );
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
    ensure!(
        fire.fire_key == fire_key(&fire.identity),
        "noncanonical outcome fire identity"
    );
    let handoff_id = fire.identity.outcome_id();
    if outcome_published(txn, fire).await? {
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
    let response = txn.execute_client_signed_create(
        "mutation($input: FireOutcomeMutationInputArg!) { create_FireOutcome(input: $input) { _docID } }",
        &serde_json::json!({"input":outcome}),
        gents_protocol::schemas::FIRE_OUTCOME,
    ).await?;
    let doc_id = crate::store::graphql::created_doc_id(&response, "FireOutcome")?;
    let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &fire.identity.owner_did,
        &fire.session_id,
    )
    .await?
    .context("Task outcome session is unavailable")?;
    stage_session_child_relationships(
        txn,
        "FireOutcome",
        &doc_id,
        &session.doc_id,
        &session.session.agent_did,
    )
    .await?;
    for reader in declared_outcome_readers(txn, &fire.identity.owner_did, &fire.task_id).await? {
        stage_outcome_reader(txn, &doc_id, &reader).await?;
    }
    Ok(true)
}

/// The stage Task's declared `outcome_readers`, read in the publication
/// transaction. An unavailable Task declares nothing, so nothing is granted.
async fn declared_outcome_readers(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    task_id: &str,
) -> Result<Vec<String>> {
    let Some((_, task)) = crate::documents::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Task,
        owner,
        task_id,
    )
    .await?
    else {
        return Ok(Vec::new());
    };
    let task: crate::documents::Task =
        serde_json::from_value(task).context("decoding the outcome's Task")?;
    Ok(task
        .outcome_readers
        .into_iter()
        .map(|reader| reader.agent_did)
        .collect())
}

/// Lean `Identity.Automation.publishOutcome`: applies one declared `reader`
/// tuple on this FireOutcome, staged in the transaction that creates it.
pub(crate) async fn stage_outcome_reader(
    txn: &ConfigApplyTxn<'_>,
    outcome_doc_id: &str,
    reader_did: &str,
) -> Result<()> {
    txn.add_actor_relationship(
        "FireOutcome",
        outcome_doc_id,
        "reader",
        &identity::Did::new(reader_did)?,
    )
    .await?;
    Ok(())
}

const FIRE_FIELDS: &str = "fire_key owner_did trigger_id source_collection source_doc_id task_id request_id session_id goal_id goal_objective goal_token_budget goal_assignment_applied emit_outcome queued_serial source_handoff_id reply_session_id shard_id attempt created_at";

async fn outcome_published(txn: &ConfigApplyTxn<'_>, fire: &TriggerFire) -> Result<bool> {
    let response = {
        use gents_protocol::document_query::{
            fields::fire_outcome as fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::new().field(fields::HANDOFF_ID))
                .filter(Filter::eq(fields::HANDOFF_ID, fire.identity.outcome_id()))
                .render(),
        )
        .await?
    };
    Ok(!response["data"]["FireOutcome"]
        .as_array()
        .context("Task outcome query omitted rows")?
        .is_empty())
}

pub(crate) async fn publish_request_outcome(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    request_id: &str,
    terminal_state: RequestLifecycleState,
    reason: &str,
    now: &str,
) -> Result<()> {
    let response = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::owned(FIRE_FIELDS))
                .filter(Filter::eq(fields::OWNER_DID, owner))
                .filter(Filter::eq(fields::REQUEST_ID, request_id))
                .render(),
        )
        .await?
    };
    let rows: Vec<TriggerFire> = serde_json::from_value(response["data"]["TriggerFire"].clone())?;
    for fire in rows {
        let stage = OutcomeStage {
            goal: None,
            assignment_replaced: false,
            request_terminal: true,
            terminal_state: terminal_state.as_str(),
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
    let response = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::owned(FIRE_FIELDS))
                .filter(Filter::eq(fields::OWNER_DID, owner))
                .filter(Filter::eq(fields::GOAL_ID, goal_id))
                .render(),
        )
        .await?
    };
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

pub fn task_fire_key(identity: &gents_protocol::trigger_delivery::FireIdentity) -> String {
    fire_key(identity)
}

pub struct TaskDeliveryAdmission {
    pub request: EnqueuedAgentRequest,
    pub duplicate: bool,
}

pub async fn write_task_delivery(
    access: &crate::store::config_client::ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    access
        .transact_write("lifecycle.admit_task_delivery", |txn| {
            Box::pin(async move { stage_task_delivery(txn, fire, continue_existing, create).await })
        })
        .await
}

pub async fn write_task_delivery_local(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    let access = crate::store::config_client::ConfigAccess::local_as(node.clone(), actor);
    write_task_delivery(&access, fire, continue_existing, create).await
}

pub(crate) async fn write_trigger_delivery_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    prepared: &PreparedFire,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&dyn crate::store::identity::NodeIdentity>,
) -> Result<TaskDeliveryAdmission> {
    let identity = identity.context("trigger delivery requires a managed Agent identity")?;
    let access = crate::ConfigAccess::local_managed_agent(node.clone(), actor, identity)?;
    access
        .transact_write("lifecycle.admit_trigger_delivery", |txn| {
            Box::pin(async move {
                let fresh = stage_fire_receipt(txn, &prepared.receipt).await?;
                let mut create = create.clone();
                if fresh {
                    if prepared.target_existing && prepared.receipt.goal_id.is_none() {
                        create.input = crate::goal::prepare_goal_input_in_txn(
                            txn,
                            &create.node_did,
                            &create.session_id,
                            &create.agent_did,
                            Some(&create.requester_did),
                            create.input,
                        )
                        .await?;
                    }
                    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
                }
                let admitted = stage_task_delivery_with_receipt(
                    txn,
                    &prepared.receipt,
                    prepared.target_existing,
                    &create,
                    fresh,
                    prepared.event_source_generation.as_ref(),
                )
                .await?;
                if prepared.replace_prior && !admitted.duplicate {
                    supersede_trigger_requests_in_txn(
                        txn,
                        &prepared.receipt.identity.owner_did,
                        &prepared.receipt.identity.trigger_id,
                        Some(&prepared.receipt.request_id),
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(admitted)
            })
        })
        .await
}

#[doc(hidden)]
pub async fn supersede_trigger_requests_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
    excluded_request_id: Option<&str>,
) -> Result<usize> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Input, Query, Selection, Update,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let scope = Filter::eq(fields::NODE_DID, owner)
        .and(Filter::eq(fields::CAUSED_BY_TRIGGER_ID, trigger_id));
    let scope = match excluded_request_id {
        Some(id) => scope.and(Filter::ne(fields::REQUEST_ID, id)),
        None => scope,
    };
    let mutation = Update::matching(
        scope.clone().and(Filter::any_of(
            fields::LIFECYCLE_STATE,
            [
                gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
                gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending,
            ]
            .into_iter()
            .map(gents_protocol::request_lifecycle::RequestLifecycleState::as_str),
        )),
        Input::new()
            .set(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::Superseded.as_str(),
            )
            .set(fields::TERMINALIZED_AT, now.as_str())
            .set(fields::TERMINAL_REDRIVE_ATTEMPTS, 0_i32),
    )
    .returning(Selection::doc_id().field(fields::REQUEST_ID))
    .render();
    let response = txn.run(&mutation).await?;
    let pending = response["data"]["update_AgentRequest"]
        .as_array()
        .context("supersession omitted pending requests")?;
    for row in pending {
        publish_request_outcome(
            txn,
            owner,
            row["request_id"]
                .as_str()
                .context("superseded request lacks ID")?,
            RequestLifecycleState::Superseded,
            "newer trigger fire",
            &now,
        )
        .await?;
    }
    let query = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::NODE_DID)
            .field(fields::REQUESTER_DID)
            .field(fields::AGENT_DID)
            .field(fields::SESSION_ID)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::EXECUTION_GENERATION)
            .field(fields::EXECUTION_LEASE_EXPIRES_AT),
    )
    .filter(
        scope.and(Filter::any_of(
            fields::LIFECYCLE_STATE,
            [
                gents_protocol::request_lifecycle::RequestLifecycleState::Claimed,
                gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
            ]
            .into_iter()
            .map(gents_protocol::request_lifecycle::RequestLifecycleState::as_str),
        )),
    )
    .render();
    let response = txn.run(&query).await?;
    let active: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    for row in &active {
        anyhow::ensure!(
            stage_revoke_execution_preserving_output(
                txn,
                row,
                RequestTerminalOutcome::Superseded,
                "superseded by a newer trigger fire",
            )
            .await?
                == TerminalizeResult::Won,
            "execution changed during atomic trigger supersession",
        );
    }
    Ok(pending.len() + active.len())
}

async fn stage_task_delivery(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    let fresh = stage_fire_receipt(txn, fire).await?;
    stage_task_delivery_with_receipt(txn, fire, continue_existing, create, fresh, None).await
}

async fn stage_task_delivery_with_receipt(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    fresh: bool,
    expected_event_source: Option<&serde_json::Value>,
) -> Result<TaskDeliveryAdmission> {
    if fresh {
        if create.caused_by_trigger_kind.as_deref() == Some("event") {
            anyhow::ensure!(
                create.caused_by_trigger_id.as_deref() == Some(fire.identity.trigger_id.as_str()),
                "event request and receipt trigger identity disagree"
            );
            let generation = expected_event_source
                .context("event source generation is required for admission")?;
            crate::documents::event_consumer::validate_event_admission(txn, fire, generation)
                .await?;
        }
        anyhow::ensure!(
            create.node_did == fire.identity.owner_did
                && create.request_id == fire.request_id
                && create.session_id == fire.session_id,
            "Task receipt and signed request identity disagree"
        );
        anyhow::ensure!(
            !fire.goal_assignment_applied,
            "a Task admission cannot apply its Goal assignment"
        );
        anyhow::ensure!(
            !fire.emit_outcome
                || fire
                    .source_handoff_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty()),
            "outcome-enabled Task requires its source handoff identity"
        );
        match &fire.goal_id {
            Some(goal_id) => {
                crate::goal::validate_task_goal_identity_in_txn(
                    txn,
                    &create.node_did,
                    &create.session_id,
                    goal_id,
                    create.input.goal_assignment.as_ref(),
                )
                .await?;
                anyhow::ensure!(
                    fire.goal_objective
                        .as_deref()
                        .is_some_and(|objective| !objective.trim().is_empty()),
                    "Goal-backed Task requires an objective"
                );
                anyhow::ensure!(
                    fire.goal_token_budget.is_none_or(|budget| budget > 0),
                    "Task Goal budget must be positive"
                );
            }
            None => anyhow::ensure!(
                fire.goal_objective.is_none() && fire.goal_token_budget.is_none(),
                "ordinary Task receipt cannot declare a Goal assignment"
            ),
        }
        let manual_label = create.caused_by_trigger_kind.as_deref() == Some("manual")
            && create.caused_by_trigger_id.is_none()
            && fire.identity.source_collection == "Task"
            && fire
                .identity
                .trigger_id
                .starts_with(&format!("manual:{}:", fire.task_id));
        let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
            txn,
            &create.node_did,
            &create.session_id,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if continue_existing {
            anyhow::ensure!(
                resolve_session_id(&fire.identity, Some(&create.session_id), session.is_some())
                    .is_some(),
                "Task target session is missing or belongs to another owner"
            );
        } else {
            anyhow::ensure!(
                manual_label
                    || resolve_session_id(&fire.identity, None, false).as_deref()
                        == Some(fire.session_id.as_str()),
                "new Task session must derive from its fire identity"
            );
        }
        if let Some(session) = session {
            anyhow::ensure!(
                session.session.agent_did == create.agent_did,
                "Task target session has a different agent"
            );
            anyhow::ensure!(
                session.session.closed_at.is_none(),
                "Task target session is closed"
            );
        }
        if create.admission.kind
            == gents_protocol::request_admission::AgentRequestAdmissionKind::Client
        {
            let mut resolved = create.clone();
            bind_client_targets_in_txn(txn, &mut resolved).await?;
            anyhow::ensure!(
                create.session_doc_id == resolved.session_doc_id
                    && create.agent_doc_id == resolved.agent_doc_id,
                "Task request physical targets changed before publication"
            );
            refuse_retired_reserved_cause(create)?;
            stage_client_request_in_txn(
                txn,
                create,
                create
                    .session_doc_id
                    .as_deref()
                    .context("client Task session is unavailable")?,
                create
                    .agent_doc_id
                    .as_deref()
                    .context("client Task agent is unavailable")?,
            )
            .await?;
        } else {
            refuse_retired_reserved_cause(create)?;
            txn.execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                .await?;
        }
    }
    let query = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        Query::select(Selection::owned(gents_protocol::row::AGENT_REQUEST_FIELDS))
            .filter(Filter::eq(fields::NODE_DID, &fire.identity.owner_did))
            .filter(Filter::eq(fields::REQUEST_ID, &fire.request_id))
            .render()
    };
    let response = txn.run(&query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("admitted request query omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "fire receipt requires exactly one admitted request"
    );
    let session_id = rows[0]["session_id"]
        .as_str()
        .context("admitted request lacks session ID")?;
    anyhow::ensure!(
        !fresh || session_id == fire.session_id,
        "admitted request changed its fire's session"
    );
    if fresh {
        let request = AgentRequest::try_from(serde_json::from_value::<
            gents_protocol::row::AgentRequestRow,
        >(rows[0].clone())?)?;
        let provenance = created_session_provenance_in_txn(txn, &request).await?;
        let created = session::ensure_session_in_txn(
            txn,
            &request.session_id,
            &request.node_did,
            &request.agent_did,
            request.requester_did.as_deref(),
            request.input.initial_title.clone(),
            provenance,
            &request.created_at,
        )
        .await?;
        let agent_owned = matches!(
            create.admission.kind,
            gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf
                | gents_protocol::request_admission::AgentRequestAdmissionKind::RuntimeInternal
        );
        if created || agent_owned {
            let session =
                crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    &request.node_did,
                    &request.session_id,
                    request.requester_did.as_deref(),
                )
                .await?
                .context("Task session disappeared")?;
            let (agent, _) = crate::documents::read_desired_state_record_in_txn(
                txn,
                crate::Collection::Agent,
                &request.node_did,
                &request.agent_did,
            )
            .await?
            .context("selected Task agent is unavailable")?;
            if agent_owned {
                anyhow::ensure!(
                    txn.actor_did() == Some(request.agent_did.as_str())
                        && create.admission.signer_did == request.agent_did
                        && request.doc_id == rows[0]["_docID"].as_str().unwrap_or_default()
                        && request.node_did == create.node_did
                        && request.agent_did == create.agent_did
                        && request.session_id == create.session_id
                        && request.request_id == create.request_id
                        && request.requester_did.as_deref() == Some(create.requester_did.as_str())
                        && session.session.agent_did == request.agent_did,
                    "Task request publication differs from its authenticated Agent or session"
                );
                anyhow::ensure!(
                    crate::store::identity::verify_did_signature(
                        &create.admission.signer_did,
                        &create.signing_payload(),
                        &create.admission.signature,
                    )?,
                    "Task request publication requires its valid signed receipt"
                );
                txn.add_resource_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "parent",
                    "agents",
                    &agent,
                )
                .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "session",
                    "sessions",
                    &session.doc_id,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "working_agent",
                    &identity::Did::new(&request.agent_did)?,
                )
                .await?;
            }
            if created {
                session::stage_created_session_relationships_in_txn(
                    txn,
                    &session.doc_id,
                    &agent,
                    &identity::Did::new(&request.agent_did)?,
                )
                .await?;
            }
        }
        let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
            txn,
            &request.node_did,
            &request.session_id,
            request.requester_did.as_deref(),
        )
        .await?
        .context("Task receipt session is unavailable")?;
        let query = {
            use gents_protocol::document_query::{
                fields::trigger_fire as fields, Filter, Query, Selection,
            };
            Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::FIRE_KEY, &fire.fire_key))
                .limit(2)
                .render()
        };
        let receipt = txn.run(&query).await?;
        let rows = receipt["data"]["TriggerFire"]
            .as_array()
            .context("Task receipt query omitted rows")?;
        let [receipt] = rows.as_slice() else {
            anyhow::bail!("Task receipt is missing or ambiguous")
        };
        let doc_id = receipt["_docID"]
            .as_str()
            .context("Task receipt lacks physical ID")?;
        stage_session_child_relationships(
            txn,
            "TriggerFire",
            doc_id,
            &session.doc_id,
            &request.agent_did,
        )
        .await?;
    }
    Ok(TaskDeliveryAdmission {
        duplicate: !fresh,
        request: EnqueuedAgentRequest {
            doc_id: rows[0]["_docID"]
                .as_str()
                .context("admitted request lacks document ID")?
                .into(),
            request_id: fire.request_id.clone(),
            session_id: session_id.into(),
        },
    })
}
