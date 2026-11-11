//! Goals at the lifecycle hook points. Goals are a bundled plugin: each hook
//! declares its reads, decides on the rows it is shown and returns its writes
//! to the Goal collection, or a named core operation, as data.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gents_protocol::session::SESSION_SCOPE_FIELDS;

use super::*;
use gents_protocol::goal_names::GOAL_TRIGGER_KIND;
use gents_protocol::row::AgentRequestRow;
use gents_session::request::hooks::claim::{ClaimVerdict, Successor};
use gents_session::request::hooks::effect::{Effect, Manifest, OwnedCollection, Row, Write};
use gents_session::request::hooks::hook::{Hook, Outcome, Query};
use gents_session::request::hooks::prepared::{Decide, Verdict};
use gents_session::request::hooks::queue_scope::{ScopeAnswer, ScopeAnswers, ScopeFailure};
use gents_session::request::hooks::registry::{
    AdmitShown, ClaimCandidate, QueueScopeAsk, Registry, SessionHalt,
};
use gents_store::config_client::agent_session::{decode_agent_session_read, AgentSessionRowRead};

pub(super) const PLUGIN: &str = "goals";

/// A Goal hook stages at most its Goal write and the outcome settlements that
/// write owes.
const EFFECTS_PER_POINT: usize = 16;

pub(super) fn manifest() -> Manifest {
    Manifest {
        name: PLUGIN,
        owns: vec![OwnedCollection {
            name: "Goal",
            schema: Some(gents_protocol::schemas::GOAL),
        }],
        bound: EFFECTS_PER_POINT,
    }
}

/// The session row and the session's Goal rows: the reads from which the
/// canonical Goal of a session is resolved without a second round.
pub(super) fn canonical_goal_reads(node_did: &str, session_id: &str) -> [Query; 2] {
    [
        Query::select("AgentSession", SESSION_SCOPE_FIELDS)
            .eq("node_did", node_did)
            .eq("session_id", session_id)
            .limit(2),
        Query::select("Goal", format!("{GOAL_FIELDS} session_doc_id"))
            .eq("node_did", node_did)
            .eq("session_id", session_id),
    ]
}

/// The canonical Goal of the session the rows were read for, with the row it
/// was shown as (the decision `load_canonical_goal_in_txn` makes on the same
/// rows).
pub(super) fn canonical_goal<'a>(
    session_id: &str,
    sessions: &[Row],
    goals: &'a [Row],
) -> Result<Option<(GoalDocument, &'a Row)>> {
    let response = serde_json::json!({
        "data": { "AgentSession": sessions.iter().map(|row| &row.value).collect::<Vec<_>>() }
    });
    let AgentSessionRowRead::Present(session) = decode_agent_session_read(&response, session_id)?
    else {
        return Ok(None);
    };
    let mut canonical = Vec::new();
    for row in goals {
        if row
            .value
            .get("session_doc_id")
            .and_then(serde_json::Value::as_str)
            != Some(session.doc_id.as_str())
        {
            continue;
        }
        let goal: GoalDocument = serde_json::from_value(row.value.clone())
            .context("decoding transactional Goal rows")?;
        canonical.push((goal, row));
    }
    canonical.sort_by(|(left, _), (right, _)| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.goal_id.cmp(&right.goal_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    Ok(canonical.into_iter().next())
}

/// `control.session.halt`: an interrupt or stop pauses the session's Goal.
struct PauseOnHalt {
    manifest: Manifest,
}

impl PauseOnHalt {
    fn effects(event: &SessionHalt, rows: &[Vec<Row>]) -> Result<Vec<Effect>> {
        let [sessions, goals] = rows else {
            bail!("Goal pause was not shown its declared rows");
        };
        let Some((goal, row)) = canonical_goal(&event.session_id, sessions, goals)? else {
            return Ok(Vec::new());
        };
        let state = goal.state().context("Goal has an unknown status")?;
        if state.step(GoalAction::Pause).is_none() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            goal.token_budget.is_none_or(|budget| budget > 0),
            "goal token budget must be positive"
        );
        let now = DateTime::parse_from_rfc3339(&event.at)
            .context("halt time is not RFC 3339")?
            .with_timezone(&Utc);
        let paused = apply_operator_status_transition(state, GoalStatus::Paused)?;
        let input = serde_json::json!({
            "objective": goal.objective,
            "status": paused.status.as_str(),
            "token_budget": goal.token_budget,
            "tokens_used": goal.tokens_used.unwrap_or_default().max(0),
            "active_time_seconds": goal.current_active_time_seconds(now),
            "active_started_at": paused.status.accrues_active_time().then_some(event.at.as_str()),
            "consecutive_blocked_audits": paused.blocked_audits,
            "wrapup_requested": paused.wrapup_requested,
            "wrapup_completed": paused.wrapup_completed,
            "updated_at": event.at,
        });
        Ok(vec![
            Effect::Write(Write::Update {
                doc: row.doc.clone(),
                expected: row.revision,
                value: input,
            }),
            crate::triggers::lifecycle_hooks::settle_goal_outcomes(
                &goal.node_did,
                &goal.goal_id,
                goal.last_blocked_reason
                    .as_deref()
                    .unwrap_or(paused.status.as_str()),
                &event.at,
            ),
        ])
    }
}

impl Hook<SessionHalt> for PauseOnHalt {
    type Value = ();

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, event: &SessionHalt) -> Vec<Query> {
        canonical_goal_reads(&event.node_did, &event.session_id).into()
    }

    fn decide(&self, event: &SessionHalt, rows: &[Vec<Row>]) -> Outcome<()> {
        match Self::effects(event, rows) {
            Ok(effects) => Outcome::effects(effects),
            Err(error) => Outcome::Refuse(format!("{error:#}")),
        }
    }
}

