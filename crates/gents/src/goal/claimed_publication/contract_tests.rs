//! Generated cases exercise the actual native transaction and publication owner.
use super::*;
use crate::identity::AgentIdentity;
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;

use crate::lifecycle::{ClaimOutcome, RequestTerminalOutcome, TerminalizeResult};
use crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request;
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, SpawnedBackgroundToolAdmission};

// Reuse the existing real-DB Goal fixture and independent signed-field checks.
#[path = "../operator_resume/support.rs"]
mod support;
use support::*;

#[derive(Deserialize)]
struct PublicationContracts {
    goal_claimed_publication_cases: Vec<ResumeCase>,
}

async fn run_generated_running_wait() {
    let contracts: PublicationContracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let resumed = contracts
        .goal_claimed_publication_cases
        .iter()
        .find(|case| case.name == "completed_waited_tool_resumes")
        .expect("generated completed wait case")
        .expected
        .clone();
    let case = contracts
        .goal_claimed_publication_cases
        .into_iter()
        .find(|case| case.name == "claimed_wait_running_defers")
        .expect("generated running wait case");
    assert_eq!(case.outcome, "deferred");
    assert_eq!(case.request["binding"]["predecessor_doc"], 100);
    assert_eq!(case.observation["waits"][0]["reply"], "timed_out_running");
    assert_eq!(case.observation["backgrounds"][0]["state"], "running");
    let fixture = Fixture::new_with_open_parent(&case.before).await;
    let mut request = crate::RequestLifecycle::new_with_agent_did(
        fixture.node.clone(),
        "contract-behavior",
        fixture.identity.did(),
        fixture.parent.clone(),
        60,
    );
    assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
    let mut spawn = publish_accepted_on_claimed_request(
        fixture.node.clone(),
        &mut request,
        fixture.identity.did(),
        0,
        "spawn_process",
        "goal-spawn",
        json!({"tool_name":"bash","args":{"command":"sleep 1"}}),
        None,
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .unwrap();
    let handle = format!("spawned:{}", spawn.doc_id().unwrap());
    let mut background = spawn
        .admit_spawned_background(
            SpawnedBackgroundToolAdmission {
                tool_name: "bash".into(),
                deadline_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                selected_tool_identity: None,
            },
            &json!({"ok":true,"tool_call_id":handle,"status":"running"}).to_string(),
        )
        .await
        .unwrap();
    background.start_running().await.unwrap();
    let mut wait = publish_accepted_on_claimed_request(
        fixture.node.clone(),
        &mut request,
        fixture.identity.did(),
        1,
        "wait_process",
        "goal-wait",
        json!({"tool_call_id":handle,"timeout_secs":1}),
        None,
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .unwrap();
    let terminal_header = wait.accepted_header_doc_id().unwrap().to_owned();
    wait.complete(
        &json!({"ok":false,"tool_call_id":handle,"tool_name":"bash",
            "await_mode":"background","status":"running","result":"",
            "error":{"reason":"wait_timeout","failure_class":"external"}})
        .to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        request
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: terminal_header,
                },
                None,
            )
            .await
            .unwrap(),
        TerminalizeResult::Won
    );
    assert_eq!(
        fixture.observe().await,
        case.before,
        "running wait prestate"
    );
    let observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let result = publish_claimed_continuation(
        &fixture.node,
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    )
    .await
    .unwrap();
    assert!(result.is_none(), "generated intentional wait must defer");
    assert_eq!(fixture.observe().await, case.expected);
    // claimed_wait_then_completion_reuses_claim: the deferred claim publishes
    // once its waited target settles, without another Goal claim.
    background.complete("finished").await.unwrap();
    let receipt = publish_claimed_continuation(
        &fixture.node,
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    )
    .await
    .unwrap()
    .expect("settled wait target must release the deferred claim");
    assert!(receipt.created);
    assert_eq!(fixture.observe().await, resumed);
    fixture.node.shutdown().await;
}

