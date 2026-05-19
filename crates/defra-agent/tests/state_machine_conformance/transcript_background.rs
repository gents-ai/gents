use super::*;

#[derive(Clone, Default)]
struct TranscriptConformanceModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TranscriptConformanceModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in transcript conformance tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in transcript conformance tests".to_string(),
        ))
    }
}

fn transcript_user_message(text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    }
}

fn transcript_assistant_tool_call_message(model_call_id: &str) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: model_call_id.to_string(),
            call_id: Some(model_call_id.to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: json!({ "file_path": "/tmp/transcript-contract.txt" }),
            },
            signature: None,
            additional_params: None,
        })),
    }
}

fn transcript_tool_result_message(result_id: &str, text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: result_id.to_string(),
            call_id: Some(result_id.to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: text.to_string(),
            })),
        })),
    }
}

async fn transcript_hook_fixture(test_name: &str) -> (support::TestDb, DefraSessionHook, String) {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        AGENT_NAME,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .expect("resume transcript hook");
    hook.set_active_request_id(Some(format!("{test_name}-request")))
        .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    (db, hook, session_id)
}

async fn transcript_messages_and_calls(
    node: &EmbeddedNode,
    session_id: &str,
) -> (Vec<MessageSnapshot>, Vec<ToolCallSnapshot>, Vec<Message>) {
    let messages = fetch_message_snapshots_for_session(node, session_id).await;
    let tool_calls = fetch_tool_call_snapshots_for_session(node, session_id).await;
    let history = defra_agent::load_history(node, session_id)
        .await
        .expect("load transcript history");
    (messages, tool_calls, history)
}

fn transcript_tool_result_count(history: &[Message]) -> usize {
    history
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::User { content }
                    if matches!(content.first_ref(), UserContent::ToolResult(_))
            )
        })
        .count()
}

fn transcript_ordered(messages: &[MessageSnapshot]) -> bool {
    messages
        .windows(2)
        .all(|window| window[0].sequence < window[1].sequence)
}

fn transcript_strong_drain(tool_calls: &[ToolCallSnapshot]) -> bool {
    tool_calls
        .iter()
        .all(|call| call.lifecycle_state.as_deref() != Some("running"))
}

fn transcript_pair_closed(
    messages: &[MessageSnapshot],
    tool_calls: &[ToolCallSnapshot],
    history: &[Message],
) -> bool {
    let tool_calls_reserved_by_assistant_message = tool_calls.iter().all(|call| {
        messages.iter().any(|message| {
            message.sequence == call.message_sequence && message.role.as_str() == "assistant"
        })
    });
    let no_running_tool_calls = transcript_strong_drain(tool_calls);
    let completed_tool_call_count = tool_calls
        .iter()
        .filter(|call| call.lifecycle_state.as_deref() == Some("completed"))
        .count();
    let completed_calls_have_results = completed_tool_call_count == 0
        || transcript_tool_result_count(history) == completed_tool_call_count;

    tool_calls_reserved_by_assistant_message
        && no_running_tool_calls
        && completed_calls_have_results
}

async fn assert_transcript_counts(
    label: &str,
    node: &EmbeddedNode,
    session_id: &str,
    expected_messages: usize,
    expected_tool_calls: usize,
) {
    let (messages, tool_calls, _) = transcript_messages_and_calls(node, session_id).await;
    assert_eq!(
        messages.len(),
        expected_messages,
        "{label}: AgentMessage count"
    );
    assert_eq!(
        tool_calls.len(),
        expected_tool_calls,
        "{label}: AgentToolCall count"
    );
}

