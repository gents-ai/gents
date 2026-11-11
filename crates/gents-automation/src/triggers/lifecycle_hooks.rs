//! Triggers at the lifecycle hook points. Triggers are core, so their writes
//! are core operations a hook requests; the hook itself only reads and decides.

use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;

use super::engine::durable::{publish_goal_outcomes, stage_outcome, OutcomeStage, FIRE_FIELDS};
use gents_session::request::hooks::effect::{CoreOp, Effect, Manifest, Row};
use gents_session::request::hooks::hook::{Hook, Outcome, Query};
use gents_session::request::hooks::registry::{CoreOperation, Registry, RequestTerminal};
use gents_store::config_client::ConfigApplyTxn;

pub(crate) const PUBLISH_FIRE_OUTCOME: &str = "trigger.fire_outcome.publish";
pub(crate) const SETTLE_GOAL_OUTCOMES: &str = "trigger.goal_outcomes.settle";
pub(crate) const APPLY_GOAL_ASSIGNMENT: &str = "trigger.goal_assignment.apply";

/// One request has one Task fire receipt; the bound leaves room for the
/// duplicates the publication itself tolerates.
const FIRES_PER_REQUEST: usize = 8;

/// `request.terminal.stage`: a terminal request publishes the outcome of the
/// Task fire that admitted it.
struct RequestOutcome {
    manifest: Manifest,
}

impl Hook<RequestTerminal> for RequestOutcome {
    type Value = ();

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, event: &RequestTerminal) -> Vec<Query> {
        vec![Query::select("TriggerFire", FIRE_FIELDS)
            .eq("owner_did", event.node_did.clone())
            .eq("request_id", event.request_id.clone())]
    }

    fn decide(&self, event: &RequestTerminal, rows: &[Vec<Row>]) -> Outcome<()> {
        let [fires] = rows else {
            return Outcome::Refuse("Task fire receipts were not shown".into());
        };
        Outcome::effects(
            fires
                .iter()
                .map(|fire| {
                    Effect::Core(CoreOp::new(
                        PUBLISH_FIRE_OUTCOME,
                        serde_json::json!({
                            "fire": fire.value,
                            "terminal_state": event.state.as_str(),
                            "reason": event.reason,
                            "at": event.at,
                        }),
                    ))
                })
                .collect(),
        )
    }
}

#[derive(Deserialize)]
struct PublishFireOutcome {
    fire: Value,
    terminal_state: String,
    reason: String,
    at: String,
}

/// Publishes the FireOutcome of one fire whose request is terminal. Inert for
/// a fire that emits no outcome or whose outcome is already published.
struct PublishFireOutcomeOperation;

#[async_trait::async_trait]
impl CoreOperation for PublishFireOutcomeOperation {
    fn name(&self) -> &'static str {
        PUBLISH_FIRE_OUTCOME
    }

    async fn perform(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<()> {
        let mut args = PublishFireOutcome::deserialize(args)
            .context("decoding the fire outcome publication")?;
        if let Some(fire) = args.fire.as_object_mut() {
            fire.remove("_docID");
        }
        let fire = serde_json::from_value(args.fire).context("decoding the Task fire receipt")?;
        stage_outcome(
            txn,
            &fire,
            OutcomeStage {
                goal: None,
                assignment_replaced: false,
                request_terminal: true,
                terminal_state: &args.terminal_state,
                reason: &args.reason,
            },
            &args.at,
        )
        .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct SettleGoalOutcomes {
    owner_did: String,
    goal_id: String,
    reason: String,
    at: String,
}

/// Publishes the outcome of every fire assigned to a Goal whose status ended
/// the assignment. Requested by the Goals plugin after it changes a Goal.
struct SettleGoalOutcomesOperation;

#[async_trait::async_trait]
impl CoreOperation for SettleGoalOutcomesOperation {
    fn name(&self) -> &'static str {
        SETTLE_GOAL_OUTCOMES
    }

    async fn perform(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<()> {
        let args = SettleGoalOutcomes::deserialize(args)
            .context("decoding the Goal outcome settlement")?;
        publish_goal_outcomes(txn, &args.owner_did, &args.goal_id, &args.reason, &args.at).await
    }
}

/// The effect a plugin returns to have core settle a Goal's fire outcomes.
pub(crate) fn settle_goal_outcomes(
    owner_did: &str,
    goal_id: &str,
    reason: &str,
    at: &str,
) -> Effect {
    Effect::Core(CoreOp::new(
        SETTLE_GOAL_OUTCOMES,
        serde_json::json!({
            "owner_did": owner_did,
            "goal_id": goal_id,
            "reason": reason,
            "at": at,
        }),
    ))
}

#[derive(Deserialize)]
struct ApplyGoalAssignment {
    fire_doc_id: String,
}

/// Marks a Task fire's Goal assignment applied (Lean
/// `Hooks.GoalTask.TaskOp.assignmentApplied`). The flag is a TriggerFire
/// field, so core writes it when the Goals plugin applies the assignment.
struct ApplyGoalAssignmentOperation;

#[async_trait::async_trait]
impl CoreOperation for ApplyGoalAssignmentOperation {
    fn name(&self) -> &'static str {
        APPLY_GOAL_ASSIGNMENT
    }

    async fn perform(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<()> {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Input, Update,
        };
        let args = ApplyGoalAssignment::deserialize(args)
            .context("decoding the Goal assignment application")?;
        let rendered = Update::matching(
            Filter::eq(fields::DOC_ID, &args.fire_doc_id),
            Input::new().set(fields::GOAL_ASSIGNMENT_APPLIED, true),
        )
        .render();
        txn.run(&rendered).await?;
        Ok(())
    }
}

/// The effect a plugin returns to have core mark a fire's Goal assignment
/// applied.
pub(crate) fn apply_goal_assignment(fire_doc_id: &str) -> Effect {
    Effect::Core(CoreOp::new(
        APPLY_GOAL_ASSIGNMENT,
        serde_json::json!({ "fire_doc_id": fire_doc_id }),
    ))
}

pub fn register(registry: &mut Registry) {
    registry.request_terminal.push(Arc::new(RequestOutcome {
        manifest: Manifest {
            name: "triggers",
            owns: Vec::new(),
            bound: FIRES_PER_REQUEST,
        },
    }));
    registry
        .operations
        .push(Arc::new(PublishFireOutcomeOperation));
    registry
        .operations
        .push(Arc::new(SettleGoalOutcomesOperation));
    registry
        .operations
        .push(Arc::new(ApplyGoalAssignmentOperation));
}