async fn run_generated_wait_observations() {
    let contracts: PublicationContracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<_> = contracts
        .goal_claimed_publication_cases
        .into_iter()
        .filter(|case| {
            case.name == "launched_without_wait_publishes"
                || (case.observation["waits"] != json!([])
                    && case.name != "claimed_wait_running_defers")
        })
        .collect();
    assert_eq!(
        cases.len(),
        19,
        "every new generated wait case is owner-bound"
    );
    for case in cases {
        let fixture = Fixture::new_with_open_parent(&case.before).await;
        if case.name == "unrelated_wait_cannot_suppress" {
            let older = fixture.older_open_request("older-wait-parent").await;
            let mut older_request = crate::RequestLifecycle::new_with_agent_did(
                fixture.node.clone(),
                "contract-behavior",
                fixture.identity.did(),
                older,
                60,
            );
            assert_eq!(older_request.claim().await.unwrap(), ClaimOutcome::Claimed);
            let unrelated_handle = "unrelated-background-handle";
            let mut unrelated = publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut older_request,
                fixture.identity.did(),
                0,
                "wait_process",
                "older-goal-wait",
                json!({"tool_call_id":unrelated_handle,"timeout_secs":1}),
                None,
                AwaitMode::Foreground,
                CancelPolicy::Cascade,
                true,
            )
            .await
            .unwrap();
            let older_terminal_header = unrelated.accepted_header_doc_id().unwrap().to_owned();
            unrelated
                .complete(
                    &json!({"ok":false,"tool_call_id":unrelated_handle,
                    "tool_name":"bash","await_mode":"background","status":"running",
                    "result":"","error":{"reason":"wait_timeout","failure_class":"external"}})
                    .to_string(),
                )
                .await
                .unwrap();
            assert_eq!(
                older_request
                    .terminalize_owned(
                        RequestTerminalOutcome::Completed,
                        gents_protocol::output::TerminalOutput::Message {
                            message_doc_id: older_terminal_header,
                        },
                        None,
                    )
                    .await
                    .unwrap(),
                TerminalizeResult::Won
            );
        }
        let mut request = crate::RequestLifecycle::new_with_agent_did(
            fixture.node.clone(),
            "contract-behavior",
            fixture.identity.did(),
            fixture.parent.clone(),
            60,
        );
        assert_eq!(
            request.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{}",
            case.name
        );
        let backgrounds = case.observation["backgrounds"].as_array();
        let wants_target = backgrounds.is_some_and(|rows| !rows.is_empty())
            || matches!(
                case.name.as_str(),
                "unreadable_target_fails_closed" | "lost_waited_tool_recovers"
            );
        let mut turn = 0;
        let mut terminal_header = None;
        let mut handle = "absent-generated-handle".to_owned();
        let mut target_doc = None;
        let mut renamed_wait_doc = None;
        if wants_target {
            let mut spawn = publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut request,
                fixture.identity.did(),
                turn,
                "spawn_process",
                "goal-spawn",
                json!({"tool_name":"bash","args":{"command":"sleep 1"}}),
                None,
                AwaitMode::Foreground,
                CancelPolicy::Cascade,
                true,
            )
            .await
            .unwrap();
            handle = format!("spawned:{}", spawn.doc_id().unwrap());
            terminal_header = spawn.accepted_header_doc_id().map(str::to_owned);
            let mut target = spawn
                .admit_spawned_background(
                    SpawnedBackgroundToolAdmission {
                        tool_name: "bash".into(),
                        deadline_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                        selected_tool_identity: None,
                    },
                    &json!({"ok":true,"tool_call_id":handle,"status":"running"}).to_string(),
                )
                .await
                .unwrap();
            target_doc = Some(target.doc_id().unwrap().to_owned());
            let target_state = if matches!(
                case.name.as_str(),
                "unreadable_target_fails_closed" | "lost_waited_tool_recovers"
            ) {
                "running"
            } else {
                case.observation["backgrounds"][0]["state"]
                    .as_str()
                    .unwrap()
            };
            match target_state {
                "pending" => {}
                "running" => {
                    target.start_running().await.unwrap();
                }
                "terminal" => {
                    target.start_running().await.unwrap();
                    target.complete("finished").await.unwrap();
                }
                state => panic!("unmapped generated background state {state}"),
            }
            turn += 1;
        }
        if case.name == "ambiguous_same_handle_fails_closed" {
            let mut duplicate = publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut request,
                fixture.identity.did(),
                turn,
                "bash",
                &handle,
                json!({"command":"sleep 1"}),
                None,
                AwaitMode::Foreground,
                CancelPolicy::Cascade,
                false,
            )
            .await
            .unwrap();
            duplicate.start_running().await.unwrap();
            duplicate
                .complete("duplicate background handle")
                .await
                .unwrap();
            terminal_header = duplicate.accepted_header_doc_id().map(str::to_owned);
            assert_ne!(duplicate.doc_id(), target_doc.as_deref());
            turn += 1;
        }
        for (index, observed_wait) in case.observation["waits"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            if case.name == "unrelated_wait_cannot_suppress" {
                continue;
            }
            let reply = observed_wait["reply"].as_str().unwrap();
            let malformed_arguments = case.name == "malformed_accepted_wait_error_fails_closed";
            let mut wait = publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut request,
                fixture.identity.did(),
                turn,
                "wait_process",
                &format!("goal-wait-{index}"),
                if malformed_arguments {
                    json!({"tool_call_id":7})
                } else {
                    json!({"tool_call_id":handle,"timeout_secs":1})
                },
                None,
                AwaitMode::Foreground,
                CancelPolicy::Cascade,
                true,
            )
            .await
            .unwrap();
            terminal_header = wait.accepted_header_doc_id().map(str::to_owned);
            match reply {
                "timed_out_running" => {
                    let reply_handle = if observed_wait["reply_handle"] == "wrong" {
                        "wrong"
                    } else {
                        &handle
                    };
                    wait.complete(
                        &json!({"ok":false,"tool_call_id":reply_handle,
                        "tool_name":"bash","await_mode":"background","status":"running",
                        "result":"","error":{"reason":"wait_timeout","failure_class":"external"}})
                        .to_string(),
                    )
                    .await
                    .unwrap();
                }
                "malformed" => {
                    let raw = if case.name == "renamed_wait_row_cannot_hide_accepted_control" {
                        json!({"ok":false,"tool_call_id":handle,"tool_name":"bash",
                            "await_mode":"background","status":"running","result":"",
                            "error":{"reason":"wait_timeout","failure_class":"external"}})
                    } else if malformed_arguments {
                        json!({"ok":true,"failure_class":"argument_invalid",
                            "path":null,"message":"invalid", "retryable":false,
                            "service_id":"process","tool_name":"wait_process"})
                    } else {
                        json!({"ok":true,"tool_call_id":handle,"tool_name":"bash",
                            "await_mode":"background","status":"running","result":"",
                            "error":{"reason":"caller_deadline_exceeded","failure_class":"external"}})
                    };
                    wait.complete(&raw.to_string()).await.unwrap();
                }
                "argument_error" => {
                    let raw = json!({"ok":false,"failure_class":"argument_invalid",
                        "path":"/tool_call_id","message":"not a runnable handle",
                        "retryable":false,"service_id":"process","tool_name":"wait_process"})
                    .to_string();
                    if case.name == "bounded_presentation_preserves_raw_wait_receipt" {
                        use gents_protocol::output::{PayloadPresentation, PresentationPart};
                        wait.complete_raw_with_presentation(
                            &raw,
                            "[bounded presentation]",
                            PayloadPresentation::Composed {
                                parts: vec![PresentationPart::Literal {
                                    text: "[bounded presentation]".into(),
                                }],
                            },
                        )
                        .await
                        .unwrap();
                    } else {
                        wait.complete(&raw).await.unwrap();
                    }
                }
                "other" => {
                    let pending = case.name == "pending_waited_tool_does_not_suppress";
                    wait.complete(
                        &json!({"ok":false,"tool_call_id":handle,
                            "tool_name":"bash","await_mode":"background",
                            "status":if pending { "pending" } else { "running" },
                            "result":"","error":{"reason":if pending { "wait_timeout" }
                                else { "caller_deadline_exceeded" },
                                "failure_class":"external"}})
                        .to_string(),
                    )
                    .await
                    .unwrap();
                }
                "settled_diagnostic" => {
                    wait.timeout().await.unwrap();
                }
                result => panic!("unmapped generated wait result {result}"),
            }
            if case.name == "renamed_wait_row_cannot_hide_accepted_control" {
                renamed_wait_doc = Some(wait.doc_id().unwrap().to_owned());
            }
            turn += 1;
        }
        assert_eq!(
            request
                .terminalize_owned(
                    RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::Message {
                        message_doc_id: terminal_header
                            .expect("accepted parent has assistant header"),
                    },
                    None,
                )
                .await
                .unwrap_or_else(|error| panic!("{} terminalize: {error:#}", case.name)),
            TerminalizeResult::Won,
            "{}",
            case.name
        );
        assert_eq!(
            fixture.observe().await,
            case.before,
            "{} prestate",
            case.name
        );
        if case.name == "unreadable_target_fails_closed" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.node, &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "invalid-state" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "malformed_spawn_origin_fails_closed" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.node, &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ tool_call_key: "wrong-key" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "foreign_session_target_cannot_suppress" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.node, &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ session_id: "foreign-session" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "lost_waited_tool_recovers" {
            execute(&fixture.node, &format!(
                r#"mutation {{ delete_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc.as_deref().unwrap()),
            )).await;
        }
        if let Some(wait_doc) = renamed_wait_doc {
            execute(&fixture.node, &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ tool_name: "bash" }}) {{ _docID }} }}"#,
                escape_graphql_string(&wait_doc),
            )).await;
        }
        let observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        let result = publish_claimed_continuation(
            &fixture.node,
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await;
        match case.outcome.as_str() {
            "created" => {
                let receipt = result.unwrap().expect(&case.name);
                assert!(
                    receipt.created,
                    "{} recovered instead of creating",
                    case.name
                );
            }
            "deferred" => assert!(result.unwrap().is_none(), "{}", case.name),
            "invalid_evidence" => {
                let error = result.expect_err(&case.name);
                assert!(
                    error
                        .downcast_ref::<wait_observation::InvalidWaitObservation>()
                        .is_some(),
                    "{} returned unrelated publication error: {error:#}",
                    case.name,
                );
            }
            outcome => panic!("unmapped generated wait outcome {outcome}"),
        }
        assert_eq!(fixture.observe().await, case.expected, "{}", case.name);
        fixture.node.shutdown().await;
    }
}