async fn assert_transcript_post_state(
    case: &lean_vocab_test::LeanTranscriptCase,
    node: &EmbeddedNode,
    session_id: &str,
) -> (Vec<MessageSnapshot>, Vec<ToolCallSnapshot>, Vec<Message>) {
    let (messages, tool_calls, history) = transcript_messages_and_calls(node, session_id).await;
    assert_eq!(
        messages.len(),
        case.post_message_count,
        "{}: post_message_count",
        case.name
    );
    assert_eq!(
        tool_calls.len(),
        case.post_tool_call_count,
        "{}: post_tool_call_count",
        case.name
    );
    assert_eq!(
        transcript_ordered(&messages),
        case.expected_ordered,
        "{}: expected_ordered",
        case.name
    );
    assert_eq!(
        transcript_pair_closed(&messages, &tool_calls, &history),
        case.expected_pair_closed,
        "{}: expected_pair_closed",
        case.name
    );
    assert_eq!(
        transcript_strong_drain(&tool_calls),
        case.expected_strong_drain,
        "{}: expected_strong_drain",
        case.name
    );
    (messages, tool_calls, history)
}

async fn persist_completed_tool_sequence(
    test_name: &str,
    case: &lean_vocab_test::LeanTranscriptCase,
) -> (support::TestDb, DefraSessionHook, String, u32) {
    let (db, hook, session_id) = transcript_hook_fixture(test_name).await;
    assert_transcript_counts(
        &format!("{} pre-state", case.name),
        &db.node,
        &session_id,
        case.pre_message_count,
        case.pre_tool_call_count,
    )
    .await;

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_completion_call(
            &hook,
            &transcript_user_message("run transcript conformance tool"),
            &[],
        )
        .await,
        HookAction::Continue
    ));

    let model_call_id = format!("result-{}", case.logical_result_id);
    let internal_call_id = format!("internal-{}", case.logical_result_id);
    let payload = format!("payload-{}", case.payload_hash);
    let tool_args = r#"{"file_path":"/tmp/transcript-contract.txt"}"#;

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some(model_call_id.clone()),
            &internal_call_id,
            tool_args,
        )
        .await,
        ToolCallHookAction::Continue
    ));

    let assistant_sequence = hook
        .persist_message(&transcript_assistant_tool_call_message(&model_call_id))
        .await
        .expect("persist assistant tool-call message");
    assert_eq!(
        assistant_sequence as usize, case.assistant_sequence,
        "{}: assistant_sequence",
        case.name
    );

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_result(
            &hook,
            "read",
            Some(model_call_id.clone()),
            &internal_call_id,
            tool_args,
            &payload,
        )
        .await,
        HookAction::Continue
    ));

    (db, hook, session_id, case.result_sequence as u32)
}

fn assert_transcript_case_shape() {
    let cases = lean_transcript_cases();
    assert_eq!(cases.len(), 6);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "ordering_user_assistant_tool_result",
            "dedupe_duplicate_reuses_sequence",
            "distinct_result_ids_append_distinct_rows",
            "completed_tool_pair_closed",
            "explicit_drain_terminalizes_ownership",
            "drop_abandon_not_strong_drain",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let ordering = lean_transcript_case("ordering_user_assistant_tool_result");
    assert!(ordering.legal);
    assert_eq!(ordering.group.as_str(), "ordering");
    assert_eq!(ordering.pre_message_count, 0);
    assert_eq!(ordering.post_message_count, 3);
    assert_eq!(ordering.pre_tool_call_count, 0);
    assert_eq!(ordering.post_tool_call_count, 1);
    assert_eq!(ordering.assistant_sequence, 2);
    assert_eq!(ordering.result_sequence, 3);
    assert!(ordering.expected_ordered);
    assert!(ordering.expected_pair_closed);

    let dedupe = lean_transcript_case("dedupe_duplicate_reuses_sequence");
    assert_eq!(dedupe.group.as_str(), "dedupe");
    assert_eq!(dedupe.action.as_str(), "observe_duplicate_tool_result");
    assert_eq!(dedupe.pre_message_count, dedupe.post_message_count);
    assert_eq!(dedupe.pre_tool_call_count, dedupe.post_tool_call_count);
    assert_eq!(dedupe.logical_result_id, ordering.logical_result_id);
    assert_eq!(dedupe.payload_hash, ordering.payload_hash);
    assert!(dedupe.expected_duplicate_reused_sequence);
    assert_eq!(dedupe.result_sequence, ordering.result_sequence);

    let distinct = lean_transcript_case("distinct_result_ids_append_distinct_rows");
    assert_eq!(distinct.group.as_str(), "dedupe");
    assert_eq!(distinct.payload_hash, ordering.payload_hash);
    assert_ne!(distinct.logical_result_id, ordering.logical_result_id);
    assert_eq!(distinct.pre_message_count + 1, distinct.post_message_count);
    assert!(!distinct.expected_duplicate_reused_sequence);

    let pair = lean_transcript_case("completed_tool_pair_closed");
    assert_eq!(pair.group.as_str(), "pairing");
    assert!(pair.expected_pair_closed);
    assert!(pair.expected_ordered);

    let drain = lean_transcript_case("explicit_drain_terminalizes_ownership");
    assert_eq!(drain.group.as_str(), "hook_boundary");
    assert_eq!(drain.pre_in_flight_count, 1);
    assert_eq!(drain.post_in_flight_count, 0);
    assert!(drain.expected_strong_drain);

    let abandon = lean_transcript_case("drop_abandon_not_strong_drain");
    assert_eq!(abandon.group.as_str(), "hook_boundary");
    assert_eq!(abandon.action.as_str(), "abandon_hook_ownership");
    assert_eq!(abandon.pre_in_flight_count, 1);
    assert_eq!(abandon.post_in_flight_count, 0);
    assert!(!abandon.expected_strong_drain);
    assert!(!abandon.expected_pair_closed);

    for case in cases {
        assert!(case.legal, "transcript case {} should be legal", case.name);
        assert!(
            case.expected_ordered,
            "transcript case {} should preserve ordering",
            case.name
        );
    }
}

