use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gents_inference::llm::message::{AssistantContent, Message, ToolCall, ToolFunction};
use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_store::identity::NodeIdentity;
use serde_json::json;

use defra_node::Node as EmbeddedNode;
use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;

mod background_admission;
mod background_budget;

mod delegated_finality_tests;

mod script_composition;
mod wait_settlement;

#[tokio::test]
async fn goal_completion_shares_output_gate_and_preserves_operator_override() {
    use crate::runtime::agent::output_obligation::{ActiveOutputObligation, OutputObligationGate};
    use gents_automation::goal::load_canonical_goal;
    use gents_documents::{WriteToolOutputObligation, WriteToolOutputObligationScope};
    use gents_protocol::goal_status::GoalStatus;

    // The hook's session is signed by the runtime it runs as.
    let db = gents_inference::test_support::test_db(
        "goal-output-gate",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false)
            .with_goal_tools(Arc::new(
                gents_automation::goal::goal_tools::GoalToolAnswers,
            ));
    // The hook acts as the runtime that signs its session and requests.
    hook.actor = Some(identity::Did::new(identity.did()).unwrap());
    assert!(matches!(
        hook.on_completion_call(&user_text_message("publish required review output"), &[])
            .await,
        HookAction::Continue
    ));
    let session = hook.session_id().await.unwrap();
    gents_automation::test_support::goal_hook::set_goal_as_session_owner(
        &node,
        &identity,
        &session,
        Some("publish review output"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    gents_automation::test_support::goal_hook::bind_signed_request(
        &node,
        &hook,
        &identity,
        "goal-output-request",
        &session,
        None,
    )
    .await;
    let request_doc = hook.active_request_doc_id().await.unwrap();
    let gate = OutputObligationGate::new(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        request_doc.clone(),
        vec![ActiveOutputObligation {
            tool_name: "write_scan_result".into(),
            contract: WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: None,
            },
        }],
    );
    let hook = hook.with_output_obligation_gate(Some(Arc::new(gate.clone())));
    let persistence_hook = hook.clone();
    let before = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    accept_hook_tool_call(
        &persistence_hook,
        "premature-goal-complete",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","reason":"text alone is not output"}"#,
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                "premature-goal-complete",
                r#"{"status":"complete","reason":"text alone is not output"}"#,
            )
            .await,
        ToolCallHookAction::Skip { .. }
    ));
    let denied = fetch_tool_call_row(&hook, &session, "premature-goal-complete").await;
    assert_eq!(denied["lifecycle_state"], "completed");
    let result: serde_json::Value =
        serde_json::from_str(denied["result"].as_str().unwrap()).unwrap();
    assert_eq!(result["accepted"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("write_scan_result"));
    let after = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(after.continuation_sequence, before.continuation_sequence);
    assert_eq!(after.completion_evidence, before.completion_evidence);

    accept_hook_tool_call(
        &persistence_hook,
        "required-output",
        "write_scan_result",
        "{}",
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call("write_scan_result", None, "required-output", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    let mut output = persistence_hook
        .in_flight_lifecycles
        .lock()
        .await
        .remove("required-output")
        .expect("accepted write lifecycle");
    assert_eq!(gate.unmet().await.unwrap().len(), 1);
    output
        .complete("persisted scan result acknowledgment")
        .await
        .unwrap();
    assert!(gate.unmet().await.unwrap().is_empty());
    accept_hook_tool_call(
        &persistence_hook,
        "acknowledged-goal-complete",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","reason":"required scan output is durable"}"#,
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                "acknowledged-goal-complete",
                r#"{"status":"complete","reason":"required scan output is durable"}"#,
            )
            .await,
        ToolCallHookAction::Skip { .. }
    ));
    let accepted = fetch_tool_call_row(&hook, &session, "acknowledged-goal-complete").await;
    assert_eq!(accepted["lifecycle_state"], "completed");
    let result: serde_json::Value =
        serde_json::from_str(accepted["result"].as_str().unwrap()).unwrap();
    assert_eq!(result["accepted"], true);
    let complete = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(complete.parsed_status(), Some(GoalStatus::Complete));

    gents_automation::test_support::goal_hook::set_goal_as_session_owner(
        &node,
        &identity,
        "operator-output-session",
        Some("operator decision"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let empty_gate = OutputObligationGate::new(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        "operator-unwritten-request",
        vec![ActiveOutputObligation {
            tool_name: "write_scan_result".into(),
            contract: WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: None,
            },
        }],
    );
    assert_eq!(empty_gate.unmet().await.unwrap().len(), 1);
    gents_automation::test_support::goal_hook::set_goal_as_session_owner(
        &node,
        &identity,
        "operator-output-session",
        None,
        Some(GoalStatus::Complete),
        None,
    )
    .await
    .unwrap();
    let operator = load_canonical_goal(&node, identity.did(), "operator-output-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operator.parsed_status(), Some(GoalStatus::Complete));
    assert_eq!(empty_gate.unmet().await.unwrap().len(), 1);
    drop(persistence_hook);
    drop(hook);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn dispatch_receipt_scripts_bind_call_hook_under_both_persistence_policies() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().canonical_dispatch_observation_cases;
    for case in cases
        .iter()
        .filter(|case| case.inputs.iter().all(|input| input.policy_allows))
    {
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let node_db = gents_inference::test_support::test_db(
                "dispatch-receipt-scripts-bind-call-hook-",
                gents_automation::lifecycle_hooks::composition(),
            )
            .await;
            let node = node_db.node.clone();
            let root =
                gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
            let mut hook = DefraSessionHook::with_identity(node.clone(), root.did(), policy);
            assert!(matches!(
                hook.on_completion_call(&user_text_message("Run a tool"), &[])
                    .await,
                HookAction::Continue
            ));
            let session_id = hook.session_id().await.unwrap();
            bind_managed_hook_request(
                &mut hook,
                "request-receipt",
                &root,
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .await;
            accept_hook_tool_call(&hook, "receipt-call", "read", "{}", None).await;
            let accepted = hook.accepted_tool_calls.lock().await["receipt-call"].clone();
            let admitted = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(case.inputs.len(), case.expected.len());
            for (input, expected) in case.inputs.iter().zip(&case.expected) {
                if expected.observation == "replay" {
                    hook.adopt_accepted_tool_calls(vec![("receipt-call".into(), accepted.clone())])
                        .await
                        .unwrap();
                }
                let call = hook.on_tool_call("read", None, "receipt-call", "{}");
                let action = if input.acknowledged {
                    call.await
                } else {
                    let (action, fired) =
                        gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss(
                            call,
                        )
                        .await;
                    assert!(fired, "{}: receipt fault must fire", case.name);
                    action
                };
                assert_eq!(
                    matches!(action, ToolCallHookAction::Continue),
                    expected.may_invoke,
                    "{}: {action:?}",
                    case.name
                );
                if expected.observation == "replay" {
                    assert!(
                        matches!(&action, ToolCallHookAction::Terminate { reason }
                        if reason.contains("no longer pending")),
                        "{action:?}"
                    );
                }
                let row = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
                assert_eq!(
                    row["lifecycle_state"].as_str() == Some("running"),
                    expected.running,
                    "{}: durable dispatch state",
                    case.name
                );
            }
            let mut fixture = hook_execution_fixtures()
                .lock()
                .await
                .remove(&hook_execution_fixture_key(&hook, "request-receipt"))
                .unwrap();
            let selection = fixture
                .writer
                .terminal_output(&fixture.lifecycle.request().doc_id)
                .await;
            let outcome = match case.parent_outcome.as_str() {
                "failed" => gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                other => panic!("unsupported modeled parent outcome {other}"),
            };
            let completion_outcome = match case.completion_probe_outcome.as_str() {
                "completed" => gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
                other => panic!("unsupported modeled completion probe {other}"),
            };
            assert!(
                !case.completion_probe_accepted,
                "{}: this adapter branch requires a running-call completion rejection",
                case.name
            );
            let document =
                gents_store::graphql::escape_graphql_string(&fixture.lifecycle.request().doc_id);
            let request_query = format!(
                "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{document}\" }} }}) {{ _docID lifecycle_state execution_generation terminal_output }} }}"
            );
            let access = hook.actor_access().unwrap();
            let before_probe = access.execute(&request_query).await.unwrap();
            let before_request = before_probe["data"]["AgentRequest"].as_array().unwrap();
            assert_eq!(before_request.len(), 1);
            let completion = fixture
                .lifecycle
                .terminalize_owned(completion_outcome, selection.clone(), None)
                .await;
            assert_eq!(completion.is_ok(), case.completion_probe_accepted);
            let native_in_flight = completion.as_ref().err().is_some_and(|rejection| {
                matches!(
                    rejection.downcast_ref::<gents_session::request::lifecycle::ToolAccountingRejection>(),
                    Some(gents_session::request::lifecycle::ToolAccountingRejection::ForegroundRunning)
                )
            });
            assert_eq!(
                native_in_flight,
                case.expected.last().unwrap().in_flight,
                "{}: {completion:?}",
                case.name
            );
            let after_probe = access.execute(&request_query).await.unwrap();
            assert_eq!(
                after_probe["data"]["AgentRequest"].as_array().unwrap(),
                before_request,
                "rejected completion must not terminalize or replace the request generation"
            );
            let terminalized = fixture
                .lifecycle
                .terminalize_owned(outcome, selection, Some("dispatch observation failed"))
                .await
                .unwrap();
            assert_eq!(
                terminalized,
                gents_session::request::lifecycle::TerminalizeResult::Won
            );
            let document =
                gents_store::graphql::escape_graphql_string(&fixture.lifecycle.request().doc_id);
            let observed = access.execute(&format!(
                "{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: \"{document}\" }} }}) {{ _docID lifecycle_state stuck_since }} AgentMessage(filter: {{ request_doc_id: {{ _eq: \"{document}\" }} }}) {{ _docID }} }}"
            )).await.unwrap();
            let data = &observed["data"];
            let rows = data["AgentToolCall"].as_array().unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the exact committed call survives parent failure"
            );
            assert_eq!(rows[0]["_docID"], admitted["_docID"]);
            let expected = &case.expected_after_parent_failure;
            assert_eq!(
                rows[0]["lifecycle_state"].as_str() == Some("running"),
                expected.running
            );
            assert_eq!(rows[0]["stuck_since"].is_string(), expected.needs_recovery);
            assert_eq!(
                data["AgentMessage"].as_array().unwrap().len(),
                expected.message_count
            );
            let recovery = case
                .expected_after_parent_recovery
                .as_ref()
                .expect("a handed-off running call has a modeled recovery");
            let report =
                gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                    &node,
                    root.did(),
                    &[Arc::new(fixture.lifecycle.managed_agent_signer().unwrap())],
                )
                .await
                .unwrap();
            assert_eq!(
                report.tool_calls_terminalized, recovery.terminalized,
                "{}",
                case.name
            );
            let recovered = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(recovered["_docID"], admitted["_docID"]);
            assert_eq!(
                recovered["lifecycle_state"].as_str(),
                Some(recovery.state.as_str()),
                "{}: terminal-parent recovery settles the handed-off call",
                case.name
            );
            hook.adopt_accepted_tool_calls(vec![("receipt-call".into(), accepted.clone())])
                .await
                .unwrap();
            let replay = hook.on_tool_call("read", None, "receipt-call", "{}").await;
            assert_eq!(
                matches!(replay, ToolCallHookAction::Continue),
                recovery.dispatchable,
                "{}: a settled call is never re-elected: {replay:?}",
                case.name
            );
            let after_replay = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(
                after_replay["lifecycle_state"],
                recovered["lifecycle_state"]
            );
            node.shutdown().await.expect("shut down test node");
        }
    }
}
