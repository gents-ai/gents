use super::presentation::{diagnostic_replay_matches, terminal_diagnostic_presentation};
use super::*;
use crate::graphql::escape_graphql_string;
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use crate::tools::call::lifecycle::admission_fixture::{
    published_admission_with_owner, published_background_bridge, published_spawn_parent,
    PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission;
use crate::tools::call::lifecycle::ToolCallLifecycle;
use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{
    MessageBlock, MessagePublication, OutputOutcome, OutputSource, SourceClose,
};

#[test]
fn generated_terminal_diagnostic_cases_bind_native_presentation() {
    use crate::lean_vocab_test::LeanTerminalDiagnosticPresentationExpected;

    let cases = crate::lean_vocab_test::lean_terminal_diagnostic_presentation_cases();
    assert_eq!(
        cases.len(),
        10,
        "all generated terminal diagnostic boundaries must be bound"
    );
    for case in cases {
        let raw = String::from_utf8(case.raw.clone());
        let cause = String::from_utf8(case.cause.clone());
        match &case.expected {
            LeanTerminalDiagnosticPresentationExpected::InvalidUtf8 => {
                assert!(
                    raw.is_err() || cause.is_err(),
                    "{}: model rejected valid native UTF-8 input",
                    case.name
                );
            }
            LeanTerminalDiagnosticPresentationExpected::UnexpectedError { error } => {
                panic!(
                    "{}: model produced unexpected presentation error: {error}",
                    case.name
                )
            }
            LeanTerminalDiagnosticPresentationExpected::Ok {
                presentation,
                rendered,
            } => {
                let raw = raw.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                let cause = cause.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                let budget = usize::try_from(case.tail_budget)
                    .unwrap_or_else(|error| panic!("{}: {error}", case.name));
                let actual = terminal_diagnostic_presentation(&raw, &cause, budget)
                    .expect("valid native diagnostic input");
                assert_eq!(
                    actual,
                    crate::lean_vocab_test::native_presentation(presentation)
                        .expect("modeled presentation translates"),
                    "{}: native terminal diagnostic presentation diverged",
                    case.name
                );
                assert_eq!(
                    render_presentation(&raw, &actual)
                        .expect("native presentation renders")
                        .as_bytes(),
                    rendered,
                    "{}: native rendered diagnostic diverged",
                    case.name
                );
            }
        }
    }
}

fn admission(deadline_at: DateTime<Utc>) -> SpawnedBackgroundToolAdmission {
    SpawnedBackgroundToolAdmission {
        tool_name: "background_worker".into(),
        deadline_at,
        selected_tool_identity: None,
    }
}

async fn tool_delivery_rows(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<crate::session::canonical_rows::TranscriptMessageRow> {
    let session = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ {} }} }}"#,
            AGENT_MESSAGE_FIELDS,
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()
        .unwrap()
}