pub(super) fn generated_r6_backgrounding_cases_pin_tool_backgrounding_contract() {
    let cases = lean_r6_backgrounding_cases();
    assert_eq!(cases.len(), 7);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "background_tool_budget_count_7_admits_spawn",
            "background_tool_budget_count_8_rejects_spawn",
            "tool_kind_bridge_complete_persists_result",
            "tool_kind_bridge_failure_cancelled_projects_parent_cancelled",
            "background_recovery_running_live_parent_to_cancelled",
            "background_completion_source_writes_canonical_key",
            "legacy_subagent_completion_source_aliases_canonical_key",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.max_backgrounded, 8, "{}", case.name);
        assert_eq!(case.await_mode.as_str(), "background", "{}", case.name);
        assert_eq!(case.cancel_policy.as_str(), "cascade", "{}", case.name);
        assert_eq!(case.child_request_id.as_deref(), None, "{}", case.name);
    }

    let admit = lean_r6_backgrounding_case("background_tool_budget_count_7_admits_spawn");
    assert!(admit.legal);
    assert_eq!(admit.pre_live_count, 7);
    assert_eq!(admit.terminal_state.as_str(), "running");

    let reject = lean_r6_backgrounding_case("background_tool_budget_count_8_rejects_spawn");
    assert!(!reject.legal);
    assert_eq!(reject.pre_live_count, 8);
    assert_eq!(
        reject.error_code.as_deref(),
        Some("background_tool_budget_exceeded")
    );

    let completed = lean_r6_backgrounding_case("tool_kind_bridge_complete_persists_result");
    assert!(completed.legal);
    assert_eq!(completed.terminal_state.as_str(), "completed");
    assert_eq!(completed.result.as_deref(), Some("done"));

    let cancelled =
        lean_r6_backgrounding_case("tool_kind_bridge_failure_cancelled_projects_parent_cancelled");
    assert_eq!(cancelled.terminal_state.as_str(), "cancelled");
    assert_eq!(cancelled.reason.as_deref(), Some("parent_cancelled"));

    let recovered =
        lean_r6_backgrounding_case("background_recovery_running_live_parent_to_cancelled");
    assert_eq!(
        recovered.action.as_str(),
        "TerminalizeBackgroundedAsInterrupted"
    );
    assert_eq!(recovered.terminal_state.as_str(), "cancelled");
    assert_eq!(recovered.reason.as_deref(), Some("interrupted_on_restart"));

    let canonical = lean_r6_backgrounding_case("background_completion_source_writes_canonical_key");
    assert_eq!(
        canonical.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        canonical.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let legacy =
        lean_r6_backgrounding_case("legacy_subagent_completion_source_aliases_canonical_key");
    assert_eq!(legacy.queue_source.as_deref(), Some("subagent_completion"));
    assert_eq!(legacy.queue_key.as_deref(), canonical.queue_key.as_deref());
}

