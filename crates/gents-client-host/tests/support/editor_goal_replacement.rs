use super::{fake_llm, ChatAction};
use crate::support::{TestError, TestResult};
use gents::{graphql::escape_graphql_string, ConfigAccess, NodeIdentity};
use gents_protocol::{
    goal_control::{
        GoalControlOutcome, GoalOperation, SessionGoalControl, SessionGoalControlReceipt,
    },
    request_input::{CapturedContextKind, CapturedContextSource},
    request_lifecycle::RequestLifecycleState,
    row::AgentRequestRow,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const OBJECTIVES: [&str; 3] = [
    "Verify first goal replacement in this chat",
    "Verify second goal replacement in this chat",
    "Verify third goal replacement in this chat",
];
pub const DRAFT: &str = "Keep this unsent replacement draft";
pub const QUEUED: &str = "Run this queued follow-up before replacing the goal";
pub const FOLLOWUP: &str = "Complete the second goal from this follow-up";
pub const OBSERVER_DRAFT: &str = "Keep this observer replacement draft";
pub const OBSERVER_EDIT: &str = "Keep this stale replacement goal edit";
pub const CAPTURE: &str = "replacement goal captured selection";
pub const OBSERVER_CAPTURE: &str = "private observer replacement selection";
const CAPTURE_FILE: &str = "context.rs";
const OBSERVER_CAPTURE_FILE: &str = "goal-observer/context.rs";
const CAPTURE_DISK: &str = "replacement disk context";
const OBSERVER_CAPTURE_DISK: &str = "observer disk context";
const WORK_RUNNING: [&str; 3] = [
    "First goal work is running.",
    "Second goal work is running.",
    "Third goal work is running.",
];
const WORK_FINISHED: [&str; 3] = [
    "First goal work finished.",
    "Second goal work finished.",
    "Third goal work finished.",
];
const QUEUED_FINISHED: &str = "Queued follow-up finished before replacement.";
const COMPLETED_REPLY: &str = "Second goal completed.";
const COMPLETION_EVIDENCE: &str = "Second goal completed through its signed follow-up.";
const COMPLETE_CALL: &str = "goal-replacement-complete-second";
const BUSY_REASON: &str = "wait for this session to finish before starting a goal";

pub fn prepare(root: &Path) -> TestResult<()> {
    std::fs::create_dir_all(root.join("goal-observer"))?;
    std::fs::write(root.join(CAPTURE_FILE), CAPTURE_DISK)?;
    std::fs::write(root.join(OBSERVER_CAPTURE_FILE), OBSERVER_CAPTURE_DISK)?;
    std::fs::write(
        root.join("editor-goal-replacement-fixture.json"),
        serde_json::to_vec_pretty(&json!({
            "objectives": OBJECTIVES, "draft": DRAFT, "queued": QUEUED,
            "followup": FOLLOWUP, "observerDraft": OBSERVER_DRAFT,
            "observerEdit": OBSERVER_EDIT, "capture": CAPTURE,
            "observerCapture": OBSERVER_CAPTURE, "captureFile": CAPTURE_FILE,
            "observerCaptureFile": OBSERVER_CAPTURE_FILE,
            "captureDisk": CAPTURE_DISK, "observerCaptureDisk": OBSERVER_CAPTURE_DISK,
            "workRunning": WORK_RUNNING, "workFinished": WORK_FINISHED,
            "queuedFinished": QUEUED_FINISHED, "completedReply": COMPLETED_REPLY,
            "busyReason": BUSY_REASON, "completionEvidence": COMPLETION_EVIDENCE,
        }))?,
    )?;
    Ok(())
}

pub struct Provider {
    gates: [Arc<tokio::sync::Semaphore>; 3],
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            gates: std::array::from_fn(|_| Arc::new(tokio::sync::Semaphore::new(0))),
        }
    }
}