#[tokio::test]
async fn spawn_receipt_survives_modeled_publication_contention() {
    let witness = crate::lean_vocab_test::lean_r6_background_theorem_witnesses()
        .iter()
        .find(|w| w.witness_kind == "receipt_contention")
        .expect("executable receipt contention witness");
    let (node, path, mut tool) = published_spawn_parent("receipt-contention").await;
    let doc_id = tool.doc_id().unwrap().to_owned();
    let before = tool_delivery_rows(&node, &tool.session_id).await.len();
    for _ in 0..witness.numeric_bound {
        fail_next_terminal_for_test(&doc_id);
    }
    assert!(tool.complete_owned("receipt", None).await.unwrap());
    assert!(!terminal_failure_pending_for_test(&doc_id));
    assert_eq!(tool.state, ToolCallState::Completed);
    assert_eq!(
        tool_delivery_rows(&node, &tool.session_id).await.len(),
        before + 1
    );
    assert!(tool.complete_owned("receipt", None).await.is_err());
    assert_eq!(
        tool_delivery_rows(&node, &tool.session_id).await.len(),
        before + 1
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn terminal_point_selection_retains_state_and_tool_identity_fences() {
    for (name, changed_field, changed_value) in [
        ("stale-terminal-point", "lifecycle_state", "cancelled"),
        ("mismatched-terminal-point", "tool_name", "different_tool"),
    ] {
        let (node, path, mut tool) = published_spawn_parent(name).await;
        let doc_id = escape_graphql_string(tool.doc_id().unwrap());
        let before = tool_delivery_rows(&node, &tool.session_id).await.len();
        let escaped_value = escape_graphql_string(changed_value);
        crate::config_client::ConfigAccess::write_local(
            &node,
            "test.change_tool_compare",
            &format!(
                r#"mutation {{ update_AgentToolCall(docID: "{doc_id}", input: {{
                    {changed_field}: "{escaped_value}", started_at: "{}", deadline_at: "{}"
                }}) {{ _docID }} }}"#,
                tool.started_at.unwrap().to_rfc3339(),
                tool.deadline_at.to_rfc3339(),
            ),
        )
        .await
        .unwrap();

        let result = tool
            .fail_owned("late failure", FailureClass::External, None)
            .await;
        assert!(
            !matches!(result, Ok(true)),
            "changed comparison must not publish"
        );
        let row = crate::config_client::ConfigAccess::local(node.clone())
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{doc_id}") {{ lifecycle_state tool_name }} }}"#,
            ))
            .await
            .unwrap();
        assert_eq!(
            row["data"]["AgentToolCall"][0][changed_field],
            changed_value
        );
        assert_ne!(row["data"]["AgentToolCall"][0]["lifecycle_state"], "failed");
        assert_eq!(
            tool_delivery_rows(&node, &tool.session_id).await.len(),
            before
        );
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn failed_streaming_tool_preserves_raw_prefix_and_publishes_terminal_failure() {
    let case = crate::lean_vocab_test::lean_terminal_diagnostic_presentation_cases()
        .iter()
        .find(|case| case.name == "whole_prefix_then_cause")
        .expect("generated terminal diagnostic fixture");
    let raw = String::from_utf8(case.raw.clone()).unwrap();
    let cause = String::from_utf8(case.cause.clone()).unwrap();
    let crate::lean_vocab_test::LeanTerminalDiagnosticPresentationExpected::Ok { rendered, .. } =
        &case.expected
    else {
        panic!("generated valid terminal diagnostic result");
    };
    let (node, path, mut tool) = published_spawn_parent("streamed-failure").await;
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let request_doc_id = tool.request_doc_id.as_deref().unwrap().to_owned();
    let binding = tool
        .tool_output_binding()
        .expect("running tool output binding");
    append_tool_output(&binding, &raw).await.unwrap();

    assert!(tool
        .fail_owned(&cause, FailureClass::External, None)
        .await
        .unwrap());

    let rows = tool_delivery_rows(&node, &tool.session_id).await;
    let delivery = rows
        .iter()
        .find(|row| {
            matches!(
                row.message.publication,
                MessagePublication::ToolDelivery { ref tool_call_doc_id }
                    if tool_call_doc_id == &tool_doc_id
            )
        })
        .expect("failed tool delivery header");
    let MessageBlock::ToolResult { parts, .. } = &delivery.message.blocks[0] else {
        panic!("failed tool delivery is not a ToolResult")
    };
    let gents_protocol::output::ToolResultPart::Text { text } = &parts[0] else {
        panic!("failed tool delivery does not select text")
    };
    assert_eq!(
        render_presentation(&raw, &text.presentation)
            .unwrap()
            .as_bytes(),
        rendered
    );
    let observed = crate::tools::call::lifecycle::load_tool_call_presentation(
        &crate::config_client::ConfigAccess::local(node.clone()),
        &tool_doc_id,
        &tool.node_did,
        &tool.session_id,
        tool.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(observed.result.unwrap().as_bytes(), rendered);

    let segments = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&request_doc_id),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ))
        .await;
    assert!(!segments.has_errors(), "{:#?}", segments.errors);
    let rows = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()
        .unwrap();
    let tool_rows = rows
        .iter()
        .filter(|row| {
            row.segment.source
                == OutputSource::ToolCall {
                    tool_call_doc_id: tool_doc_id.clone(),
                }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tool_rows
            .iter()
            .map(|row| row.segment.payload.as_str())
            .collect::<String>(),
        raw
    );
    assert_eq!(
        tool_rows
            .iter()
            .filter(|row| row.segment.close.is_some())
            .count(),
        1
    );

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn cancelled_and_timed_out_streaming_tools_preserve_raw_prefixes() {
    for (name, timeout) in [("streamed-cancel", false), ("streamed-timeout", true)] {
        let (node, path, mut tool) = published_spawn_parent(name).await;
        let tool_doc_id = tool.doc_id().unwrap().to_owned();
        let request_doc_id = tool.request_doc_id.as_deref().unwrap().to_owned();
        let binding = tool
            .tool_output_binding()
            .expect("running tool output binding");
        append_tool_output(&binding, "stdout-before-terminal\n")
            .await
            .unwrap();
        let mut replay = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &tool_doc_id,
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .expect("running physical tool for same-state replay");
        let mut conflicting_cancel = if timeout {
            None
        } else {
            Some(
                ToolCallLifecycle::load_by_doc_id(
                    node.clone(),
                    &tool_doc_id,
                    &tool.node_did,
                    &tool.session_id,
                    tool.requester_did.as_deref(),
                )
                .await
                .unwrap()
                .expect("running physical tool for conflicting cause"),
            )
        };

        if timeout {
            assert!(tool.timeout().await.unwrap());
        } else {
            assert!(tool
                .cancel_during_run(CancelCause::Interrupted)
                .await
                .unwrap());
        }

        let segments = node
            .execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(&request_doc_id),
                AGENT_OUTPUT_SEGMENT_FIELDS,
            ))
            .await;
        assert!(!segments.has_errors(), "{:#?}", segments.errors);
        let rows = segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let tool_rows = rows
            .iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                    }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_rows
                .iter()
                .map(|row| row.segment.payload.as_str())
                .collect::<String>(),
            "stdout-before-terminal\n"
        );
        assert_eq!(
            tool_rows
                .iter()
                .filter(|row| row.segment.close.is_some())
                .count(),
            1
        );
        let stored = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                escape_graphql_string(&tool_doc_id),
            ))
            .await;
        assert_eq!(
            stored.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
            if timeout { "timedOut" } else { "cancelled" }
        );
        let raw = crate::tools::background::status::canonical_tool_output(
            node.as_ref(),
            &tool_doc_id,
            &request_doc_id,
            &tool.session_id,
            &tool.node_did,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(raw, "stdout-before-terminal\n");
        let presentation = crate::tools::call::lifecycle::load_tool_call_presentation(
            &crate::config_client::ConfigAccess::local(node.clone()),
            &tool_doc_id,
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap();
        let presented = presentation.result.expect("terminal delivery presentation");
        assert!(
            presented.starts_with("stdout-before-terminal\n\n"),
            "terminal diagnostic must expose committed output before the cause: {presented:?}"
        );
        if timeout {
            assert!(presented.contains("tool call deadline exceeded at "));
        } else {
            assert!(presented.ends_with("tool call cancelled"));
        }

        let mut before_messages = tool_delivery_rows(&node, &tool.session_id)
            .await
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>();
        before_messages.sort();
        let mut before_segments = rows
            .iter()
            .map(|row| row.doc_id.clone())
            .collect::<Vec<_>>();
        before_segments.sort();
        if timeout {
            assert!(!replay.timeout().await.unwrap());
        } else {
            assert!(!replay
                .cancel_during_run(CancelCause::Interrupted)
                .await
                .unwrap());
            let mut conflicting_cancel = conflicting_cancel.take().expect("cancel cause contender");
            assert!(!conflicting_cancel
                .cancel_during_run(CancelCause::UserCancelled)
                .await
                .unwrap());
            assert_eq!(conflicting_cancel.state, ToolCallState::Cancelled);
            assert_eq!(
                conflicting_cancel.cancel_cause,
                Some(CancelCause::Interrupted)
            );
        }
        let mut after_messages = tool_delivery_rows(&node, &tool.session_id)
            .await
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>();
        after_messages.sort();
        assert_eq!(
            after_messages, before_messages,
            "terminal replay added a header"
        );
        let after_segments = node
            .execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(&request_doc_id),
                AGENT_OUTPUT_SEGMENT_FIELDS,
            ))
            .await;
        assert!(!after_segments.has_errors(), "{:#?}", after_segments.errors);
        let mut after_segments = after_segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>();
        after_segments.sort();
        assert_eq!(
            after_segments, before_segments,
            "terminal replay added raw output"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}