#[tokio::test]
async fn generated_goal_claimed_publication_cases_drive_real_transactions() {
    run_historical_publication_cases().await;
    run_generated_running_wait().await;
    run_generated_wait_observations().await;
}

async fn run_historical_publication_cases() {
    let contracts: PublicationContracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for case in contracts
        .goal_claimed_publication_cases
        .into_iter()
        .filter(|case| {
            case.observation["waits"] == json!([]) && case.name != "launched_without_wait_publishes"
        })
    {
        assert!(seen.insert(case.name.clone()), "duplicate generated case");
        let fixture = Fixture::new(&case.before).await;
        if !case.before["children"].as_array().unwrap().is_empty() {
            fixture.seed_child(case.outcome == "conflict").await;
        }
        if case.before["latest_request"] == 40 {
            fixture
                .other_request("claimed-later-request", "2030-01-01T00:00:00Z", "completed")
                .await;
        }
        assert_eq!(
            fixture.observe().await,
            case.before,
            "{} initial",
            case.name
        );
        let mut observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        observed.status = case.request["expected_status"].as_str().unwrap().into();
        observed.continuation_sequence = case.request["expected_sequence"].as_i64();
        observed.last_continued_from_request_id =
            match case.request["expected_last_continued_from"].as_u64() {
                Some(10) => Some(PARENT.into()),
                None => None,
                value => panic!("unmapped observed watermark {value:?}"),
            };
        let wrapup = observed.parsed_status() == Some(GoalStatus::BudgetLimited);
        let txn = ConfigApplyTxn::begin_local(&fixture.node, None)
            .await
            .unwrap();
        let result = stage_claimed_continuation(
            &txn,
            fixture.identity.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            wrapup,
        )
        .await;
        match case.outcome.as_str() {
            "created" | "rolled_back" => {
                let receipt = result.unwrap().expect("current claim must stage a child");
                assert!(receipt.created);
                let staged_goal = load_canonical_goal_in_txn(&txn, fixture.identity.did(), SESSION)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(staged_goal.status, case.before["status"].as_str().unwrap());
                assert_eq!(
                    staged_goal.continuation_sequence(),
                    case.before["sequence"].as_i64().unwrap()
                );
                assert_eq!(
                    staged_goal.last_continued_from_request_id.as_deref(),
                    Some(PARENT)
                );
                let staged = txn.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#,
                    escape_graphql_string(&receipt.request_id),
                )).await.unwrap();
                let rows: Vec<AgentRequestRow> =
                    serde_json::from_value(staged["data"]["AgentRequest"].clone()).unwrap();
                assert_eq!(rows.len(), 1);
                crate::request_admission::verify_runtime_local_control_receipt(
                    &rows[0],
                    fixture.identity.did(),
                    PARENT,
                )
                .unwrap();
                assert_eq!(rows[0].doc_id.as_deref(), Some(receipt.doc_id.as_str()));
                if case.commit {
                    txn.commit().await.unwrap();
                } else {
                    txn.discard().await.unwrap();
                }
            }
            "recovered" => {
                let receipt = result.unwrap().expect("existing child must recover");
                assert!(!receipt.created);
                assert_eq!(
                    receipt.request_id,
                    goal_continuation_identity(&fixture.goal.goal_id, PARENT, 1)
                        .unwrap()
                        .request_id
                );
                txn.discard().await.unwrap();
            }
            "stale" => {
                assert!(
                    result.unwrap().is_none(),
                    "{} must not publish from stale observation",
                    case.name
                );
                txn.discard().await.unwrap();
            }
            "conflict" => {
                let error = format!("{:#}", result.unwrap_err());
                assert!(
                    error.contains("binding")
                        || error.contains("receipt")
                        || error.contains("immutable"),
                    "{}: {error}",
                    case.name
                );
                txn.discard().await.unwrap();
            }
            outcome => panic!("unmapped generated publication outcome {outcome}"),
        }
        assert_eq!(
            fixture.observe().await,
            case.expected,
            "{} durable result",
            case.name
        );
        fixture.node.shutdown().await;
    }
    assert_eq!(
        seen.len(),
        8,
        "all historical generated publications must bind"
    );
}

