use super::*;
use gents_lean_vocab::{
    lean_lifecycle_hook_goal_resume_cases, LeanGoalResumeDecision, LeanGoalResumeGoal,
    LeanGoalResumeReference, LeanGoalResumeResult, LeanGoalResumeStatus,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::requested_create::REQUEST_CREATE;
use gents_store::identity::{KeyIdentity, NodeIdentity};

const SESSION: &str = "session-resumed";
const SESSION_DOC: &str = "session-doc";
const GOAL_DOC: &str = "goal-doc";
const AT: &str = "2026-11-11T12:00:00Z";

fn doc(id: u64) -> String {
    format!("physical-{id}")
}

fn goal_id(goal: u64) -> String {
    format!("goal-{goal}")
}

fn status(status: LeanGoalResumeStatus) -> &'static str {
    match status {
        LeanGoalResumeStatus::Active => "active",
        LeanGoalResumeStatus::Paused => "paused",
        LeanGoalResumeStatus::Complete => "complete",
    }
}

fn reference(lean: &LeanGoalResumeReference) -> GoalAssignmentInput {
    GoalAssignmentInput {
        goal_id: goal_id(lean.goal),
        root_request_doc_id: lean.root.map(doc),
    }
}

fn goal_row(node: &str, agent: &str, goal: &LeanGoalResumeGoal) -> Row {
    Row {
        doc: DocRef::new("Goal", GOAL_DOC),
        revision: 1,
        value: serde_json::json!({
            "_docID": GOAL_DOC,
            "goal_id": goal_id(goal.goal),
            "session_id": SESSION,
            "session_doc_id": SESSION_DOC,
            "node_did": node,
            "agent_did": agent,
            "objective": "finish the resumed work",
            "status": status(goal.status),
            "assignment_root_request_doc_id": goal.root.map(doc),
            "continuation_sequence": 0,
            "tokens_used": 0,
            "active_time_seconds": 0,
            "consecutive_blocked_audits": 0,
            "wrapup_requested": false,
            "wrapup_completed": false,
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z",
        }),
    }
}

fn session_row(node: &str) -> Row {
    Row {
        doc: DocRef::new("AgentSession", SESSION_DOC),
        revision: 1,
        value: serde_json::json!({
            "_docID": SESSION_DOC,
            "session_id": SESSION,
            "node_did": node,
            "requester_did": node,
            "agent_did": node,
            "created_at": "2026-11-11T00:00:00Z",
        }),
    }
}

fn goal_of(value: &serde_json::Value) -> LeanGoalResumeGoal {
    let goal = value["goal_id"]
        .as_str()
        .and_then(|id| id.strip_prefix("goal-"))
        .and_then(|id| id.parse().ok())
        .expect("generated goal identity");
    let status = match value["status"].as_str() {
        Some("active") => LeanGoalResumeStatus::Active,
        Some("paused") => LeanGoalResumeStatus::Paused,
        Some("complete") => LeanGoalResumeStatus::Complete,
        other => panic!("unmodelled Goal status {other:?}"),
    };
    let root = value["assignment_root_request_doc_id"]
        .as_str()
        .map(|root| root.strip_prefix("physical-").unwrap().parse().unwrap());
    LeanGoalResumeGoal { goal, status, root }
}

fn written(effects: &[Effect]) -> Option<&serde_json::Value> {
    effects.iter().find_map(|effect| match effect {
        Effect::Write(Write::Update { doc, value, .. }) if doc.doc_id == GOAL_DOC => Some(value),
        _ => None,
    })
}

fn created(effects: &[Effect]) -> Option<AgentRequestCreate> {
    effects.iter().find_map(|effect| match effect {
        Effect::Core(operation) if operation.name == REQUEST_CREATE => {
            Some(serde_json::from_value(operation.args["request"].clone()).unwrap())
        }
        _ => None,
    })
}

fn merged(row: &Row, input: Option<&serde_json::Value>) -> Row {
    let mut next = row.clone();
    if let (Some(into), Some(serde_json::Value::Object(fields))) =
        (next.value.as_object_mut(), input)
    {
        into.extend(fields.clone());
        next.revision += 1;
    }
    next
}