async fn configure_bash_output_budget(
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    budget: i64,
) {
    use crate::config_client::{
        apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let tools = serde_json::json!({"node_did": owner, "tools_id": "general:tools",
        "host": {"bash": {"mode": "ReadOnly", "max_output_chars": budget}}});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::Collection::Tools,
        add: tools.clone(),
        update: tools,
    }])
    .unwrap();
    ConfigAccess::transact_local_write(node, None, "test.output_budget", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

async fn interrupted_presentation(
    node: &std::sync::Arc<EmbeddedNode>,
    tool: &ToolCallLifecycle,
) -> String {
    crate::tools::call::lifecycle::load_tool_call_presentation(
        &crate::config_client::ConfigAccess::local(node.clone()),
        tool.doc_id().unwrap(),
        &tool.node_did,
        &tool.session_id,
        tool.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .result
    .expect("terminal delivery presentation")
}

#[tokio::test]
async fn interrupted_tool_diagnostic_follows_the_configured_output_budget() {
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission, PublishedAdmission, PublishedAdmissionOptions,
    };
    for (name, tool_name, configured, reload, bounded) in [
        ("budget-bash", "bash", true, false, true),
        ("budget-bash-restart", "bash", true, true, true),
        ("budget-remote", "mcp__search__query", true, false, false),
        ("budget-unresolved", "bash", false, false, false),
    ] {
        let PublishedAdmission {
            node,
            path,
            mut tool,
            ..
        } = published_admission(PublishedAdmissionOptions {
            name: name.to_owned(),
            tool_name: Some(tool_name.to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
        let binding = tool.tool_output_binding().expect("running output binding");
        append_tool_output(&binding, "0123456789abcdefghij")
            .await
            .unwrap();
        if configured {
            crate::test_support::install_test_agent(&node, &tool.node_did, "general").await;
            configure_bash_output_budget(&node, &tool.node_did, 5).await;
        }
        if reload {
            tool = ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                tool.doc_id().unwrap(),
                &tool.node_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .expect("reloaded running tool");
        }
        let mut replay = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            tool.doc_id().unwrap(),
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .expect("running tool for a later same-state replay");
        assert!(tool.timeout().await.unwrap(), "{name}");
        let presented = interrupted_presentation(&node, &tool).await;
        if bounded {
            assert!(
                presented.starts_with("fghij\n") && !presented.contains("0123456789abcde"),
                "{name}: {presented:?}"
            );
        } else {
            assert!(
                presented.starts_with("0123456789abcdefghij\n"),
                "{name}: {presented:?}"
            );
        }
        assert!(
            presented.contains("tool call deadline exceeded at "),
            "{presented:?}"
        );

        if bounded && !reload {
            configure_bash_output_budget(&node, &tool.node_did, 12).await;
            assert!(!replay.timeout().await.unwrap(), "{name}: replay");
            assert_eq!(interrupted_presentation(&node, &tool).await, presented);
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn generated_terminal_diagnostic_replay_cases_bind_native_replay() {
    let cases = crate::lean_vocab_test::lean_terminal_diagnostic_replay_cases();
    assert_eq!(cases.len(), 12, "all generated replay shapes must be bound");
    assert!(cases.iter().any(|case| case.accepted) && cases.iter().any(|case| !case.accepted));
    for case in cases {
        let raw = String::from_utf8(case.raw.clone()).expect("valid replay source");
        let cause = String::from_utf8(case.cause.clone()).expect("valid replay cause");
        let stored = crate::lean_vocab_test::native_presentation(&case.stored)
            .expect("modeled presentation translates");
        assert_eq!(
            diagnostic_replay_matches(&raw, &cause, &stored).expect("replay check"),
            case.accepted,
            "{}: native diagnostic replay diverged from the model",
            case.name
        );
        if case.accepted {
            let budget = usize::try_from(case.configured_budget).unwrap();
            let current = terminal_diagnostic_presentation(&raw, &cause, budget).unwrap();
            assert!(
                diagnostic_replay_matches(&raw, &cause, &current).unwrap(),
                "{}: freshly planned diagnostic must also replay",
                case.name
            );
        }
    }
}

#[tokio::test]
async fn completed_streaming_tool_still_rejects_a_non_extension() {
    let (node, path, mut tool) = published_spawn_parent("streamed-complete-conflict").await;
    let binding = tool
        .tool_output_binding()
        .expect("running tool output binding");
    append_tool_output(&binding, "persisted-prefix")
        .await
        .unwrap();

    let error = tool
        .complete_owned("different-success", None)
        .await
        .expect_err("successful completion cannot replace persisted raw bytes");
    assert!(
        error
            .to_string()
            .contains("not an exact extension of persisted raw output"),
        "{error:#}"
    );
    assert_eq!(tool.state, ToolCallState::Running);

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn spawned_background_replays_parent_provenance_and_never_authors_a_child_tool_result() {
    let (published, _request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "replay".to_owned(),
        ..Default::default()
    })
    .await
    .expect("publish canonical native tool admission");
    let PublishedAdmission {
        node,
        path,
        tool: mut parent,
        ..
    } = published;
    let parent_doc_id = parent.doc_id().unwrap().to_owned();
    let deadline = parent.deadline_at;
    let receipt = r#"{"ok":true,"status":"started"}"#;
    let mut child = parent
        .admit_spawned_background(admission(deadline), receipt)
        .await
        .unwrap();
    let child_doc_id = child.doc_id().unwrap().to_owned();
    assert_ne!(child_doc_id, parent_doc_id);
    assert_eq!(
        child.spawned_by_tool_call_doc_id.as_deref(),
        Some(parent_doc_id.as_str())
    );

    child.start_running().await.unwrap();
    let binding = child.tool_output_binding().unwrap();
    append_tool_output(&binding, "child raw output")
        .await
        .unwrap();
    assert!(child
        .complete_owned("child raw output", None)
        .await
        .unwrap());
    assert!(
        parent
            .admit_spawned_background(admission(deadline + chrono::Duration::seconds(1)), receipt,)
            .await
            .is_err(),
        "a replay cannot change the child deadline"
    );
    let mut replay_parent = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &parent_doc_id,
        "did:test:test",
        &parent.session_id,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let replay = replay_parent
        .admit_spawned_background(admission(deadline), receipt)
        .await
        .unwrap();
    assert_eq!(replay.doc_id(), Some(child_doc_id.as_str()));
    assert_eq!(replay.state, ToolCallState::Completed);

    let messages = tool_delivery_rows(&node, &parent.session_id).await;
    let deliveries = messages.iter().filter(|row| matches!(
        row.message.publication,
        MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &parent_doc_id
    )).count();
    assert_eq!(
        deliveries, 1,
        "the accepted meta-call gets one immediate receipt"
    );
    assert!(messages.iter().all(|row| !matches!(
        row.message.publication,
        MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &child_doc_id
    )), "spawned work must not invent a direct ToolResult header");

    let segments = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(child.request_doc_id.as_deref().unwrap()),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ))
        .await;
    assert!(!segments.has_errors(), "{:#?}", segments.errors);
    let child_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.segment.source
                == OutputSource::ToolCall {
                    tool_call_doc_id: child_doc_id.clone(),
                }
        })
        .collect::<Vec<_>>();
    assert!(
        child_segments.iter().any(|row| matches!(
            row.segment.close,
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                ..
            })
        )),
        "child raw source closes durably under its own physical identity"
    );
    assert_eq!(
        crate::tools::background::status::canonical_tool_output(
            node.as_ref(),
            &child_doc_id,
            child.request_doc_id.as_deref().unwrap(),
            &child.session_id,
            &child.node_did,
            child.requester_did.as_deref(),
        )
        .await
        .unwrap(),
        "child raw output",
        "spawned terminal output is read from its physical ToolOutput source, not a fabricated provider reply"
    );

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn spawned_background_admission_rejects_a_stale_parent_generation() {
    let (node, path, mut parent) = published_spawn_parent("stale").await;
    let request_doc_id = parent.request_doc_id.clone().unwrap();
    let deadline = parent.deadline_at;
    let changed = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "stale-generation" }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(&request_doc_id),
    )).await;
    assert!(!changed.has_errors(), "{:#?}", changed.errors);
    assert!(parent
        .admit_spawned_background(admission(deadline), "receipt")
        .await
        .is_err());
    let parent_doc_id = crate::graphql::escape_graphql_string(parent.doc_id().unwrap());
    let children = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ spawned_by_tool_call_doc_id: {{ _eq: "{parent_doc_id}" }} }}) {{ _docID }} }}"#,
    )).await;
    assert!(!children.has_errors(), "{:#?}", children.errors);
    assert!(children.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn background_bridge_receipt_is_authored_once_and_keeps_bridge_running() {
    let (node, path, mut bridge) = published_background_bridge("receipt").await;
    let tool_doc_id = bridge.doc_id().unwrap().to_owned();
    assert!(bridge.publish_background_receipt("started").await.unwrap());
    assert!(!bridge.publish_background_receipt("started").await.unwrap());
    assert_eq!(bridge.state, ToolCallState::Running);
    assert!(bridge
        .publish_background_receipt("different")
        .await
        .is_err());
    let rows = tool_delivery_rows(&node, &bridge.session_id).await;
    let receipts = rows
        .iter()
        .filter(|row| {
            matches!(&row.message.publication,
        MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == &tool_doc_id)
        })
        .count();
    assert_eq!(receipts, 1);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(&tool_doc_id)
        ))
        .await;
    assert_eq!(
        response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "running"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn background_bridge_completion_closes_its_source_and_notifies_without_a_second_native_result(
) {
    let (node, path, mut bridge) = published_background_bridge("receipt-completion").await;
    let tool_doc_id = bridge.doc_id().unwrap().to_owned();
    let final_bytes = "child terminal bytes";

    assert!(bridge.publish_background_receipt("started").await.unwrap());
    bridge = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &tool_doc_id,
        &bridge.node_did,
        &bridge.session_id,
        bridge.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .expect("rehydrated background bridge");
    assert!(bridge
        .settle_session_message(
            &crate::tools::call::lifecycle::CausedRequestTerminal::Completed {
                output: final_bytes.into()
            },
            None,
        )
        .await
        .unwrap());
    assert_eq!(bridge.state, ToolCallState::Completed);

    crate::tools::background::completion::append_background_tool_completion(
        &node,
        &bridge.session_id,
        "request-receipt-completion",
        &tool_doc_id,
        &bridge.tool_name,
        "completed",
        final_bytes,
        None,
        crate::request::lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap();

    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(&tool_doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    assert_eq!(
        response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "completed"
    );

    let segments = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(bridge.request_doc_id.as_deref().unwrap()),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ))
        .await;
    assert!(!segments.has_errors(), "{:#?}", segments.errors);
    let tool_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.segment.source
                == OutputSource::ToolCall {
                    tool_call_doc_id: tool_doc_id.clone(),
                }
        })
        .collect::<Vec<_>>();
    let close = tool_segments
        .iter()
        .find(|row| {
            matches!(
                row.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    ..
                })
            )
        })
        .expect("terminal bridge source closure");

    let rows = tool_delivery_rows(&node, &bridge.session_id).await;
    let native_results = rows
        .iter()
        .flat_map(|row| row.message.blocks.iter())
        .filter(|block| matches!(
            block,
            MessageBlock::ToolResult { tool_call_doc_id, .. } if tool_call_doc_id == &tool_doc_id
        ))
        .count();
    assert_eq!(
        native_results, 1,
        "the running receipt is the bridge's sole native invocation reply"
    );
    let notification = rows
        .iter()
        .find(|row| {
            row.message.message_key
                == format!("background-completion-notification:{tool_doc_id}:tool")
        })
        .expect("canonical terminal background notification");
    assert!(matches!(
        notification.message.publication,
        MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &tool_doc_id
    ));
    let [MessageBlock::Text { text }] = notification.message.blocks.as_slice() else {
        panic!("background notification must be a composed text reference");
    };
    assert_eq!(text.output.close_doc_id, close.doc_id);
    let observed = tool_segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let reconstructed = reconstruct_stream(&observed, &[], &[], &text.output).unwrap();
    assert_eq!(reconstructed.text, final_bytes);
    assert!(render_presentation(&reconstructed.text, &text.presentation)
        .unwrap()
        .contains(final_bytes));

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn concurrent_background_bridge_completion_replays_one_exact_closure() {
    let (node, path, mut bridge) = published_background_bridge("concurrent-replay").await;
    let tool_doc_id = bridge.doc_id().unwrap().to_owned();
    assert!(bridge.publish_background_receipt("started").await.unwrap());
    let load = || {
        ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &tool_doc_id,
            &bridge.node_did,
            &bridge.session_id,
            bridge.requester_did.as_deref(),
        )
    };
    let mut first = load().await.unwrap().expect("first admitted bridge");
    let mut second = load().await.unwrap().expect("second admitted bridge");
    let one_result = crate::tools::call::lifecycle::CausedRequestTerminal::Completed {
        output: "one result".into(),
    };
    let (left, right) = tokio::join!(
        first.settle_session_message(&one_result, None),
        second.settle_session_message(&one_result, None)
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left, right, "exactly one projector must commit");

    let mut replay = load().await.unwrap().expect("terminal admitted bridge");
    assert!(!replay
        .settle_session_message(
            &crate::tools::call::lifecycle::CausedRequestTerminal::Completed {
                output: "one result".into()
            },
            None,
        )
        .await
        .unwrap());
    assert!(replay
        .settle_session_message(
            &crate::tools::call::lifecycle::CausedRequestTerminal::Completed {
                output: "different result".into()
            },
            None,
        )
        .await
        .is_err());

    let request_doc_id = bridge.request_doc_id.as_deref().unwrap();
    let response = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(request_doc_id),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let closures = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.segment.source
                == OutputSource::ToolCall {
                    tool_call_doc_id: tool_doc_id.clone(),
                }
                && row.segment.close.is_some()
        })
        .count();
    assert_eq!(closures, 1);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}
