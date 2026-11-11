//! Goals at the lifecycle hook points. Goals are a bundled plugin: each hook
//! declares its reads, decides on the rows it is shown and returns its writes
//! to the Goal collection, or a named core operation, as data.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gents_protocol::session::SESSION_SCOPE_FIELDS;

use super::*;
use gents_protocol::goal_names::GOAL_TRIGGER_KIND;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::SessionOwnerRow;
use gents_session::request::hooks::claim::{ClaimVerdict, Successor};
use gents_session::request::hooks::effect::{
    DocRef, Effect, Grant, Manifest, OwnedCollection, Row, Subject, Write, SESSION_CHILD_GRANTS,
};
use gents_session::request::hooks::hook::{Direction, Hook, Outcome, Query};
use gents_session::request::hooks::operations::{supersede_pending, Selector};
use gents_session::request::hooks::prepared::{Decide, Verdict};
use gents_session::request::hooks::queue_scope::{ScopeAnswer, ScopeAnswers, ScopeFailure};
use gents_session::request::hooks::registry::{
    AdmitShown, ClaimApplied, ClaimCandidate, QueueScopeAsk, Registry, SessionHalt, SessionResume,
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
            grants: SESSION_CHILD_GRANTS,
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

/// The session the rows were read for, if it is visible
/// (`load_agent_session_owner_in_txn` on the same rows).
fn canonical_session(session_id: &str, sessions: &[Row]) -> Result<Option<SessionOwnerRow>> {
    let response = serde_json::json!({
        "data": { "AgentSession": sessions.iter().map(|row| &row.value).collect::<Vec<_>>() }
    });
    Ok(match decode_agent_session_read(&response, session_id)? {
        AgentSessionRowRead::Present(session) => Some(*session),
        AgentSessionRowRead::Absent => None,
    })
}

/// The canonical Goal of the session the rows were read for, with the row it
/// was shown as (the decision `load_canonical_goal_in_txn` makes on the same
/// rows).
pub(super) fn canonical_goal<'a>(
    session_id: &str,
    sessions: &[Row],
    goals: &'a [Row],
) -> Result<Option<(GoalDocument, &'a Row)>> {
    let Some(session) = canonical_session(session_id, sessions)? else {
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
                conflict: None,
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

/// `control.session.resume`: a paused Goal resumes with its session. Busy, it
/// resumes the work the session holds; idle, it continues from its terminal
/// predecessor through core's `request.create` (Lean `Hooks.Resume`).
struct RestageOnResume {
    manifest: Manifest,
}

/// Why the plugin cannot resume a Goal that has no assignment root. Binding
/// the continuation as the root needs the document ID core gives the created
/// request, which no effect of this point can name.
const UNBOUND_RESUME: &str =
    "goal has no assignment root to bind its resumed continuation to; resume was not applied";

impl RestageOnResume {
    fn effects(event: &SessionResume, rows: &[Vec<Row>]) -> Result<Vec<Effect>> {
        let [sessions, goals, requests] = rows else {
            bail!("Goal resume was not shown its declared rows");
        };
        let Some((goal, row)) = canonical_goal(&event.session_id, sessions, goals)? else {
            return Ok(Vec::new());
        };
        let state = goal.state().context("Goal has an unknown status")?;
        if state.status != GoalStatus::Paused {
            return Ok(Vec::new());
        }
        let requests = requests
            .iter()
            .map(|row| serde_json::from_value::<AgentRequestRow>(row.value.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("decoding the session's requests")?;
        let now = DateTime::parse_from_rfc3339(&event.at)
            .context("resume time is not RFC 3339")?
            .with_timezone(&Utc);
        let prepared = client_control::prepare_goal_resume(goal, state, &requests)
            .map_err(|error| anyhow::anyhow!("{error}; resume was not applied"))?;
        match prepared {
            client_control::PreparedGoalControl::ResumeCurrent(goal) => {
                let post = operator_resume::busy_resume_post(&goal)?;
                Ok(vec![Effect::Write(Write::Update {
                    doc: row.doc.clone(),
                    expected: row.revision,
                    value: operator_resume::resume_state_input(&goal, post, None, now),
                    conflict: None,
                })])
            }
            client_control::PreparedGoalControl::ResumeIdle { goal, predecessor } => {
                let parent_row = operator_resume::resume_predecessor(&requests, &predecessor)?;
                let parent = gents_protocol::row::AgentRequest::try_from(parent_row.clone())?;
                let key = continuation::goal_continuation_identity(&goal.goal_id, &predecessor, 1)?
                    .retry_key;
                let children = requests
                    .iter()
                    .filter(|row| row.retry_key.as_deref() == Some(key.as_str()))
                    .collect::<Vec<_>>();
                anyhow::ensure!(children.len() <= 1, "ambiguous goal continuation receipt");
                if let Some(child) = children.first() {
                    request_head::verify_goal_continuation_receipt(&goal, parent_row, child)?;
                    return Ok(Vec::new());
                }
                if goal.assignment_root_request_doc_id.is_none() {
                    bail!(UNBOUND_RESUME);
                }
                let agent = parent.agent_did.clone();
                let resumed = operator_resume::decide_idle_resume(
                    &goal, &requests, parent_row, &parent, agent, now, 0,
                )?;
                Ok(vec![
                    Effect::Write(Write::Update {
                        doc: row.doc.clone(),
                        expected: row.revision,
                        value: operator_resume::resume_state_input(
                            &goal,
                            resumed.post,
                            Some((resumed.sequence, &predecessor)),
                            now,
                        ),
                        conflict: None,
                    }),
                    gents_session::request::lifecycle::requested_create::request_create(
                        resumed.create,
                    )?,
                ])
            }
            _ => bail!("goal resume prepared another control"),
        }
    }
}

impl Hook<SessionResume> for RestageOnResume {
    type Value = ();

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, event: &SessionResume) -> Vec<Query> {
        let [session, goals] = canonical_goal_reads(&event.node_did, &event.session_id);
        vec![
            session,
            goals,
            Query::select("AgentRequest", request_head::request_selection_fields())
                .eq("node_did", event.node_did.clone())
                .eq("session_id", event.session_id.clone())
                .order_by("created_at", Direction::Descending)
                .order_by("request_id", Direction::Descending),
        ]
    }

    fn decide(&self, event: &SessionResume, rows: &[Vec<Row>]) -> Outcome<()> {
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

/// `request.claim.applied`: a claimed Task fire's Goal assignment replaces the
/// session's Goal binding (Lean `Hooks.GoalTask.applyTaskGoal`): settle the
/// previous Goal's fire outcomes, create the session's Goal with its
/// session-child grants or move the shown one to this assignment, have core
/// supersede the continuations the assignment makes stale, mark the fire
/// applied, and settle again.
struct TaskGoalAssignment {
    manifest: Manifest,
}

#[derive(Deserialize)]
struct TaskFire {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    goal_id: Option<String>,
    goal_objective: Option<String>,
    goal_token_budget: Option<i64>,
    #[serde(default)]
    goal_assignment_applied: bool,
}

/// The Goal fields a move to a new assignment writes on a Goal that was not
/// active: its accounting restarts at `baseline`.
fn restarted_goal_fields(at: &str, baseline: i64) -> serde_json::Value {
    serde_json::json!({
        "tokens_used": 0,
        "token_usage_baseline": baseline,
        "active_time_seconds": 0,
        "active_started_at": at,
        "consecutive_blocked_audits": 0,
        "last_blocked_request_id": null,
        "last_blocked_reason": null,
        "last_continued_from_request_id": null,
        "wrapup_requested": false,
        "wrapup_completed": false,
        "infrastructure_retry_count": 0,
        "last_failure": null,
        "completion_evidence": null,
    })
}

fn merge(into: &mut serde_json::Value, fields: serde_json::Value) {
    if let (Some(into), serde_json::Value::Object(fields)) = (into.as_object_mut(), fields) {
        into.extend(fields);
    }
}

impl TaskGoalAssignment {
    /// The fire whose Goal assignment this claim applies, if any.
    fn pending(event: &ClaimApplied) -> Result<Option<TaskFire>> {
        let Some(fire) = &event.fire else {
            return Ok(None);
        };
        let fire: TaskFire =
            serde_json::from_value(fire.clone()).context("decoding the Task fire receipt")?;
        Ok((fire.goal_id.is_some() && !fire.goal_assignment_applied).then_some(fire))
    }

    fn effects(event: &ClaimApplied, rows: &[Vec<Row>]) -> Result<Vec<Effect>> {
        let request = &event.request;
        let at = event.at.as_str();
        let Some(fire) = Self::pending(event)? else {
            return Ok(Vec::new());
        };
        anyhow::ensure!(
            fire.session_id == request.session_id,
            "Task receipt session differs from claimed request"
        );
        let [sessions, goals, usage] = rows else {
            bail!("Task Goal assignment was not shown its declared rows");
        };
        let previous = canonical_goal(&request.session_id, sessions, goals)?;
        let goal_id = fire
            .goal_id
            .as_deref()
            .context("Task assignment lost Goal identity")?;
        anyhow::ensure!(
            goal_id
                == task_assignment_goal_id(
                    previous.as_ref().map(|(goal, _)| goal.goal_id.as_str()),
                    &request.node_did,
                    &request.session_id,
                )
                && request
                    .input
                    .goal_assignment
                    .as_ref()
                    .is_none_or(|reference| reference.goal_id == goal_id
                        && reference.root_request_doc_id.is_none()),
            "Task Goal assignment changed before publication or claim"
        );
        let objective = fire
            .goal_objective
            .as_deref()
            .map(str::trim)
            .filter(|objective| !objective.is_empty())
            .context("Goal-backed Task has no objective")?;
        let budget = fire.goal_token_budget;
        anyhow::ensure!(
            budget.is_none_or(|budget| budget > 0),
            "Task Goal budget must be positive"
        );
        let baseline = || {
            usage
                .first()
                .and_then(|row| row.value.get("total"))
                .and_then(serde_json::Value::as_i64)
                .context("the session usage read returned no total")
        };
        let mut effects = Vec::new();
        if let Some((goal, _)) = &previous {
            anyhow::ensure!(
                goal.parsed_status().is_some(),
                "existing Task Goal has an unknown status"
            );
            effects.push(crate::triggers::lifecycle_hooks::settle_goal_outcomes(
                &goal.node_did,
                &goal.goal_id,
                goal.last_blocked_reason.as_deref().unwrap_or(&goal.status),
                at,
            ));
        }
        let assigned = match &previous {
            None => {
                let session = canonical_session(&request.session_id, sessions)?
                    .context("goal creation requires a visible physical session")?;
                let creator = event
                    .actor
                    .as_deref()
                    .context("goal creation requires an authenticated actor")?;
                let mut value = serde_json::json!({
                    "goal_id": deterministic_goal_id(&request.node_did, &request.session_id),
                    "session_id": request.session_id,
                    "session_doc_id": session.doc_id,
                    "agent_did": session.session.agent_did,
                    "creator_did": creator,
                    "parent_linked": true,
                    "requester_did": session.session.requester_did,
                    "node_did": request.node_did,
                    "objective": objective,
                    "status": GoalStatus::Active.as_str(),
                    "token_budget": budget,
                    "continuation_sequence": 0,
                    "assignment_root_request_doc_id": request.doc_id,
                    "auto_resume_at_reset": null,
                    "created_at": at,
                    "updated_at": at,
                });
                merge(&mut value, restarted_goal_fields(at, baseline()?));
                effects.push(Effect::Write(Write::Create {
                    doc: DocRef::new("Goal", ""),
                    value: value.clone(),
                    grants: vec![
                        Grant {
                            relation: "parent".into(),
                            subject: Subject::Document(DocRef::new(
                                "AgentSession",
                                session.doc_id.clone(),
                            )),
                        },
                        Grant {
                            relation: "working_agent".into(),
                            subject: Subject::Actor(session.session.agent_did.clone()),
                        },
                    ],
                }));
                merge(&mut value, serde_json::json!({ "_docID": "" }));
                value
            }
            Some((goal, row)) => {
                let next_sequence = goal
                    .continuation_sequence()
                    .checked_add(1)
                    .context("Task Goal continuation sequence exhausted")?;
                let mut input = if goal.parsed_status() == Some(GoalStatus::Active) {
                    let now = DateTime::parse_from_rfc3339(at)
                        .context("claim time is not RFC 3339")?
                        .with_timezone(&Utc);
                    let pre = goal
                        .state()
                        .context("existing Goal has an unknown status")?;
                    let post = apply_operator_status_transition(pre, GoalStatus::Active)?;
                    serde_json::json!({
                        "tokens_used": goal.tokens_used.unwrap_or_default().max(0),
                        "active_time_seconds": goal.current_active_time_seconds(now),
                        "active_started_at": post.status.accrues_active_time().then_some(at),
                        "consecutive_blocked_audits": post.blocked_audits,
                        "wrapup_requested": post.wrapup_requested,
                        "wrapup_completed": post.wrapup_completed,
                    })
                } else {
                    restarted_goal_fields(at, baseline()?)
                };
                merge(
                    &mut input,
                    serde_json::json!({
                        "objective": objective,
                        "status": GoalStatus::Active.as_str(),
                        "token_budget": budget,
                        "assignment_root_request_doc_id": request.doc_id,
                        "continuation_sequence": next_sequence,
                        "updated_at": at,
                    }),
                );
                effects.push(Effect::Write(Write::Update {
                    doc: row.doc.clone(),
                    expected: row.revision,
                    value: input.clone(),
                    conflict: Some("Task Goal assignment lost its continuation fence".into()),
                }));
                let mut value = row.value.clone();
                merge(&mut value, input);
                value
            }
        };
        let assigned: GoalDocument =
            serde_json::from_value(assigned).context("decoding the assigned Task Goal")?;
        effects.push(supersede_pending(&Selector {
            node_did: assigned.node_did.clone(),
            session_id: assigned.session_id.clone(),
            goal_id: assigned.goal_id.clone(),
            root_doc_id: request.doc_id.clone(),
            successor: request.request_id.clone(),
            successor_doc: request.doc_id.clone(),
            reason: request_head::STALE_ASSIGNMENT_REASON.into(),
            at: at.to_owned(),
        }));
        effects.push(crate::triggers::lifecycle_hooks::apply_goal_assignment(
            &fire.doc_id,
        ));
        effects.push(crate::triggers::lifecycle_hooks::settle_goal_outcomes(
            &assigned.node_did,
            &assigned.goal_id,
            "Task Goal assignment replaced",
            at,
        ));
        Ok(effects)
    }
}

impl Hook<ClaimApplied> for TaskGoalAssignment {
    type Value = ();

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// A request without a pending Task Goal assignment declares nothing:
    /// core already read its fire receipt for the event.
    fn reads(&self, event: &ClaimApplied) -> Vec<Query> {
        if !matches!(Self::pending(event), Ok(Some(_))) {
            return Vec::new();
        }
        let request = &event.request;
        let [session, goals] = canonical_goal_reads(&request.node_did, &request.session_id);
        vec![
            session,
            goals,
            Query::core(
                gents_tools::native::SESSION_TOKEN_USAGE,
                serde_json::json!({
                    "node_did": request.node_did,
                    "session_id": request.session_id,
                }),
            ),
        ]
    }

    fn decide(&self, event: &ClaimApplied, rows: &[Vec<Row>]) -> Outcome<()> {
        match Self::effects(event, rows) {
            Ok(effects) => Outcome::effects(effects),
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
    registry.claim_applied.push(Arc::new(TaskGoalAssignment {
        manifest: manifest(),
    }));
    registry.session_resume.push(Arc::new(RestageOnResume {
        manifest: manifest(),
    }));
}
