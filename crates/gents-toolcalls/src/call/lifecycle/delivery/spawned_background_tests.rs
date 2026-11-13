use super::presentation::{diagnostic_replay_matches, terminal_diagnostic_presentation};
use super::*;
use crate::call::lifecycle::admission_fixture::{
    published_admission_with_owner, published_background_bridge, published_spawn_parent,
    PublishedAdmission, PublishedAdmissionOptions,
};
use crate::call::lifecycle::SpawnedBackgroundToolAdmission;
use crate::call::lifecycle::ToolCallLifecycle;
use anyhow::Result;
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, OutputOutcome, OutputSource, SourceClose,
};
use gents_session::session::tool_presentation::render_presentation;
use gents_store::graphql::escape_graphql_string;

fn modeled_bytes(bytes: &[u64]) -> Vec<u8> {
    bytes
        .iter()
        .map(|byte| u8::try_from(*byte).expect("modeled byte"))
        .collect()
}

fn wire_bytes(bytes: &[u8]) -> Vec<u64> {
    bytes.iter().map(|byte| u64::from(*byte)).collect()
}

#[test]
fn generated_terminal_diagnostic_cases_bind_native_presentation() {
    use gents_lean_vocab::families::terminal_diagnostic_contracts_presentation_plan::{
        PartWire, PlanOutput, PresentationWire,
    };
    use gents_protocol::output::{PayloadPresentation, PresentationPart};
    let wire = |presentation: &PayloadPresentation| match presentation {
        PayloadPresentation::Full => PresentationWire {
            kind: "full".into(),
            parts: Vec::new(),
        },
        PayloadPresentation::Composed { parts } => PresentationWire {
            kind: "composed".into(),
            parts: parts
                .iter()
                .map(|part| match part {
                    PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => PartWire {
                        kind: "range".into(),
                        start: Some(*start_byte),
                        stop: Some(*end_byte),
                        bytes: None,
                    },
                    PresentationPart::Literal { text } => PartWire {
                        kind: "literal".into(),
                        start: None,
                        stop: None,
                        bytes: Some(wire_bytes(text.as_bytes())),
                    },
                })
                .collect(),
        },
    };
    gents_lean_vocab::families::terminal_diagnostic_contracts_presentation_plan::replay(|i| {
        let (Ok(raw), Ok(cause)) = (
            String::from_utf8(modeled_bytes(&i.raw)),
            String::from_utf8(modeled_bytes(&i.cause)),
        ) else {
            return PlanOutput {
                kind: "invalid_utf8".into(),
                presentation: None,
                rendered: None,
            };
        };
        let budget = usize::try_from(i.tail_budget).expect("modeled tail budget");
        match terminal_diagnostic_presentation(&raw, &cause, budget) {
            Ok(actual) => PlanOutput {
                kind: "ok".into(),
                rendered: Some(wire_bytes(
                    render_presentation(&raw, &actual)
                        .expect("native presentation renders")
                        .as_bytes(),
                )),
                presentation: Some(wire(&actual)),
            },
            Err(_) => PlanOutput {
                kind: "unexpected_error".into(),
                presentation: None,
                rendered: None,
            },
        }
    });
}

fn admission(deadline_at: DateTime<Utc>) -> SpawnedBackgroundToolAdmission {
    SpawnedBackgroundToolAdmission {
        tool_name: "background_worker".into(),
        deadline_at,
        selected_tool_identity: None,
    }
}

/// The lifecycle's Agent owns its session's rows; ACP hides them from any
/// other reader, so fixtures observe them through that Agent's access.
struct Observed {
    data: Option<serde_json::Value>,
    errors: Option<serde_json::Value>,
}

impl Observed {
    fn has_errors(&self) -> bool {
        self.errors.is_some()
    }
}

async fn observe(tool: &ToolCallLifecycle, query: &str) -> Observed {
    match tool.access().unwrap().execute(query).await {
        Ok(response) => Observed {
            data: response.get("data").cloned(),
            errors: None,
        },
        Err(error) => Observed {
            data: None,
            errors: Some(serde_json::Value::String(format!("{error:#}"))),
        },
    }
}