async fn request_rows(
    identity: &KeyIdentity,
    case: &gents_lean_vocab::LeanLifecycleHookGoalResumeCase,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, request) in case.requests.iter().enumerate() {
        assert!(
            request.trigger.is_none() && request.reference.is_none(),
            "{}: generated predecessors are client requests",
            case.name
        );
        let created_at = (chrono::DateTime::parse_from_rfc3339("2026-11-11T11:00:00Z").unwrap()
            - chrono::Duration::seconds(i64::try_from(index).unwrap()))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("request-{}", request.id),
            identity.did(),
            identity.did(),
            identity.did(),
            SESSION,
            "resumed predecessor",
            "interactive",
            &created_at,
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        gents_session::request::admission::sign_agent_request_create(identity, &mut create)
            .await
            .unwrap();
        let mut row = super::super::request_head::tests::signed_row(&create, request.id);
        if !request.terminal {
            row.lifecycle_state = Some(RequestLifecycleState::Processing);
        }
        let mut value = serde_json::to_value(row).unwrap();
        value["_docID"] = doc(request.id).into();
        rows.push(Row {
            doc: DocRef::new("AgentRequest", doc(request.id)),
            revision: 1,
            value,
        });
    }
    rows
}

fn assert_decision(
    name: &str,
    point: &str,
    decided: &Result<Vec<Effect>>,
    expected: &LeanGoalResumeDecision,
    before: &Row,
) {
    match (decided, expected) {
        (Err(_), LeanGoalResumeDecision::Refuse { .. }) => {}
        (Ok(effects), LeanGoalResumeDecision::Proceed { goal, .. }) => {
            assert_eq!(
                written(effects).map(|input| goal_of(&merged(before, Some(input)).value)),
                goal.clone(),
                "{name}: {point} Goal write"
            );
        }
        (actual, expected) => panic!("{name}: {point} decided {actual:?}, model {expected:?}"),
    }
}

/// Lean `Hooks.GoalResume`: the Goals listeners at `control.session.resume`
/// and, after core creates the continuation, at `request.create.stage`.
#[tokio::test]
async fn generated_goal_resume_cases_drive_the_resume_and_binding_listeners() {
    let cases = lean_lifecycle_hook_goal_resume_cases();
    assert_eq!(cases.len(), 5);
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap();
    let node = identity.did().to_owned();
    for case in cases {
        let goal = goal_row(&node, &node, &case.goal);
        let sessions = vec![session_row(&node)];
        let requests = request_rows(&identity, case).await;
        let event = SessionResume {
            node_did: node.clone(),
            session_id: SESSION.into(),
            at: AT.into(),
        };
        let restaged = RestageOnResume::effects(
            &event,
            &[sessions.clone(), vec![goal.clone()], requests.clone()],
        );
        assert_decision(&case.name, "resume", &restaged, &case.restage, &goal);
        let effects = restaged.unwrap_or_default();
        let resumed = merged(&goal, written(&effects));
        let continuation = created(&effects);
        let expected_create = match &case.restage {
            LeanGoalResumeDecision::Proceed { create, .. } => create.clone(),
            LeanGoalResumeDecision::Refuse { .. } => None,
        };
        assert_eq!(
            continuation.as_ref().map(|create| (
                create.caused_by_trigger_kind.clone(),
                create.caused_by_trigger_id.clone(),
                create.input.goal_assignment.clone(),
            )),
            expected_create.as_ref().map(|create| (
                Some(GOAL_TRIGGER_KIND.to_owned()),
                create.trigger.map(goal_id),
                create.reference.as_ref().map(reference),
            )),
            "{}: requested continuation",
            case.name
        );
        let mut current = resumed.clone();
        match (continuation, &case.bind, expected_create) {
            (Some(request), Some(expected), Some(create)) => {
                let event = RequestCreated {
                    request,
                    doc_id: doc(create.id),
                };
                let reads = BindResumedRoot {
                    manifest: manifest(),
                }
                .reads(&event);
                assert_eq!(reads.len(), 2, "{}: binding reads", case.name);
                let bound =
                    BindResumedRoot::effects(&event, &[sessions.clone(), vec![resumed.clone()]]);
                assert_decision(&case.name, "binding", &bound, expected, &resumed);
                current = merged(&resumed, bound.ok().as_deref().and_then(written));
            }
            (None, None, None) => {}
            (actual, expected, _) => {
                panic!("{}: created {actual:?}, model binding {expected:?}", case.name)
            }
        }
        match &case.result {
            LeanGoalResumeResult::Ok { goals, requests: after } => {
                assert_eq!(
                    vec![goal_of(&current.value)],
                    *goals,
                    "{}: Goal after the resume",
                    case.name
                );
                assert_eq!(
                    after.len(),
                    case.requests.len() + usize::from(case.bind.is_some()),
                    "{}: requests after the resume",
                    case.name
                );
            }
            LeanGoalResumeResult::Error { .. } => assert!(
                matches!(case.restage, LeanGoalResumeDecision::Refuse { .. }),
                "{}: a failed resume is a refusal",
                case.name
            ),
        }
    }
}