/// `request.claim.gate`: the Goal continuation fence (Lean
/// `Hooks.GoalFence.fence`). A client continuation must carry the binding to
/// the Goal's verified opening; a pending continuation that is not a member
/// of the Goal's current assignment is retired.
struct ClaimFence {
    manifest: Manifest,
}

const CLAIM_DENIED: &str = "request authorization denied";

fn goal_triggered(event: &ClaimCandidate) -> bool {
    event.request.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
}

impl ClaimFence {
    fn verdict(event: &ClaimCandidate, rows: &[Vec<Row>]) -> Result<ClaimVerdict> {
        let request = &event.request;
        let client_goal = event.client && request.input.goal_continuation.is_some();
        let denied = || ClaimVerdict::Reject {
            reason: CLAIM_DENIED.into(),
        };
        if !goal_triggered(event) {
            return Ok(if client_goal {
                denied()
            } else {
                ClaimVerdict::Allow
            });
        }
        let [sessions, goals, requests] = rows else {
            bail!("Goal claim fence was not shown its declared rows");
        };
        let goal = canonical_goal(&request.session_id, sessions, goals)?.map(|(goal, _)| goal);
        let requests = requests
            .iter()
            .map(|row| serde_json::from_value::<AgentRequestRow>(row.value.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if client_goal {
            let opening = goal.as_ref().and_then(|goal| {
                let root = goal
                    .assignment_root_request_doc_id
                    .as_deref()
                    .filter(|id| !id.is_empty())?;
                requests
                    .iter()
                    .find(|row| row.doc_id.as_deref() == Some(root))
                    .map(|row| {
                        (
                            request_assignment::request_belongs_to_goal(goal, row),
                            row.agent_did.as_deref() == Some(request.agent_did.as_str()),
                        )
                    })
            });
            let (opening_verified, opening_agent_matches) = opening.unwrap_or((false, false));
            if !controller::client_goal_claim_binding(
                goal.as_ref(),
                request.caused_by_trigger_id.as_deref(),
                opening_verified,
                opening_agent_matches,
            ) {
                return Ok(denied());
            }
        }
        let Some(goal) = goal else {
            return Ok(ClaimVerdict::Allow);
        };
        let stale =
            request_head::stale_assignment_continuations(&goal, Some(&request.doc_id), &requests)?;
        Ok(match stale.into_iter().next() {
            None => ClaimVerdict::Allow,
            Some(stale) => ClaimVerdict::Retire {
                reason: request_head::STALE_ASSIGNMENT_REASON.into(),
                successor: Some(Successor {
                    request_id: stale.survivor_request_id,
                    request_doc_id: stale.survivor_doc_id,
                }),
            },
        })
    }
}

impl Hook<ClaimCandidate> for ClaimFence {
    type Value = ClaimVerdict;

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, event: &ClaimCandidate) -> Vec<Query> {
        if !goal_triggered(event) {
            return Vec::new();
        }
        let request = &event.request;
        let [session, goals] = canonical_goal_reads(&request.node_did, &request.session_id);
        vec![
            session,
            goals,
            Query::select(
                "AgentRequest",
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            )
            .eq("node_did", request.node_did.clone())
            .eq("session_id", request.session_id.clone()),
        ]
    }

    fn decide(&self, event: &ClaimCandidate, rows: &[Vec<Row>]) -> Outcome<ClaimVerdict> {
        match Self::verdict(event, rows) {
            Ok(verdict) => Outcome::proceed(verdict),
            Err(error) => Outcome::Refuse(format!("{error:#}")),
        }
    }
}

/// `request.admit.verify`: a Goal continuation must be the physical,
/// signed continuation edge of its session predecessor
/// (`verify_goal_continuation_edge`).
struct ContinuationEdge;

impl Decide<AgentRequestRow, AdmitShown> for ContinuationEdge {
    type Payload = ();