async fn tool_delivery_rows(
    tool: &ToolCallLifecycle,
) -> Vec<gents_protocol::output::rows::TranscriptMessageRow> {
    let session = gents_store::graphql::escape_graphql_string(&tool.session_id);
    let response = observe(
        tool,
        &format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ {} }} }}"#,
            AGENT_MESSAGE_FIELDS,
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn spawn_receipt_survives_modeled_publication_contention() {
    for case in gents_lean_vocab::families::contracts_background_receipt_contention::cases() {
        let (node, path, mut tool) = published_spawn_parent("receipt-contention").await;
        let doc_id = tool.doc_id().unwrap().to_owned();
        let before = tool_delivery_rows(&tool).await.len();
        for _ in 0..case.input.numeric_bound {
            fail_next_terminal_for_test(&doc_id);
        }
        let committed = tool.complete_owned("receipt", None).await.unwrap();
        assert!(!terminal_failure_pending_for_test(&doc_id));
        if committed {
            assert_eq!(tool.state, ToolCallState::Completed);
            assert_eq!(tool_delivery_rows(&tool).await.len(), before + 1);
            assert!(tool.complete_owned("receipt", None).await.is_err());
            assert_eq!(tool_delivery_rows(&tool).await.len(), before + 1);
        }
        case.check(committed);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn terminal_point_selection_retains_state_and_tool_identity_fences() {
    for (name, changed_field, changed_value) in [
        ("stale-terminal-point", "lifecycle_state", "cancelled"),
        ("mismatched-terminal-point", "tool_name", "different_tool"),
    ] {
        let (node, path, mut tool) = published_spawn_parent(name).await;
        let doc_id = escape_graphql_string(tool.doc_id().unwrap());
        let before = tool_delivery_rows(&tool).await.len();
        let escaped_value = escape_graphql_string(changed_value);
        tool.access()
            .unwrap()
            .write(
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
        let row = tool
            .access()
            .unwrap()
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
        assert_eq!(tool_delivery_rows(&tool).await.len(), before);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn failed_streaming_tool_preserves_raw_prefix_and_publishes_terminal_failure() {
    for case in gents_lean_vocab::families::terminal_diagnostic_contracts_streamed_failure::cases()
    {
        let raw = String::from_utf8(modeled_bytes(&case.input.raw)).unwrap();
        let cause = String::from_utf8(modeled_bytes(&case.input.cause)).unwrap();
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

        let rows = tool_delivery_rows(&tool).await;
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
        let delivered = render_presentation(&raw, &text.presentation).unwrap();
        let observed = gents_session::tool_call::query::result::load_tool_call_presentation(
            &tool.access().unwrap(),
            &tool_doc_id,
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(observed.result.as_deref(), Some(delivered.as_str()));
        case.check(
        gents_lean_vocab::families::terminal_diagnostic_contracts_streamed_failure::StreamedFailureOutput {
            rendered: Some(wire_bytes(delivered.as_bytes())),
        },
    );

        let segments = observe(
        &tool,
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&request_doc_id),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ),
    )
    .await;
        assert!(!segments.has_errors(), "{:#?}", segments.errors);
        let rows = segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>, _>>()
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

        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
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
        let mut replay = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            tool.actor().clone(),
            &tool_doc_id,
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .expect("running physical tool for same-state replay");
        replay.inherit_signer(&tool).unwrap();
        let mut conflicting_cancel = if timeout {
            None
        } else {
            Some(
                ToolCallLifecycle::load_by_doc_id_as(
                    node.clone(),
                    tool.actor().clone(),
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
        if let Some(conflicting) = conflicting_cancel.as_mut() {
            conflicting.inherit_signer(&tool).unwrap();
        }

        if timeout {
            assert!(tool.timeout().await.unwrap());
        } else {
            assert!(tool
                .cancel_during_run(CancelCause::Interrupted)
                .await
                .unwrap());
        }

        let segments = observe(&tool, &format!(
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
            .collect::<Result<Vec<_>, _>>()
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
        let stored = observe(&tool, &format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                escape_graphql_string(&tool_doc_id),
            ))
            .await;
        assert_eq!(
            stored.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
            if timeout { "timedOut" } else { "cancelled" }
        );
        let raw =
            gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &tool.access().unwrap(),
                &tool_doc_id,
                &request_doc_id,
                &tool.session_id,
                &tool.node_did,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .into_text();
        assert_eq!(raw, "stdout-before-terminal\n");
        let presentation = gents_session::tool_call::query::result::load_tool_call_presentation(
            &tool.access().unwrap(),
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

        let mut before_messages = tool_delivery_rows(&tool)
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
        let mut after_messages = tool_delivery_rows(&tool)
            .await
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>();
        after_messages.sort();
        assert_eq!(
            after_messages, before_messages,
            "terminal replay added a header"
        );
        let after_segments = observe(&tool, &format!(
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
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>();
        after_segments.sort();
        assert_eq!(
            after_segments, before_segments,
            "terminal replay added raw output"
        );

        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}

async fn configure_bash_output_budget(
    author: &gents_store::config_client::ConfigAccess,
    owner: &str,
    agent_did: &str,
    budget: i64,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    // The request's Agent resolves its tools by the `{agent_did}:tools` id the
    // fixture wrote; a differently named document is never read.
    let tools_id = format!("{agent_did}:tools");
    let tools = serde_json::json!({"node_did": owner, "tools_id": tools_id,
        "host": {"bash": {"mode": "ReadOnly", "max_output_chars": budget}}});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Tools,
        add: tools.clone(),
        update: tools,
    }])
    .unwrap();
    // Tools are configuration: the owning User who wrote them signs the edit.
    author
        .transact_write("test.output_budget", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

async fn interrupted_presentation(tool: &ToolCallLifecycle) -> String {
    gents_session::tool_call::query::result::load_tool_call_presentation(
        &tool.access().unwrap(),
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
async fn interrupted_tool_diagnostics_follow_the_lean_output_budget() {
    use crate::call::lifecycle::admission_fixture::PublishedAdmission;
    use gents_lean_vocab::families::interrupted_diagnostic_budget::Diagnostic;

    for (index, case) in gents_lean_vocab::families::interrupted_diagnostic_budget::cases()
        .into_iter()
        .enumerate()
    {
        let input = &case.input;
        let (
            PublishedAdmission {
                node,
                path,
                mut tool,
                client,
                ..
            },
            request,
        ) = published_admission_with_owner(PublishedAdmissionOptions {
            name: format!("interrupted-budget-{index}"),
            tool_name: Some(input.tool_name.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        let agent_did = request.request().agent_did.clone();
        let binding = tool.tool_output_binding().expect("running output binding");
        append_tool_output(&binding, &input.raw).await.unwrap();
        if let Some(budget) = input.configured_budget {
            configure_bash_output_budget(
                &client,
                &tool.node_did,
                &agent_did,
                i64::try_from(budget).unwrap(),
            )
            .await;
        }
        if input.reload {
            let signer = tool.access().unwrap().managed_agent_signer();
            tool = ToolCallLifecycle::load_by_doc_id_as(
                node.clone(),
                tool.actor().clone(),
                tool.doc_id().unwrap(),
                &tool.node_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .expect("reloaded running tool");
            tool.bind_agent_signer(signer).unwrap();
        }
        let mut replay = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            tool.actor().clone(),
            tool.doc_id().unwrap(),
            &tool.node_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .expect("running tool for a later same-state replay");
        replay.inherit_signer(&tool).unwrap();
        let timed_out = tool.timeout().await.unwrap();
        let presented = interrupted_presentation(&tool).await;
        let (shown, cause) = presented
            .split_once("tool call deadline exceeded at ")
            .unwrap_or_else(|| panic!("diagnostic names the deadline: {presented:?}"));
        assert!(!cause.is_empty(), "{presented:?}");
        let tail = shown.strip_suffix('\n').unwrap_or(shown).to_owned();
        configure_bash_output_budget(
            &client,
            &tool.node_did,
            &agent_did,
            i64::try_from(input.later_budget).unwrap(),
        )
        .await;
        let replay_timed_out = replay.timeout().await.unwrap();
        assert_eq!(interrupted_presentation(&tool).await, presented);
        case.check(Diagnostic {
            timed_out,
            tail,
            replay_timed_out,
        });
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn generated_terminal_diagnostic_replay_cases_bind_native_replay() {
    use gents_lean_vocab::families::terminal_diagnostic_replay_contracts_stored_replay::{
        PresentationWire, ReplayOutput,
    };
    use gents_protocol::output::{PayloadPresentation, PresentationPart};
    let native = |wire: &PresentationWire| {
        if wire.kind == "full" {
            return PayloadPresentation::Full;
        }
        PayloadPresentation::Composed {
            parts: wire
                .parts
                .iter()
                .map(|part| match &part.bytes {
                    Some(bytes) => PresentationPart::Literal {
                        text: String::from_utf8(modeled_bytes(bytes)).expect("modeled literal"),
                    },
                    None => PresentationPart::OutputRange {
                        start_byte: part.start.expect("modeled range start"),
                        end_byte: part.stop.expect("modeled range end"),
                    },
                })
                .collect(),
        }
    };
    gents_lean_vocab::families::terminal_diagnostic_replay_contracts_stored_replay::replay(|i| {
        let raw = String::from_utf8(modeled_bytes(&i.raw)).expect("valid replay source");
        let cause = String::from_utf8(modeled_bytes(&i.cause)).expect("valid replay cause");
        let accepted =
            diagnostic_replay_matches(&raw, &cause, &native(&i.stored)).expect("replay check");
        if accepted {
            let budget = usize::try_from(i.configured_budget).unwrap();
            let current = terminal_diagnostic_presentation(&raw, &cause, budget).unwrap();
            assert!(
                diagnostic_replay_matches(&raw, &cause, &current).unwrap(),
                "freshly planned diagnostic must also replay"
            );
        }
        ReplayOutput { accepted }
    });
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

    node.shutdown().await.expect("shut down test node");
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
    let mut replay_parent = ToolCallLifecycle::load_by_doc_id_as(
        node.clone(),
        parent.actor().clone(),
        &parent_doc_id,
        &parent.node_did,
        &parent.session_id,
        parent.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    replay_parent.inherit_signer(&parent).unwrap();
    let replay = replay_parent
        .admit_spawned_background(admission(deadline), receipt)
        .await
        .unwrap();
    assert_eq!(replay.doc_id(), Some(child_doc_id.as_str()));
    assert_eq!(replay.state, ToolCallState::Completed);

    let messages = tool_delivery_rows(&parent).await;
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

    let segments = observe(
        &parent,
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(child.request_doc_id.as_deref().unwrap()),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ),
    )
    .await;
    assert!(!segments.has_errors(), "{:#?}", segments.errors);
    let child_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()
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
        gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
            &child.access().unwrap(),
            &child_doc_id,
            child.request_doc_id.as_deref().unwrap(),
            &child.session_id,
            &child.node_did,
            child.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .into_text(),
        "child raw output",
        "spawned terminal output is read from its physical ToolOutput source, not a fabricated provider reply"
    );

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn spawned_background_admission_rejects_a_stale_parent_generation() {
    let (node, path, mut parent) = published_spawn_parent("stale").await;
    let request_doc_id = parent.request_doc_id.clone().unwrap();
    let deadline = parent.deadline_at;
    let changed = parent.access().unwrap().write("test.stale_parent_generation", &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "stale-generation" }}) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(&request_doc_id),
    )).await.unwrap();
    assert_eq!(
        changed["data"]["update_AgentRequest"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let Err(refusal) = parent
        .admit_spawned_background(admission(deadline), "receipt")
        .await
    else {
        panic!("a stale generation holds no request lease")
    };
    assert!(
        matches!(
            refusal.downcast_ref::<crate::call::hook::persistence::BackgroundSlotRefusal>(),
            Some(crate::call::hook::persistence::BackgroundSlotRefusal::RequestStopped)
        ),
        "the refusal is the typed slot refusal its callers classify: {refusal:#}"
    );
    let parent_doc_id = gents_store::graphql::escape_graphql_string(parent.doc_id().unwrap());
    let children = observe(&parent, &format!(
        r#"{{ AgentToolCall(filter: {{ spawned_by_tool_call_doc_id: {{ _eq: "{parent_doc_id}" }} }}) {{ _docID }} }}"#,
    )).await;
    assert!(!children.has_errors(), "{:#?}", children.errors);
    assert!(children.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await.expect("shut down test node");
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
    let rows = tool_delivery_rows(&bridge).await;
    let receipts = rows
        .iter()
        .filter(|row| {
            matches!(&row.message.publication,
        MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == &tool_doc_id)
        })
        .count();
    assert_eq!(receipts, 1);
    let response = observe(
        &bridge,
        &format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(&tool_doc_id)
        ),
    )
    .await;
    assert_eq!(
        response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "running"
    );
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn background_bridge_completion_closes_its_source_and_notifies_without_a_second_native_result(
) {
    let (node, path, mut bridge) = published_background_bridge("receipt-completion").await;
    let tool_doc_id = bridge.doc_id().unwrap().to_owned();
    let final_bytes = "child terminal bytes";

    assert!(bridge.publish_background_receipt("started").await.unwrap());
    let mut reloaded = ToolCallLifecycle::load_by_doc_id_as(
        node.clone(),
        bridge.actor().clone(),
        &tool_doc_id,
        &bridge.node_did,
        &bridge.session_id,
        bridge.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .expect("rehydrated background bridge");
    reloaded.inherit_signer(&bridge).unwrap();
    bridge = reloaded;
    assert!(bridge
        .settle_session_message(
            &crate::call::lifecycle::CausedRequestTerminal::Completed {
                output: final_bytes.into()
            },
            None,
        )
        .await
        .unwrap());
    assert_eq!(bridge.state, ToolCallState::Completed);

    crate::background::completion::append_background_tool_completion(
        &node,
        &bridge.access().unwrap(),
        &bridge.session_id,
        "request-receipt-completion",
        &tool_doc_id,
        &bridge.tool_name,
        "completed",
        final_bytes,
        None,
        gents_protocol::request_lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap();

    let response = observe(
        &bridge,
        &format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(&tool_doc_id)
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    assert_eq!(
        response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "completed"
    );

    let segments = observe(
        &bridge,
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(bridge.request_doc_id.as_deref().unwrap()),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ),
    )
    .await;
    assert!(!segments.has_errors(), "{:#?}", segments.errors);
    let tool_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()
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

    let rows = tool_delivery_rows(&bridge).await;
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

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn concurrent_background_bridge_completion_replays_one_exact_closure() {
    let (node, path, mut bridge) = published_background_bridge("concurrent-replay").await;
    let tool_doc_id = bridge.doc_id().unwrap().to_owned();
    assert!(bridge.publish_background_receipt("started").await.unwrap());
    let load = || {
        ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            bridge.actor().clone(),
            &tool_doc_id,
            &bridge.node_did,
            &bridge.session_id,
            bridge.requester_did.as_deref(),
        )
    };
    let mut first = load().await.unwrap().expect("first admitted bridge");
    let mut second = load().await.unwrap().expect("second admitted bridge");
    first.inherit_signer(&bridge).unwrap();
    second.inherit_signer(&bridge).unwrap();
    let one_result = crate::call::lifecycle::CausedRequestTerminal::Completed {
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
    replay.inherit_signer(&bridge).unwrap();
    assert!(!replay
        .settle_session_message(
            &crate::call::lifecycle::CausedRequestTerminal::Completed {
                output: "one result".into()
            },
            None,
        )
        .await
        .unwrap());
    assert!(replay
        .settle_session_message(
            &crate::call::lifecycle::CausedRequestTerminal::Completed {
                output: "different result".into()
            },
            None,
        )
        .await
        .is_err());

    let request_doc_id = bridge.request_doc_id.as_deref().unwrap();
    let response = observe(
        &bridge,
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(request_doc_id),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let closures = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()
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
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}
