use super::*;
use gents_lean_vocab::families::hooks_contracts_goal_resume as wire;
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

fn goal_row(node: &str, agent: &str, goal: &wire::Goal<String>) -> Row {
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
            "status": goal.status,
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

fn number(value: &str, prefix: &str) -> u64 {
    value.strip_prefix(prefix).unwrap().parse().unwrap()
}

fn goal_of(value: &serde_json::Value) -> wire::Goal<String> {
    let status = value["status"].as_str().unwrap();
    assert!(
        matches!(status, "active" | "paused" | "complete"),
        "unmodelled Goal status {status}"
    );
    wire::Goal {
        goal: number(value["goal_id"].as_str().unwrap(), "goal-"),
        status: status.to_owned(),
        root: value["assignment_root_request_doc_id"]
            .as_str()
            .map(|root| number(root, "physical-")),
    }
}

fn reference_of(input: &Option<GoalAssignmentInput>) -> Option<wire::ResumeReferenceWire> {
    input.as_ref().map(|reference| wire::ResumeReferenceWire {
        goal: number(&reference.goal_id, "goal-"),
        root: reference
            .root_request_doc_id
            .as_deref()
            .map(|root| number(root, "physical-")),
    })
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
    case: &wire::GoalResumeInput<String>,
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

fn request_wire(row: &Row) -> wire::ResumeRequestWire {
    let request: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(row.value.clone()).unwrap();
    wire::ResumeRequestWire {
        id: number(request.doc_id.as_deref().unwrap(), "physical-"),
        terminal: request.lifecycle_state != Some(RequestLifecycleState::Processing),
        trigger: request
            .caused_by_trigger_id
            .as_deref()
            .map(|goal| number(goal, "goal-")),
        reference: reference_of(&request.input.and_then(|input| input.goal_assignment)),
    }
}

fn decision(
    decided: &Result<Vec<Effect>>,
    before: &Row,
    created_doc: u64,
) -> wire::ResumeDecisionWire<String> {
    match decided {
        Err(error) => wire::ResumeDecisionWire {
            kind: "refuse".into(),
            reason: Some(error.to_string()),
            goal: None,
            create: None,
        },
        Ok(effects) => wire::ResumeDecisionWire {
            kind: "proceed".into(),
            reason: None,
            goal: written(effects).map(|input| goal_of(&merged(before, Some(input)).value)),
            create: created(effects).map(|create| {
                assert_eq!(
                    create.caused_by_trigger_kind.as_deref(),
                    create.caused_by_trigger_id.as_ref().map(|_| GOAL_TRIGGER_KIND),
                    "a requested continuation names the Goal trigger kind"
                );
                wire::ResumeCreateWire {
                    id: created_doc,
                    trigger: create
                        .caused_by_trigger_id
                        .as_deref()
                        .map(|goal| number(goal, "goal-")),
                    reference: reference_of(&create.input.goal_assignment),
                }
            }),
        },
    }
}

/// Lean `Hooks.GoalResume`: the Goals listeners at `control.session.resume`
/// and, after core creates the continuation, at `request.create.stage`.
#[tokio::test]
async fn generated_goal_resume_cases_drive_the_resume_and_binding_listeners() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap();
    let node = identity.did().to_owned();
    for modeled in gents_lean_vocab::families::hooks_contracts_goal_resume::cases::<String>() {
        let case = &modeled.input;
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
        let restage = decision(&restaged, &goal, case.next);
        let refused = restaged.as_ref().err().map(ToString::to_string);
        let effects = restaged.unwrap_or_default();
        let resumed = merged(&goal, written(&effects));
        let continuation = created(&effects);
        let mut current = resumed.clone();
        let bind = continuation.clone().map(|request| {
            let event = RequestCreated {
                request,
                doc_id: doc(case.next),
            };
            let reads = BindResumedRoot {
                manifest: manifest(),
            }
            .reads(&event);
            assert_eq!(reads.len(), 2, "{}: binding reads", case.name);
            let bound =
                BindResumedRoot::effects(&event, &[sessions.clone(), vec![resumed.clone()]]);
            let decided = decision(&bound, &resumed, case.next);
            current = merged(&resumed, bound.ok().as_deref().and_then(written));
            decided
        });
        let result = match refused {
            Some(reason) => wire::ResumeResultWire {
                kind: "error".into(),
                goals: Vec::new(),
                requests: Vec::new(),
                refusal: Some(wire::ResumeRefusalWire {
                    kind: "refused".into(),
                    plugin: Some(manifest().name.into()),
                    reason: Some(reason),
                }),
            },
            None => wire::ResumeResultWire {
                kind: "ok".into(),
                goals: vec![goal_of(&current.value)],
                requests: continuation
                    .iter()
                    .map(|create| wire::ResumeRequestWire {
                        id: case.next,
                        terminal: false,
                        trigger: create
                            .caused_by_trigger_id
                            .as_deref()
                            .map(|goal| number(goal, "goal-")),
                        reference: reference_of(&create.input.goal_assignment),
                    })
                    .chain(requests.iter().map(request_wire))
                    .collect(),
                refusal: None,
            },
        };
        modeled.check(wire::GoalResumeOutput {
            restage,
            bind,
            result,
        });
    }
}