impl Provider {
    pub fn reply(
        &self,
        request: &Value,
        emitted: &tokio::sync::watch::Sender<Option<Instant>>,
    ) -> Option<ChatAction> {
        if current_input_is(request, "Return the fixture response.") {
            return None;
        }
        if current_input_is(request, QUEUED) {
            return Some(ChatAction::Sse(fake_llm::completion_text_sse(
                QUEUED_FINISHED,
            )));
        }
        if current_input_is(request, FOLLOWUP) {
            let completed = request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "tool" && message["tool_call_id"] == COMPLETE_CALL
                })
            });
            return Some(ChatAction::Sse(if completed {
                fake_llm::completion_text_sse(COMPLETED_REPLY)
            } else {
                fake_llm::tool_call_sse_with_id(
                    COMPLETE_CALL,
                    gents::goal::UPDATE_GOAL_TOOL_NAME,
                    &json!({"status":"complete","reason":COMPLETION_EVIDENCE}).to_string(),
                )
            }));
        }
        for (index, objective) in OBJECTIVES.iter().enumerate() {
            if current_input_is(request, objective) {
                return Some(ChatAction::GatedSse(
                    vec![(
                        Duration::ZERO,
                        format!(
                            "data: {}\n\n",
                            json!({
                                "choices":[{"delta":{"content":WORK_RUNNING[index]},"finish_reason":null}],
                                "usage":null,
                            })
                        ),
                    )],
                    Arc::clone(&self.gates[index]),
                    fake_llm::completion_text_sse(&format!(" {}", WORK_FINISHED[index])),
                    emitted.clone(),
                ));
            }
        }
        Some(ChatAction::Sse(fake_llm::completion_text_sse(
            "Goal replacement checks",
        )))
    }

    pub fn release(&self, index: usize) {
        self.gates[index].add_permits(1);
    }
}

