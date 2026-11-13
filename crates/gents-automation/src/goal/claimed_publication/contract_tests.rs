use super::*;
use crate::goal::continuation::goal_continuation_identity;
use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::row::AgentRequestRow;
use gents_store::identity::NodeIdentity;
use serde_json::json;

use gents_protocol::tool_call::AwaitMode;
use gents_session::request::lifecycle::{ClaimOutcome, RequestTerminalOutcome, TerminalizeResult};
use gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
use gents_toolcalls::call::lifecycle::SpawnedBackgroundToolAdmission;

use super::super::operator_resume::support::*;

async fn verified_client_lifecycle(
    fixture: &Fixture,
    request: gents_protocol::row::AgentRequest,
) -> gents_session::request::lifecycle::RequestLifecycle {
    assert_eq!(request.requester_did.as_deref(), Some(fixture.user.did()));
    assert_eq!(request.agent_did, fixture.agent.did());
    let (_authority_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        fixture.node.clone(),
        fixture.identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, fixture.agent.as_ref())
        .await
        .expect("verify signed client request before goal fixture claim");
    assert!(
        admission.is_some(),
        "client request must retain its native claim admission"
    );
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
        fixture.node.clone(),
        request,
        60,
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(admission);
    lifecycle
}

fn claimed_goal_state() -> serde_json::Value {
    json!({"status":"active","blocked_audits":2,"wrapup_requested":false,
        "wrapup_completed":false,"sequence":1,"last_continued_from":10,"latest_request":10,
        "children":[],"tokens_used":37,"token_budget":1000})
}

fn publication_outcome(result: &Result<ClaimedPublication>, commit: bool) -> String {
    match result {
        Ok(ClaimedPublication::Created(_)) if !commit => "rolled_back",
        Ok(ClaimedPublication::Created(_)) => "created",
        Ok(ClaimedPublication::Recovered(_)) => "recovered",
        Ok(ClaimedPublication::Stale) => "stale",
        Ok(ClaimedPublication::Illegal) => "illegal",
        Ok(ClaimedPublication::Deferred) => "deferred",
        Ok(ClaimedPublication::InvalidEvidence(_)) => "invalid_evidence",
        Err(error) if gents_store::config_client::is_transaction_step_unavailable(error) => {
            "unavailable"
        }
        Err(error) => match gents_protocol::error_chain::find_map_source(&**error, |cause| {
            cause.downcast_ref::<crate::goal::continuation::ContinuationRefusal>()
        }) {
            Some(crate::goal::continuation::ContinuationRefusal::Denied(_)) => "denied",
            Some(crate::goal::continuation::ContinuationRefusal::Illegal(_)) => "illegal",
            Some(crate::goal::continuation::ContinuationRefusal::Conflict(_)) => "conflict",
            None => panic!("untyped claimed publication failure: {error:#}"),
        },
    }
    .to_owned()
}