pub(super) fn generated_r4c_background_work_cases_pin_observable_shapes() {
    let cases = lean_r4c_background_work_cases();
    assert_eq!(cases.len(), 6);

    let names = cases
        .iter()
        .map(LeanR4cBackgroundWorkCase::witness)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "r4c.list_subagents.lineage_rejects",
            "r4c.read_subagent_transcript.cursor_advances",
            "r4c.read_subagent_transcript.hides_bridge_rows",
            "r4c.read_tool_output.dispatch_by_state",
            "r4c.steer_subagent.append_preserves_lineage",
            "r4c.steer_subagent.interrupt_composes",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    match lean_r4c_background_work_case("r4c.list_subagents.lineage_rejects") {
        LeanR4cBackgroundWorkCase::ListSubagentsLineageRejects {
            caller_request_id,
            sibling_request_id,
            sibling_child_id,
            caller_sees_sibling_child,
        } => {
            assert_eq!(caller_request_id, "r4c-w1-caller");
            assert_eq!(sibling_request_id, "r4c-w1-sibling");
            assert_eq!(sibling_child_id, "r4c-w1-sibling-child");
            assert!(!*caller_sees_sibling_child);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_subagent_transcript.cursor_advances") {
        LeanR4cBackgroundWorkCase::ReadTranscriptCursorAdvances {
            child_session_id,
            first_since_sequence,
            first_through_sequence,
            first_next_sequence,
            second_since_sequence,
            second_through_sequence,
            no_gap,
            no_overlap,
        } => {
            assert_eq!(child_session_id, "r4c-w2-session");
            assert_eq!(*first_since_sequence, 0);
            assert_eq!(*first_through_sequence, 5);
            assert_eq!(*first_next_sequence, 6);
            assert_eq!(*second_since_sequence, 6);
            assert_eq!(*second_through_sequence, 10);
            assert_eq!(first_next_sequence, second_since_sequence);
            assert!(*no_gap);
            assert!(*no_overlap);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_subagent_transcript.hides_bridge_rows") {
        LeanR4cBackgroundWorkCase::ReadTranscriptHidesBridgeRows {
            child_session_id,
            bridge_call_id,
            rendered_transcript,
        } => {
            assert_eq!(child_session_id, "r4c-w3-session");
            assert_eq!(bridge_call_id, "r4c-w3-bridge-call");
            assert_eq!(
                rendered_transcript,
                "[assistant seq=2]\nplain assistant message\n"
            );
            assert!(
                !rendered_transcript.contains(bridge_call_id),
                "rendered transcript must hide bridge tool-call rows"
            );
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_tool_output.dispatch_by_state") {
        LeanR4cBackgroundWorkCase::ReadToolOutputDispatchesByState {
            tool_call_id,
            running_source,
            terminal_source,
            running_payload,
            stale_running_payload,
            terminal_payload,
        } => {
            assert_eq!(tool_call_id, "r4c-w4-tool-call");
            assert_eq!(running_source, "ring_buffer");
            assert_eq!(terminal_source, "persisted_tool_completion");
            assert_eq!(running_payload, "ring-buffer-live-tail");
            assert_eq!(stale_running_payload, "stale-ring-buffer-tail");
            assert_eq!(terminal_payload, "persisted-completion-stdout");
            assert_ne!(
                terminal_payload, stale_running_payload,
                "terminal reads must not replay a stale live buffer"
            );
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.steer_subagent.append_preserves_lineage") {
        LeanR4cBackgroundWorkCase::SteerAppendPreservesLineage {
            caller_request_id,
            child_session_id,
            queued_request_id,
            caused_by_parent_request_id,
            queue_source,
            queue_policy,
        } => {
            assert_eq!(caller_request_id, "r4c-w5-caller");
            assert_eq!(child_session_id, "r4c-w5-child-session");
            assert_eq!(queued_request_id, "r4c-w5-queued");
            assert_eq!(caused_by_parent_request_id, caller_request_id);
            assert_eq!(queue_source, "steering");
            assert_eq!(queue_policy, "append");
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.steer_subagent.interrupt_composes") {
        LeanR4cBackgroundWorkCase::SteerInterruptComposes {
            caller_request_id,
            child_session_id,
            interrupted_active_request_id,
            drained_wake_up_request_ids,
            drained_wake_up_queue_key,
            queued_request_id,
            queue_interrupted_request_id,
        } => {
            assert_eq!(caller_request_id, "r4c-w6-caller");
            assert_eq!(child_session_id, "r4c-w6-child-session");
            assert_eq!(interrupted_active_request_id, "r4c-w6-interrupted");
            assert_eq!(
                drained_wake_up_request_ids,
                &vec!["r4c-w6-wake-1".to_string(), "r4c-w6-wake-2".to_string()]
            );
            assert_eq!(
                drained_wake_up_queue_key,
                "background_completion:r4c-w6-child-session"
            );
            assert_eq!(
                drained_wake_up_queue_key,
                &format!("background_completion:{child_session_id}")
            );
            assert_eq!(queued_request_id, "r4c-w6-queued");
            assert_eq!(queue_interrupted_request_id, interrupted_active_request_id);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }
}

pub(super) async fn generated_transcript_cases_drive_agent_message_ordering_contract() {
    assert_transcript_case_shape();

    let ordering = lean_transcript_case("ordering_user_assistant_tool_result");
    let (db, hook, session_id, result_sequence) =
        persist_completed_tool_sequence("transcript-ordering", ordering).await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        ordering.post_in_flight_count,
        "{}: post_in_flight_count",
        ordering.name
    );
    let (messages, tool_calls, history) =
        assert_transcript_post_state(ordering, &db.node, &session_id).await;
    assert_eq!(result_sequence as usize, ordering.result_sequence);
    assert_eq!(
        messages
            .iter()
            .find(|message| message.role.as_str() == "user" && message.sequence > 1)
            .map(|message| message.sequence as usize),
        Some(ordering.result_sequence),
        "{}: result_sequence",
        ordering.name
    );
    assert_eq!(
        tool_calls
            .first()
            .map(|call| call.message_sequence as usize),
        Some(ordering.assistant_sequence),
        "{}: tool call reserves assistant sequence",
        ordering.name
    );
    assert_eq!(
        transcript_tool_result_count(&history),
        1,
        "{}",
        ordering.name
    );

    let dedupe = lean_transcript_case("dedupe_duplicate_reuses_sequence");
    let (db, hook, session_id, first_result_sequence) =
        persist_completed_tool_sequence("transcript-dedupe", ordering).await;
    assert_transcript_counts(
        "dedupe duplicate pre-state",
        &db.node,
        &session_id,
        dedupe.pre_message_count,
        dedupe.pre_tool_call_count,
    )
    .await;
    let duplicate_sequence = hook
        .persist_message(&transcript_tool_result_message(
            &format!("result-{}", dedupe.logical_result_id),
            &format!("payload-{}", dedupe.payload_hash),
        ))
        .await
        .expect("persist duplicate tool-result message");
    assert_eq!(
        duplicate_sequence as usize, dedupe.result_sequence,
        "{}: duplicate reused sequence",
        dedupe.name
    );
    assert_eq!(
        first_result_sequence as usize, dedupe.result_sequence,
        "{}: original sequence",
        dedupe.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        dedupe.post_in_flight_count,
        "{}: post_in_flight_count",
        dedupe.name
    );
    let (messages, _, history) = assert_transcript_post_state(dedupe, &db.node, &session_id).await;
    assert_eq!(messages.len(), dedupe.pre_message_count, "{}", dedupe.name);
    assert_eq!(transcript_tool_result_count(&history), 1, "{}", dedupe.name);

    let distinct = lean_transcript_case("distinct_result_ids_append_distinct_rows");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-distinct").await;
    let seed_result_id = format!("result-{}", ordering.logical_result_id);
    let payload = format!("payload-{}", distinct.payload_hash);
    let first_sequence = hook
        .persist_message(&transcript_tool_result_message(&seed_result_id, &payload))
        .await
        .expect("persist seed tool-result message");
    assert_eq!(first_sequence, 1, "{}: seed sequence", distinct.name);
    assert_transcript_counts(
        "distinct result-id pre-state",
        &db.node,
        &session_id,
        distinct.pre_message_count,
        distinct.pre_tool_call_count,
    )
    .await;
    let distinct_sequence = hook
        .persist_message(&transcript_tool_result_message(
            &format!("result-{}", distinct.logical_result_id),
            &payload,
        ))
        .await
        .expect("persist distinct tool-result message");
    assert_eq!(
        distinct_sequence as usize, distinct.result_sequence,
        "{}: result_sequence",
        distinct.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        distinct.post_in_flight_count,
        "{}: post_in_flight_count",
        distinct.name
    );
    let (_, _, history) = assert_transcript_post_state(distinct, &db.node, &session_id).await;
    assert_eq!(
        transcript_tool_result_count(&history),
        distinct.post_message_count,
        "{}: distinct result rows",
        distinct.name
    );

    let pair = lean_transcript_case("completed_tool_pair_closed");
    let (db, hook, session_id, _) =
        persist_completed_tool_sequence("transcript-pair-closed", pair).await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        pair.post_in_flight_count,
        "{}: post_in_flight_count",
        pair.name
    );
    let (_, tool_calls, history) = assert_transcript_post_state(pair, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("completed"),
        "{}: completed tool call",
        pair.name
    );
    assert_eq!(transcript_tool_result_count(&history), 1, "{}", pair.name);

    let drain = lean_transcript_case("explicit_drain_terminalizes_ownership");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-explicit-drain").await;
    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some("result-drain".to_string()),
            "internal-drain",
            r#"{"file_path":"/tmp/transcript-contract.txt"}"#,
        )
        .await,
        ToolCallHookAction::Continue
    ));
    let assistant_sequence = hook
        .persist_message(&transcript_assistant_tool_call_message("result-drain"))
        .await
        .expect("persist drain assistant message");
    assert_eq!(
        assistant_sequence as usize, drain.assistant_sequence,
        "{}: assistant_sequence",
        drain.name
    );
    assert_transcript_counts(
        "explicit drain pre-state",
        &db.node,
        &session_id,
        drain.pre_message_count,
        drain.pre_tool_call_count,
    )
    .await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        drain.pre_in_flight_count,
        "{}: explicit drain count",
        drain.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        drain.post_in_flight_count,
        "{}: post_in_flight_count",
        drain.name
    );
    let (_, tool_calls, _) = assert_transcript_post_state(drain, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("cancelled"),
        "{}: durable row terminalized",
        drain.name
    );

    let abandon = lean_transcript_case("drop_abandon_not_strong_drain");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-drop-abandon").await;
    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some("result-abandon".to_string()),
            "internal-abandon",
            r#"{"file_path":"/tmp/transcript-contract.txt"}"#,
        )
        .await,
        ToolCallHookAction::Continue
    ));
    assert_transcript_counts(
        "drop abandon pre-state",
        &db.node,
        &session_id,
        abandon.pre_message_count,
        abandon.pre_tool_call_count,
    )
    .await;
    let observer = hook.clone();
    drop(hook);
    assert_eq!(
        observer.cancel_in_flight_tool_calls().await.unwrap(),
        abandon.post_in_flight_count,
        "{}: drop abandons in-memory ownership",
        abandon.name
    );
    let (_, tool_calls, _) = assert_transcript_post_state(abandon, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("running"),
        "{}: durable row remains running after Drop",
        abandon.name
    );
}