fn current_input_is(request: &Value, expected: &str) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message["role"] == "user")
        .is_some_and(|message| match &message["content"] {
            Value::String(text) => text == expected,
            Value::Array(parts) => parts.iter().any(|part| part["text"] == expected),
            _ => false,
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    access: &ConfigAccess,
    provider: &fake_llm::FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
    identity: &dyn NodeIdentity,
) -> TestResult<Value> {
    let owner = escape_graphql_string(node);
    let session_literal = escape_graphql_string(session);
    let scope =
        format!("node_did: {{_eq: \"{owner}\"}}, session_id: {{_eq: \"{session_literal}\"}}");
    let query = format!(
        r#"{{
        AgentRequest(filter: {{{scope}, purpose: {{_eq: "normal"}}}}) {{{} interrupt_requested_at}}
        Goal(filter: {{{scope}}}, showDeleted: true) {{{}}}
        GoalCreationClaim(filter: {{{scope}}}) {{_docID goal_id objective token_budget}}
        AgentSessionGoalControl(filter: {{{scope}}}) {{_docID intent_json receipt_json}}
        AgentSession(filter: {{{scope}}}) {{_docID session_id node_did requester_did}}
    }}"#,
        gents::SIGNED_REQUEST_FIELDS,
        gents::goal::GOAL_FIELDS
    );
    let value = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = access.execute(&query).await?;
            if value["data"]["AgentRequest"]
                .as_array()
                .is_some_and(|rows| {
                    rows.len() == 6 && rows.iter().all(|row| row["lifecycle_state"] == "completed")
                })
                && value["data"]["Goal"].as_array().is_some_and(|goals| {
                    goals.len() == 3
                        && goals.iter().any(|goal| {
                            goal["objective"] == OBJECTIVES[2] && goal["status"] == "stopped"
                        })
                })
            {
                return Ok::<_, TestError>(value);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect(
        "same-chat replacement did not settle as six owned requests and three retained Goals",
    )?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value["data"]["AgentRequest"].clone())?;
    let sessions = value["data"]["AgentSession"]
        .as_array()
        .expect("session rows");
    assert!(
        sessions.len() == 1 && sessions[0]["requester_did"] == author,
        "replacement changed its physical AgentSession: {value}"
    );
    let session_doc = sessions[0]["_docID"].as_str().expect("physical session");
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        assert!(
            row.node_did.as_deref() == Some(node)
                && row.session_id.as_deref() == Some(session)
                && row.requester_did.as_deref() == Some(author)
                && row.admission_signer_did.as_deref() == Some(author),
            "replacement changed signed request scope"
        );
        assert!(
            row.lifecycle_state == Some(RequestLifecycleState::Completed)
                && row.interrupt_requested_at.is_none(),
            "replacement interrupted owned work"
        );
        assert!(
            row.input
                .as_ref()
                .and_then(|input| input.goal_continuation.as_ref())
                .is_none(),
            "replacement admitted autonomous continuation"
        );
    }
    let request = |content: &str| -> TestResult<&AgentRequestRow> {
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| row.content.as_deref() == Some(content))
            .collect();
        assert!(
            matching.len() == 1,
            "expected one immutable authored request for {content:?}"
        );
        Ok(matching[0])
    };
    let initial = request("Return the fixture response.")?;
    let queued = request(QUEUED)?;
    let followup = request(FOLLOWUP)?;
    let openings = OBJECTIVES
        .map(request)
        .into_iter()
        .collect::<TestResult<Vec<_>>>()?;
    let goals: Vec<gents::goal::GoalDocument> =
        serde_json::from_value(value["data"]["Goal"].clone())?;
    let selected_goals = OBJECTIVES
        .map(|objective| -> TestResult<&gents::goal::GoalDocument> {
            let matching: Vec<_> = goals
                .iter()
                .filter(|goal| goal.objective == objective)
                .collect();
            assert!(
                matching.len() == 1,
                "expected one retained Goal for {objective:?}"
            );
            Ok(matching[0])
        })
        .into_iter()
        .collect::<TestResult<Vec<_>>>()?;
    assert!(
        selected_goals
            .iter()
            .map(|goal| &goal.goal_id)
            .collect::<BTreeSet<_>>()
            .len()
            == 3
            && selected_goals
                .iter()
                .map(|goal| &goal.doc_id)
                .collect::<BTreeSet<_>>()
                .len()
                == 3,
        "replacement reused a logical or physical Goal identity"
    );
    assert!(
        openings
            .iter()
            .map(|row| &row.doc_id)
            .collect::<BTreeSet<_>>()
            .len()
            == 3,
        "replacement reused an opening request"
    );
    for (index, (goal, opening)) in selected_goals.iter().zip(&openings).enumerate() {
        let opening_doc = opening.doc_id.as_deref().expect("physical Goal opening");
        assert!(
            goal.node_did == node
                && goal.session_id == session
                && goal.assignment_root_request_doc_id.as_deref() == Some(opening_doc),
            "Goal {index} lost its exact opening binding"
        );
        let reference = opening
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .expect("signed Goal opening assignment");
        assert!(
            reference.goal_id == goal.goal_id && reference.root_request_doc_id.is_none(),
            "Goal {index} opening borrowed another assignment"
        );
        assert!(
            goal.status == if index == 1 { "complete" } else { "stopped" },
            "Goal {index} lost its terminal state"
        );
        assert!(
            goal.completion_evidence.as_deref()
                == if index == 1 {
                    Some(COMPLETION_EVIDENCE)
                } else {
                    None
                },
            "Goal {index} lost its completion evidence"
        );
    }
    for (row, index) in [(queued, 0), (followup, 1)] {
        let reference = row
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .expect("signed ordinary follow-up assignment");
        assert!(
            reference.goal_id == selected_goals[index].goal_id
                && reference.root_request_doc_id == openings[index].doc_id,
            "ordinary follow-up escaped its captured assignment"
        );
    }
    assert!(
        initial
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .is_none(),
        "initial ordinary chat inherited a later Goal"
    );
    verify_captures(&rows, openings[1], root)?;
    let controls = verify_controls(
        &value,
        &rows,
        &selected_goals,
        node,
        session,
        session_doc,
        author,
        identity,
    )
    .await?;
    let timeline =
        gents::run_timeline_fetch::load_run_timeline_rows(access, &openings[2].request_id).await?;
    for goal in &selected_goals {
        assert!(
            timeline
                .goal_versions
                .iter()
                .any(|version| version.goal_doc_id == goal.doc_id
                    && version.goal_id == goal.goal_id
                    && version.status == goal.status
                    && !version.commit_cid.is_empty()),
            "retained Goal outcome disappeared from canonical history"
        );
    }
    let completed: Vec<_> = timeline
        .tool_calls
        .iter()
        .filter(|call| call.tool_call_id == COMPLETE_CALL)
        .collect();
    assert!(
        completed.len() == 1
            && completed[0].tool_name == gents::goal::UPDATE_GOAL_TOOL_NAME
            && completed[0].request_doc_id == followup.doc_id
            && completed[0].lifecycle_state.as_deref() == Some("completed"),
        "Goal completion bypassed the exact signed follow-up tool owner"
    );
    let result: Value = serde_json::from_str(
        completed[0]
            .result
            .as_deref()
            .expect("Goal completion tool result"),
    )?;
    assert!(
        result["accepted"] == true && result["status"] == "complete",
        "Goal completion was refused: {result}"
    );
    let groups = [
        vec![initial],
        vec![openings[0], queued],
        vec![openings[1], followup],
        vec![openings[2]],
    ];
    let charged = groups
        .iter()
        .map(|group| -> TestResult<u64> {
            timeline
                .inference_calls
                .iter()
                .filter(|call| {
                    group
                        .iter()
                        .any(|request| request.doc_id == call.request_doc_id)
                })
                .try_fold(0u64, |total, call| {
                    assert!(
                        call.call_state == "completed",
                        "fixture inference did not complete"
                    );
                    let prompt = u64::try_from(call.prompt_tokens.expect("fixture prompt usage"))?;
                    let completion =
                        u64::try_from(call.completion_tokens.expect("fixture completion usage"))?;
                    total
                        .checked_add(prompt)
                        .and_then(|total| total.checked_add(completion))
                        .ok_or(TestError::from(
                            "fixture usage exceeded the native token range",
                        ))
                })
        })
        .collect::<TestResult<Vec<_>>>()?;
    for (index, goal) in selected_goals.iter().enumerate() {
        let baseline: u64 = charged[..=index].iter().sum();
        assert!(
            goal.token_usage_baseline == Some(i64::try_from(baseline)?),
            "Goal {index} inherited another assignment's usage baseline"
        );
        assert!(
            goal.tokens_used.unwrap_or_default() >= 0
                && u64::try_from(goal.tokens_used.unwrap_or_default())? <= charged[index + 1],
            "Goal {index} charged another assignment's usage"
        );
    }
    let observation =
        gents::goal::load_session_goal_observation(access, identity, node, session, Some(author))
            .await?
            .expect("verified final Goal observation");
    assert!(
        observation.session_doc_id == session_doc
            && observation.goal.as_ref().is_some_and(|goal| {
                goal.snapshot.goal_id == selected_goals[2].goal_id
                    && goal.snapshot.status == "stopped"
                    && goal.assignment_root_request_doc_id == openings[2].doc_id
            }),
        "signed final read did not observe the third assignment"
    );
    let captured = provider.captured_chat_requests();
    for forbidden in [
        DRAFT,
        OBSERVER_DRAFT,
        OBSERVER_EDIT,
        OBSERVER_CAPTURE,
        &format!("{CAPTURE} changed after capture"),
    ] {
        assert!(
            !captured
                .iter()
                .any(|call| call.to_string().contains(forbidden)),
            "provider received an unsent or later edited snapshot: {forbidden}"
        );
    }
    let second = captured
        .iter()
        .position(|call| current_input_is(call, OBJECTIVES[1]))
        .expect("second Goal provider request");
    assert!(
        !captured[..second]
            .iter()
            .any(|call| call.to_string().contains(CAPTURE))
            && captured[second].to_string().matches(CAPTURE).count() == 1,
        "busy refusal submitted its capture or the opening duplicated it"
    );
    assert!(
        captured
            .iter()
            .filter(|call| current_input_is(call, QUEUED))
            .count()
            == 1
            && captured
                .iter()
                .filter(|call| current_input_is(call, FOLLOWUP))
                .count()
                == 2,
        "queued input or the completed tool loop was replayed"
    );
    let ui: Value = serde_json::from_slice(&std::fs::read(
        root.join("editor-goal-replacement-ui.json"),
    )?)?;
    assert!(ui["sessionId"] == session, "UI replacement changed chats");
    for field in [
        "twoWindowsObserved",
        "staleGoalEditPreserved",
        "observerDraftPreserved",
        "observerCapturePreserved",
        "draftPreserved",
        "busyGoalDraftPreserved",
        "busyCapturePreserved",
        "busyReplacementRefused",
        "reloadPreserved",
    ] {
        assert!(ui[field] == true, "replacement UI evidence omitted {field}");
    }
    Ok(
        json!({"nodeDid":node,"sessionId":session,"sessionDocument":session_doc,
        "goals":selected_goals,"verifiedRequestSignatures":rows.len(),"controls":controls,
        "verifiedGoalObservation":observation,"historyVersions":timeline.goal_versions.len(),
        "chargedByAssignment":charged,"completionTool":completed[0],"ui":ui}),
    )
}