#[tokio::test]
async fn queued_claimed_publication_observes_pause_before_creating_child() {
    let before = json!({"status":"active","blocked_audits":2,"wrapup_requested":false,
        "wrapup_completed":false,"sequence":1,"last_continued_from":10,"latest_request":10,
        "children":[],"tokens_used":37,"token_budget":1000});
    let fixture = Fixture::new(&before).await;
    let observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let txn = ConfigApplyTxn::begin_local(&fixture.node, None)
        .await
        .unwrap();
    let publication = publish_claimed_continuation(
        &fixture.node,
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    );
    tokio::pin!(publication);
    assert!(
        matches!(
            futures::poll!(publication.as_mut()),
            std::task::Poll::Pending
        ),
        "publisher must wait behind the held native transaction gate"
    );
    txn.execute(&format!(
        r#"mutation {{ update_Goal(docID: "{}", input: {{ status: "paused", active_started_at: null }}) {{ _docID }} }}"#,
        escape_graphql_string(&fixture.goal.doc_id),
    )).await.unwrap();
    txn.commit().await.unwrap();
    assert!(
        publication.await.unwrap().is_none(),
        "paused claim must not publish a child"
    );
    let mut expected = before;
    expected["status"] = json!("paused");
    assert_eq!(fixture.observe().await, expected);
    fixture.node.shutdown().await;
}