#[tokio::test]
async fn generated_claimed_targets_bind_signed_receipt_and_user_visibility() {
    let before = claimed_goal_state();
    for case in
        gents_lean_vocab::families::goal_claimed_publication_contracts_claimed_target::cases()
    {
        let fixture = Fixture::new(&before).await;
        let name = case.input.name.as_str();
        let duplicate = name == "duplicate-after-revocation";
        let forged = matches!(
            name,
            "wrong-session" | "wrong-agent" | "missing-session" | "missing-agent"
        );
        if duplicate || forged {
            let session = match name {
                "wrong-session" => Some("different-session".to_owned()),
                "missing-session" => None,
                _ => Some(fixture.session_doc_id.clone()),
            };
            let agent = match name {
                "wrong-agent" => Some("different-agent".to_owned()),
                "missing-agent" => None,
                _ => Some(fixture.agent_doc_id.clone()),
            };
            fixture.seed_child_with_targets(false, session, agent).await;
        }
        let request_id = goal_continuation_identity(&fixture.goal.goal_id, PARENT, 1)
            .unwrap()
            .request_id;
        let filter = format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID session_doc_id agent_doc_id}} }}",
            escape_graphql_string(&request_id)
        );
        if duplicate {
            let rows = fixture.agent_access().execute(&filter).await.unwrap();
            let doc = rows["data"]["AgentRequest"][0]["_docID"].as_str().unwrap();
            fixture
                .agent_access()
                .delete_resource_relationship(
                    "AgentRequest",
                    doc,
                    "parent",
                    "agents",
                    &fixture.agent_doc_id,
                )
                .await
                .unwrap();
            fixture
                .agent_access()
                .delete_resource_relationship(
                    "AgentRequest",
                    doc,
                    "session",
                    "sessions",
                    &fixture.session_doc_id,
                )
                .await
                .unwrap();
        }
        let observed = fixture.current_goal().await;
        let txn = ConfigApplyTxn::begin_local_with_signer(
            &fixture.node,
            identity::Did::new(fixture.agent.did()).unwrap(),
            fixture.agent.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        let result = stage_claimed_continuation(
            &txn,
            fixture.agent.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await;
        if forged {
            assert!(
                result.is_err(),
                "{name} must not recover a foreign signed target"
            );
            txn.discard().await.unwrap();
        } else {
            let receipt = result
                .unwrap()
                .receipt()
                .expect("matching claimed publication");
            assert_eq!(receipt.created, !duplicate);
            let row = txn
                .execute(&format!(
                    "{{ AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} }} }}",
                    escape_graphql_string(&receipt.doc_id),
                    SIGNED_REQUEST_FIELDS
                ))
                .await
                .unwrap();
            let row: AgentRequestRow =
                serde_json::from_value(row["data"]["AgentRequest"][0].clone()).unwrap();
            assert_eq!(
                row.session_doc_id.as_deref(),
                Some(fixture.session_doc_id.as_str())
            );
            assert_eq!(
                row.agent_doc_id.as_deref(),
                Some(fixture.agent_doc_id.as_str())
            );
            gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
            let owners = txn
                .document_owners("AgentRequest", std::slice::from_ref(&receipt.doc_id))
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(identity::Did::as_str),
                Some(fixture.agent.did())
            );
            if case.input.commit {
                txn.commit().await.unwrap();
            } else {
                txn.discard().await.unwrap();
            }
        }
        let bound = |rows: serde_json::Value| {
            rows["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| {
                    row["session_doc_id"] == fixture.session_doc_id.as_str()
                        && row["agent_doc_id"] == fixture.agent_doc_id.as_str()
                })
        };
        let owned = fixture.agent_access().execute(&filter).await.unwrap();
        let user = fixture.user_access().execute(&filter).await.unwrap();
        case.check(
            gents_lean_vocab::families::goal_claimed_publication_contracts_claimed_target::TargetOutput {
                committed: bound(owned),
                grants: bound(user),
            },
        );
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[test]
fn generated_claimed_identity_candidates_follow_the_session_agent() {
    gents_lean_vocab::families::goal_claimed_publication_contracts_identity_candidate::replay(
        |case| {
            let resolve = |name: &str| format!("did:key:z-{name}");
            gents_lean_vocab::families::goal_claimed_publication_contracts_identity_candidate::CandidateOutput {
                candidate_allowed: crate::triggers::engine::goal_source::goal_candidate_in_scope(
                    &resolve(&case.actor),
                    &resolve(&case.selected),
                    &resolve(&case.parent),
                ),
            }
        },
    );
}