fn verify_captures(
    rows: &[AgentRequestRow],
    second: &AgentRequestRow,
    root: &Path,
) -> TestResult<()> {
    let root_uri = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.starts_with('/') {
            format!("file://{text}")
        } else {
            format!("file:///{text}")
        }
    };
    let expected_workspaces = [root_uri(root), root_uri(&root.canonicalize()?)];
    for row in rows {
        let captures = row
            .input
            .as_ref()
            .map(|input| input.captured_context.as_slice())
            .unwrap_or_default();
        if row.request_id != second.request_id {
            assert!(
                captures.is_empty(),
                "an unrelated signed request captured private editor input"
            );
            continue;
        }
        assert!(
            captures.len() == 1,
            "second Goal opening must retain exactly one frozen capture"
        );
        let capture = &captures[0];
        let workspace = capture
            .workspace_uri
            .as_deref()
            .expect("captured workspace URI")
            .trim_end_matches('/');
        assert!(
            capture.kind == CapturedContextKind::Selection
                && capture.source == CapturedContextSource::Editor
                && capture.content == CAPTURE
                && capture.unsaved
                && capture.version.is_some_and(|value| value > 0)
                && capture
                    .range
                    .is_some_and(|range| (range.start.line, range.start.character)
                        < (range.end.line, range.end.character))
                && expected_workspaces
                    .iter()
                    .any(|expected| expected == workspace)
                && capture.uri.as_deref() == Some(format!("{workspace}/{CAPTURE_FILE}").as_str()),
            "second opening replaced its frozen unsaved selection: {capture:?}"
        );
    }
    assert!(
        std::fs::read_to_string(root.join(CAPTURE_FILE))? == CAPTURE_DISK
            && std::fs::read_to_string(root.join(OBSERVER_CAPTURE_FILE))? == OBSERVER_CAPTURE_DISK,
        "capturing editor context overwrote the workspace files"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_controls(
    value: &Value,
    rows: &[AgentRequestRow],
    goals: &[&gents::goal::GoalDocument],
    node: &str,
    session: &str,
    session_doc: &str,
    author: &str,
    identity: &dyn NodeIdentity,
) -> TestResult<Value> {
    let mut applied_starts = BTreeSet::new();
    let mut stops = BTreeSet::new();
    let mut busy = 0;
    let mut refused_capture = None;
    for row in value["data"]["AgentSessionGoalControl"]
        .as_array()
        .expect("Goal controls")
    {
        let command: SessionGoalControl =
            serde_json::from_str(row["intent_json"].as_str().expect("signed Goal intent"))?;
        let receipt: SessionGoalControlReceipt =
            serde_json::from_str(row["receipt_json"].as_str().expect("signed Goal receipt"))?;
        command.validate_shape()?;
        receipt.validate_for(&command)?;
        assert!(
            command.node_did == node
                && command.session_id == session
                && command.session_doc_id.as_deref() == Some(session_doc)
                && command.requester_did == author,
            "Goal control crossed the original AgentSession"
        );
        assert!(
            identity
                .verify(author, &command.signing_payload()?, &command.signature)
                .await?
                && identity
                    .verify(node, &receipt.signing_payload()?, &receipt.signature)
                    .await?,
            "Goal control or receipt signature was invalid"
        );
        match (&command.operation, &receipt.outcome) {
            (
                GoalOperation::Create { objective, .. },
                GoalControlOutcome::Applied {
                    goal_id,
                    request_id,
                },
            ) => {
                let index = OBJECTIVES
                    .iter()
                    .position(|expected| expected == objective)
                    .expect("unexpected created objective");
                assert!(
                    applied_starts.insert(index) && goal_id == &goals[index].goal_id,
                    "Goal creation was replayed or retargeted"
                );
                let initial = command
                    .initial_request
                    .as_ref()
                    .expect("signed Goal opening");
                let stored = rows
                    .iter()
                    .find(|row| Some(&row.request_id) == request_id.as_ref())
                    .expect("applied Goal request");
                assert!(
                    initial.request_id == stored.request_id
                        && stored.content.as_ref() == Some(&initial.content)
                        && stored.input.as_ref() == Some(&initial.input),
                    "stored Goal opening differs from its immutable command"
                );
            }
            (GoalOperation::Create { objective, .. }, GoalControlOutcome::Rejected { reason }) => {
                assert!(
                    objective == OBJECTIVES[1] && reason == BUSY_REASON,
                    "unexpected replacement refusal: {receipt:?}"
                );
                let initial = command
                    .initial_request
                    .as_ref()
                    .expect("refused Goal request");
                assert!(
                    !rows.iter().any(|row| row.request_id == initial.request_id),
                    "busy replacement leaked its signed request"
                );
                refused_capture = Some(initial.input.captured_context.clone());
                busy += 1;
            }
            (
                GoalOperation::Stop {},
                GoalControlOutcome::Applied {
                    goal_id,
                    request_id,
                },
            ) => {
                assert!(
                    request_id.is_none()
                        && [0, 2]
                            .into_iter()
                            .any(|index| goal_id == &goals[index].goal_id)
                        && stops.insert(goal_id.clone()),
                    "Stop published work or changed a different assignment"
                );
            }
            (GoalOperation::Edit { .. }, GoalControlOutcome::Rejected { .. }) => {}
            _ => {
                return Err(TestError::from(format!(
                    "unexpected Goal control outcome: {receipt:?}"
                )))
            }
        }
    }
    assert!(
        applied_starts.len() == 3 && stops.len() == 2 && busy == 1,
        "expected three starts, two Stops and one busy replacement refusal"
    );
    let second_capture = &rows
        .iter()
        .find(|row| row.content.as_deref() == Some(OBJECTIVES[1]))
        .expect("second opening after refusal")
        .input
        .as_ref()
        .expect("second opening input")
        .captured_context;
    assert!(
        refused_capture.as_ref() == Some(second_capture) && second_capture.len() == 1,
        "busy refusal did not preserve the exact frozen capture through resubmission"
    );
    let claims = value["data"]["GoalCreationClaim"]
        .as_array()
        .expect("Goal creation claims");
    assert!(
        claims.len() == 3
            && goals.iter().all(|goal| claims
                .iter()
                .any(|claim| claim["goal_id"] == goal.goal_id
                    && claim["objective"] == goal.objective)),
        "a refused replacement leaked a creation claim"
    );
    Ok(
        json!({"appliedStarts":applied_starts.len(),"stops":stops.len(),"busyRefusals":busy,"verifiedSignatures":true}),
    )
}

#[test]
fn provider_routes_current_input_without_replaying_historical_goal_openings() {
    let provider = Provider::default();
    let (emitted, _) = tokio::sync::watch::channel(None);
    let request = json!({"messages":[{"role":"user","content":OBJECTIVES[0]},
        {"role":"assistant","content":WORK_FINISHED[0]},
        {"role":"user","content":[{"type":"text","text":OBJECTIVES[1]}]}]});
    match provider.reply(&request, &emitted).unwrap() {
        ChatAction::GatedSse(prefix, _, _, _) => assert!(prefix[0].1.contains(WORK_RUNNING[1])),
        _ => panic!("current Goal opening was not held"),
    }
    let request = json!({"messages":[{"role":"user","content":OBJECTIVES[1]},
        {"role":"user","content":FOLLOWUP}]});
    match provider.reply(&request, &emitted).unwrap() {
        ChatAction::Sse(body) => {
            assert!(body.contains(COMPLETE_CALL) && body.contains("update_goal"))
        }
        _ => panic!("follow-up did not invoke native Goal completion"),
    }
    let request = json!({"messages":[{"role":"user","content":FOLLOWUP},
        {"role":"tool","tool_call_id":COMPLETE_CALL,"content":"completed"}]});
    match provider.reply(&request, &emitted).unwrap() {
        ChatAction::Sse(body) => {
            assert!(body.contains(COMPLETED_REPLY) && !body.contains("update_goal"))
        }
        _ => panic!("completed native tool was replayed"),
    }
}