    fn name(&self) -> &'static str {
        PLUGIN
    }

    fn configuration(&self) -> &str {
        ""
    }

    fn applies(&self, request: &AgentRequestRow) -> bool {
        request.input.as_ref().is_some_and(|input| {
            input.goal_continuation.is_some()
                || input.queue.as_ref().is_some_and(|queue| {
                    queue.source == gents_protocol::request_input::QueueSource::Goal
                })
        })
    }

    fn decide(&self, shown: &AdmitShown) -> Verdict<()> {
        let request = &shown.request;
        let required = |value: Option<&str>, field: &str| {
            value
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("AgentRequest is missing {field}"))
        };
        let verified = required(request.node_did.as_deref(), "node_did").and_then(|node_did| {
            let session_id = required(request.session_id.as_deref(), "session_id")?;
            let goal_id = required(request.caused_by_trigger_id.as_deref(), "goal_id")?;
            verify_goal_continuation_edge(&node_did, &session_id, &goal_id, &shown.parent, request)
        });
        match verified {
            Ok(_) => Verdict::Allow(()),
            Err(error) => Verdict::Deny(format!("{error:#}")),
        }
    }
}

/// `request.queue.scope`: each asked request's Goal assignment scope (Lean
/// `Hooks.QueueScope.goals`), from its signed reference or, for a request
/// without one, the session's retained assignments. The Goal history is
/// declared only when some asked request needs it, so a queue site whose
/// requests all carry signed references reads no Goal row.
struct AssignmentScope {
    manifest: Manifest,
}

impl AssignmentScope {
    fn assignments(
        event: &QueueScopeAsk,
        goals: Vec<GoalDocument>,
    ) -> request_assignment::RequestAssignments {
        request_assignment::RequestAssignments::over(Arc::clone(&event.requests), goals)
    }

    fn retained(event: &QueueScopeAsk, rows: &[Vec<Row>]) -> Result<Vec<GoalDocument>> {
        match rows {
            [] => Ok(Vec::new()),
            [history] => request_assignment::latest_assignment_goals(
                &history::assignment_versions_from_rows(
                    &event.node_did,
                    &event.session_id,
                    history,
                )?,
            ),
            _ => bail!("Goal queue scope was not shown its declared rows"),
        }
    }

    fn answers(event: &QueueScopeAsk, rows: &[Vec<Row>]) -> ScopeAnswers {
        let (assignments, unavailable) = match Self::retained(event, rows) {
            Ok(goals) => (Self::assignments(event, goals), None),
            Err(error) => (
                Self::assignments(event, Vec::new()),
                Some(ScopeFailure::of(&error)),
            ),
        };
        let mut answers = ScopeAnswers::default();
        for doc in &event.asked {
            let scope =
                assignments.request(doc).and_then(|_| {
                    match request_assignment::signed_scope(&assignments, doc) {
                        Ok(Some(scope)) => Ok(Ok(scope)),
                        _ => match &unavailable {
                            Some(failure) => Ok(Err(failure.clone())),
                            None => assignments.scope(doc).map(Ok),
                        },
                    }
                });
            let answer = match scope {
                Ok(Ok(scope)) => ScopeAnswer::Keyed(scope),
                Ok(Err(failure)) => ScopeAnswer::Failed(failure),
                Err(error) => ScopeAnswer::Failed(ScopeFailure::of(&error)),
            };
            answers.insert(doc.clone(), answer);
        }
        answers
    }
}

impl Hook<QueueScopeAsk> for AssignmentScope {
    type Value = ScopeAnswers;

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, event: &QueueScopeAsk) -> Vec<Query> {
        if request_assignment::scopes_need_history(
            &Self::assignments(event, Vec::new()),
            &event.asked,
        ) {
            vec![history::assignment_history_read(
                &event.node_did,
                &event.session_id,
            )]
        } else {
            Vec::new()
        }
    }

    fn decide(&self, event: &QueueScopeAsk, rows: &[Vec<Row>]) -> Outcome<ScopeAnswers> {
        Outcome::proceed(Self::answers(event, rows))
    }
}

pub fn register(registry: &mut Registry) {
    registry
        .request_admit_verify
        .push(Arc::new(ContinuationEdge));
    registry.queue_scope.push(Arc::new(AssignmentScope {
        manifest: manifest(),
    }));
    registry.claim_gate.push(Arc::new(ClaimFence {
        manifest: manifest(),
    }));
    registry.session_halt.push(Arc::new(PauseOnHalt {
        manifest: manifest(),
    }));
}