#[tokio::test]
async fn generated_claimed_identity_cases_drive_real_transactions() {
    let before = claimed_goal_state();
    for case in
        gents_lean_vocab::families::goal_claimed_publication_contracts_identity_staged::cases()
    {
        let fixture = Fixture::new(&before).await;
        let agent = fixture.parent.agent_did.as_str();
        let node = fixture.identity.did();
        let resolve = |name: &str| match name {
            "agent" => agent,
            "node" => node,
            _ => panic!("unknown modeled identity"),
        };
        assert_eq!(resolve(&case.input.selected), agent);
        let signer: &dyn NodeIdentity = if case.input.signer == "agent" {
            fixture.agent.as_ref()
        } else {
            fixture.identity.as_ref()
        };
        let observed = gents_store::config_client::ConfigAccess::transact_local_readonly(
            &fixture.node,
            Some(identity::Did::new(agent).unwrap()),
            "test.claimed_identity_observed_goal",
            |txn| {
                Box::pin(load_canonical_goal_in_txn(
                    txn,
                    fixture.identity.did(),
                    SESSION,
                ))
            },
        )
        .await
        .unwrap()
        .unwrap();
        let actor = identity::Did::new(resolve(&case.input.actor)).unwrap();
        let txn = if case.input.actor == "agent" {
            ConfigApplyTxn::begin_local_with_signer(
                &fixture.node,
                actor,
                fixture.agent.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap()
        } else {
            ConfigApplyTxn::begin_local(&fixture.node, Some(actor))
                .await
                .unwrap()
        };
        assert_eq!(resolve(&case.input.parent), fixture.parent.agent_did);
        let result = stage_claimed_continuation(
            &txn,
            signer,
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await;
        case.check(
            gents_lean_vocab::families::goal_claimed_publication_contracts_identity_staged::StagedOutput {
                allowed: result.is_ok(),
            },
        );
        txn.discard().await.unwrap();
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

async fn run_generated_running_wait() {
    for generated in
        gents_lean_vocab::families::goal_claimed_publication_contracts_running_wait::cases::<
            String,
            String,
            GoalStatus,
            String,
        >()
    {
        let case = &generated.input;
        assert_eq!(case.request.binding.predecessor_doc, 100);
        assert_eq!(case.observation.waits[0].reply, "timed_out_running");
        assert_eq!(
            case.observation.backgrounds.as_ref().unwrap()[0].state,
            "running"
        );
        let before = serde_json::to_value(&case.before).unwrap();
        let fixture = Fixture::new_with_parent_state(&before, false).await;
        let mut request = verified_client_lifecycle(&fixture, fixture.parent.clone()).await;
        assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
        let mut spawn = publish_accepted_on_claimed_request(
            fixture.node.clone(),
            &mut request,
            fixture.identity.did(),
            0,
            "spawn_process",
            "goal-spawn",
            json!({"tool_name":"bash","args":{"command":"sleep 1"}}),
            AwaitMode::Foreground,
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
            AwaitMode::Foreground,
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
        assert_eq!(fixture.observe().await, before, "running wait prestate");
        let observed = fixture.current_goal().await;
        let result = publish_claimed_continuation(
            &fixture.node,
            fixture.agent.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await;
        let deferred = fixture.observe().await;
        generated.check(gents_lean_vocab::families::goal_claimed_publication_contracts_running_wait::PublicationOutput {
        expected: gents_lean_vocab::family::recode(&deferred),
        outcome: publication_outcome(&result, case.commit),
    });
        background.complete("finished").await.unwrap();
        assert!(publish_claimed_continuation_with_identity(
            &fixture.node,
            fixture.agent.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await
        .unwrap()
        .is_none());
        assert_eq!(fixture.observe().await, deferred);
        let report =
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            &fixture.node,
            fixture.identity.did(),
            std::slice::from_ref(&fixture.agent),
        )
        .await
        .unwrap();
        assert_eq!(report.side_effects_converged, 1);
        assert!(publish_claimed_continuation_with_identity(
            &fixture.node,
            fixture.agent.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            false,
        )
        .await
        .unwrap()
        .is_none());
        let after = fixture.current_goal().await;
        assert_eq!(
            (
                after.continuation_sequence(),
                after.last_continued_from_request_id
            ),
            (
                observed.continuation_sequence(),
                observed.last_continued_from_request_id.clone()
            ),
            "the old-parent claim publishes nothing once the wake is the latest request"
        );
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

async fn discharge_completion_obligation(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) {
    let doc = gents_store::graphql::escape_graphql_string(doc_id);
    let before = access.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ _docID lifecycle_state status }} }}"#,
    )).await.unwrap();
    let [row] = before["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("selected Agent must read the exact background tool: {before}");
    };
    assert_eq!(row["_docID"], doc_id);
    assert_eq!(row["lifecycle_state"], "completed");
    assert!(row["status"]
        .as_str()
        .unwrap()
        .starts_with("completionPending"));
    let response = access.write("test.goal.discharge_completion", &format!(
        r#"mutation {{ update_AgentToolCall(docID: "{doc}", input: {{ status: "completed" }}) {{ _docID }} }}"#,
    )).await.unwrap();
    let [updated] = response["data"]["update_AgentToolCall"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("selected Agent must update the exact background tool: {response}");
    };
    assert_eq!(updated["_docID"], doc_id);
}

async fn run_generated_wait_observations() {
    let mut abandoned_follow_ups = 0;
    for generated in
        gents_lean_vocab::families::goal_claimed_publication_contracts_wait_observation::cases::<
            String,
            String,
            GoalStatus,
            String,
        >()
    {
        let case = &generated.input;
        let before = serde_json::to_value(&case.before).unwrap();
        let fixture = Fixture::new_with_earlier_request(
            &before,
            false,
            (case.name == "unrelated_wait_cannot_suppress").then_some("older-wait-parent"),
        )
        .await;
        if case.name == "unrelated_wait_cannot_suppress" {
            let older = gents_protocol::row::AgentRequest::try_from(
                request_rows(&fixture.agent_access())
                    .await
                    .into_iter()
                    .find(|row| row.request_id == "older-wait-parent")
                    .unwrap(),
            )
            .unwrap();
            let mut older_request = verified_client_lifecycle(&fixture, older).await;
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
                AwaitMode::Foreground,
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
        let mut request = verified_client_lifecycle(&fixture, fixture.parent.clone()).await;
        assert_eq!(
            request.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{}",
            case.name
        );
        let backgrounds = case.observation.backgrounds.as_ref();
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
        let mut wait_docs = Vec::new();
        if wants_target {
            let mut spawn = publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut request,
                fixture.identity.did(),
                turn,
                "spawn_process",
                "goal-spawn",
                json!({"tool_name":"bash","args":{"command":"sleep 1"}}),
                AwaitMode::Foreground,
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
                case.observation.backgrounds.as_ref().unwrap()[0]
                    .state
                    .as_str()
            };
            match target_state {
                "pending" => {}
                "running" => {
                    target.start_running().await.unwrap();
                }
                "completion_pending" => {
                    target.start_running().await.unwrap();
                    if case.name.starts_with("failed_") {
                        target
                            .fail(
                                "failed",
                                gents_loop::tool_call_lifecycle::FailureClass::External,
                            )
                            .await
                            .unwrap();
                    } else if case.name.starts_with("cancelled_") {
                        target
                            .cancel_during_run(
                                gents_session::session::tool_call_state::CancelCause::UserCancelled,
                            )
                            .await
                            .unwrap();
                    } else {
                        target.complete("finished").await.unwrap();
                    }
                }
                "terminal" => {
                    target.start_running().await.unwrap();
                    target.complete("finished").await.unwrap();
                    discharge_completion_obligation(
                        &fixture.agent_access(),
                        target.doc_id().unwrap(),
                    )
                    .await;
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
                AwaitMode::Foreground,
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
        for (index, observed_wait) in case.observation.waits.iter().enumerate() {
            if case.name == "unrelated_wait_cannot_suppress" {
                continue;
            }
            let reply = observed_wait.reply.as_str();
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
                AwaitMode::Foreground,
                observed_wait.replied,
            )
            .await
            .unwrap();
            terminal_header = wait.accepted_header_doc_id().map(str::to_owned);
            wait_docs.push(wait.doc_id().unwrap().to_owned());
            if !observed_wait.replied {
                turn += 1;
                continue;
            }
            match reply {
                "timed_out_running" => {
                    let reply_handle = if observed_wait.reply_handle == "wrong" {
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
        let unstarted_wait = case.observation.waits.iter().any(|wait| !wait.replied);
        let (outcome, failure) = if unstarted_wait {
            (
                RequestTerminalOutcome::Failed,
                Some("provider failed before the accepted wait started"),
            )
        } else {
            (RequestTerminalOutcome::Completed, None)
        };
        assert_eq!(
            request
                .terminalize_owned(
                    outcome,
                    gents_protocol::output::TerminalOutput::Message {
                        message_doc_id: terminal_header
                            .expect("accepted parent has assistant header"),
                    },
                    failure,
                )
                .await
                .unwrap_or_else(|error| panic!("{} terminalize: {error:#}", case.name)),
            TerminalizeResult::Won,
            "{}",
            case.name
        );
        assert_eq!(fixture.observe().await, before, "{} prestate", case.name);
        if case.name == "unreadable_target_fails_closed" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.agent_access(), &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "invalid-state" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "malformed_spawn_origin_fails_closed" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.agent_access(), &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ tool_call_key: "wrong-key" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "foreign_session_target_cannot_suppress" {
            let target_doc = target_doc.as_deref().unwrap();
            execute(&fixture.agent_access(), &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ session_id: "foreign-session" }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc),
            )).await;
        }
        if case.name == "lost_waited_tool_recovers" {
            execute(&fixture.agent_access(), &format!(
                r#"mutation {{ delete_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(target_doc.as_deref().unwrap()),
            )).await;
        }
        if let Some(wait_doc) = renamed_wait_doc {
            execute(&fixture.agent_access(), &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ tool_name: "bash" }}) {{ _docID }} }}"#,
                escape_graphql_string(&wait_doc),
            )).await;
        }
        let observed = fixture.current_goal().await;
        let wrapup = observed.parsed_status() == Some(GoalStatus::BudgetLimited);
        let publication = publish_claimed_continuation(
            &fixture.node,
            fixture.agent.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            wrapup,
        );
        let result = if case.observation.storage_failed {
            ConfigApplyTxn::with_read_storage_failure(
                "await_mode spawned_by_tool_call_doc_id",
                publication,
            )
            .await
        } else {
            publication.await
        };
        let outcome = publication_outcome(&result, case.commit);
        let published = fixture.observe().await;
        match result {
            Err(error) if gents_store::config_client::is_transaction_step_unavailable(&error) => {
                let retried = publish_claimed_continuation_with_identity(
                    &fixture.node,
                    fixture.agent.as_ref(),
                    &observed,
                    PARENT,
                    "Original signed continuation",
                    wrapup,
                )
                .await
                .unwrap();
                assert!(
                    retried.is_none(),
                    "{} retry still observes the wait",
                    case.name
                );
            }
            Ok(ClaimedPublication::InvalidEvidence(_)) => {
                let stopped = fixture.current_goal().await;
                assert!(
                    stopped
                        .last_failure
                        .as_deref()
                        .is_some_and(|reason| reason.starts_with("invalid Goal wait evidence: ")),
                    "{} must record why automation stopped: {:?}",
                    case.name,
                    stopped.last_failure,
                );
                if stopped.parsed_status() == Some(GoalStatus::BudgetLimited) {
                    let rescan = |wrapup| {
                        publish_claimed_continuation_with_identity(
                            &fixture.node,
                            fixture.agent.as_ref(),
                            &observed,
                            PARENT,
                            "Original signed continuation",
                            wrapup,
                        )
                    };
                    assert!(rescan(wrapup).await.unwrap().is_none(), "{}", case.name);
                    assert_eq!(fixture.observe().await, published, "{}", case.name);
                    let rescanned = fixture.current_goal().await;
                    assert_eq!(
                        (&rescanned.updated_at, &rescanned.last_failure),
                        (&stopped.updated_at, &stopped.last_failure),
                        "{}: a rescan must not write the Goal again",
                        case.name
                    );
                    for wait_doc in &wait_docs {
                        execute(&fixture.agent_access(), &format!(
                            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "cancelled" }}) {{ _docID }} }}"#,
                            escape_graphql_string(wait_doc),
                        )).await;
                    }
                    assert!(rescan(wrapup).await.unwrap().is_none(), "{}", case.name);
                    assert_eq!(fixture.observe().await, published, "{}", case.name);
                    abandoned_follow_ups += 1;
                }
            }
            _ => {}
        }
        generated.check(gents_lean_vocab::families::goal_claimed_publication_contracts_wait_observation::PublicationOutput {
            expected: gents_lean_vocab::family::recode(&published),
            outcome,
        });
        fixture.node.shutdown().await.expect("node shutdown");
    }
    assert_eq!(
        abandoned_follow_ups, 1,
        "the budget wrap-up case drives its follow-up scans"
    );
}

#[tokio::test]
async fn generated_goal_claimed_publication_cases_drive_real_transactions() {
    run_historical_publication_cases().await;
    run_generated_running_wait().await;
    run_generated_wait_observations().await;
}

async fn run_historical_publication_cases() {
    for generated in
        gents_lean_vocab::families::goal_claimed_publication_contracts_historical::cases::<
            String,
            String,
            GoalStatus,
            String,
        >()
    {
        let case = &generated.input;
        let before = serde_json::to_value(&case.before).unwrap();
        let fixture = Fixture::new(&before).await;
        if let Some(child) = case.before.children.first() {
            fixture
                .seed_child(child.semantic_fingerprint != case.request.binding.semantic_fingerprint)
                .await;
        }
        if case.before.latest_request == 40 {
            fixture
                .other_request("claimed-later-request", "2030-01-01T00:00:00Z", "completed")
                .await;
        }
        assert_eq!(fixture.observe().await, before, "{} initial", case.name);
        let mut observed = fixture.current_goal().await;
        observed.status = case.request.expected_status.as_str().to_owned();
        observed.continuation_sequence = i64::try_from(case.request.expected_sequence).ok();
        observed.last_continued_from_request_id = match case.request.expected_last_continued_from {
            Some(10) => Some(PARENT.into()),
            value => panic!("unmapped observed watermark {value:?}"),
        };
        let wrapup = observed.parsed_status() == Some(GoalStatus::BudgetLimited);
        let txn = ConfigApplyTxn::begin_local_with_signer(
            &fixture.node,
            identity::Did::new(&fixture.parent.agent_did).unwrap(),
            fixture.agent.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        let agent_identity = fixture.agent.as_ref();
        let result = stage_claimed_continuation(
            &txn,
            agent_identity,
            &observed,
            PARENT,
            "Original signed continuation",
            wrapup,
        )
        .await;
        let outcome = publication_outcome(&result, case.commit);
        match result {
            Ok(ClaimedPublication::Created(receipt)) => {
                assert!(receipt.created);
                let staged_goal = load_canonical_goal_in_txn(&txn, fixture.identity.did(), SESSION)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(staged_goal.status, case.before.status.as_str());
                assert_eq!(
                    staged_goal.continuation_sequence(),
                    i64::try_from(case.before.sequence).unwrap()
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
                let requester = fixture
                    .parent
                    .requester_did
                    .as_deref()
                    .expect("parent requester");
                assert_eq!(requester, fixture.user.did());
                gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
                    &rows[0],
                    fixture.identity.did(),
                    PARENT,
                    requester,
                )
                .unwrap();
                assert_eq!(rows[0].agent_did.as_deref(), Some(fixture.agent.did()));
                assert_eq!(
                    rows[0].caused_by_parent_request_doc_id.as_deref(),
                    Some(fixture.parent.doc_id.as_str()),
                );
                assert_eq!(rows[0].doc_id.as_deref(), Some(receipt.doc_id.as_str()));
                if case.commit {
                    txn.commit().await.unwrap();
                } else {
                    txn.discard().await.unwrap();
                }
            }
            Ok(ClaimedPublication::Recovered(receipt)) => {
                assert!(!receipt.created);
                assert_eq!(
                    receipt.request_id,
                    goal_continuation_identity(&fixture.goal.goal_id, PARENT, 1)
                        .unwrap()
                        .request_id
                );
                txn.discard().await.unwrap();
            }
            _ => txn.discard().await.unwrap(),
        }
        generated.check(gents_lean_vocab::families::goal_claimed_publication_contracts_historical::PublicationOutput {
            expected: gents_lean_vocab::family::recode(&fixture.observe().await),
            outcome,
        });
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn queued_claimed_publication_observes_pause_before_creating_child() {
    let before = json!({"status":"active","blocked_audits":2,"wrapup_requested":false,
        "wrapup_completed":false,"sequence":1,"last_continued_from":10,"latest_request":10,
        "children":[],"tokens_used":37,"token_budget":1000});
    let fixture = Fixture::new(&before).await;
    let observed = fixture.current_goal().await;
    let txn = ConfigApplyTxn::begin_local_with_signer(
        &fixture.node,
        identity::Did::new(fixture.agent.did()).unwrap(),
        fixture.agent.managed_agent_signer().unwrap(),
    )
    .await
    .unwrap();
    let publication = publish_claimed_continuation_with_identity(
        &fixture.node,
        fixture.agent.as_ref(),
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
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn claimed_publication_replay_keeps_its_hop_across_a_same_second_write() {
    let before = json!({"status":"active","blocked_audits":2,"wrapup_requested":false,
        "wrapup_completed":false,"sequence":1,"last_continued_from":10,"latest_request":10,
        "children":[],"tokens_used":37,"token_budget":1000});
    let fixture = Fixture::new(&before).await;
    let observed = fixture.current_goal().await;
    let first = publish_claimed_continuation_with_identity(
        &fixture.node,
        fixture.agent.as_ref(),
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    )
    .await
    .unwrap()
    .expect("first publication creates the continuation");
    assert!(first.created);
    let child = request_rows(&fixture.agent_access())
        .await
        .into_iter()
        .find(|row| row.request_id == first.request_id)
        .unwrap();
    execute(
        &fixture.agent_access(),
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{
            request_id: "background-completion-same-second", purpose: "normal",
            node_did: "{}", requester_did: "{}", agent_did: "{}",
            session_id: "{SESSION}", content: "later work", execution_origin: "scheduled",
            lifecycle_state: "completed", failure_reason: "", created_at: "{}",
            retry_count: 0, max_retries: 3, retry_root_request: "background-completion-same-second",
            request_hop: {}
        }}) {{ _docID }} }}"#,
            escape_graphql_string(fixture.identity.did()),
            escape_graphql_string(fixture.user.did()),
            escape_graphql_string(fixture.agent.did()),
            escape_graphql_string(child.created_at.as_deref().unwrap()),
            child.request_hop.unwrap_or(0) + 5,
        ),
    )
    .await;
    let replay = publish_claimed_continuation_with_identity(
        &fixture.node,
        fixture.agent.as_ref(),
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    )
    .await
    .unwrap()
    .expect("replay recovers the published continuation");
    assert!(!replay.created);
    assert_eq!(replay.request_id, first.request_id);
    fixture.node.shutdown().await.expect("node shutdown");
}
