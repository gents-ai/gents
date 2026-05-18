use super::*;
use defra_agent::StreamWriter;

#[derive(Debug, Deserialize)]
struct StreamingResponseRow {
    content: String,
    reasoning: Option<String>,
    status: String,
    error_message: Option<String>,
    token_count: i64,
    materialized_message_sequence: Option<i64>,
    interrupted_at: Option<String>,
}

pub(super) async fn generated_streaming_response_cases_pin_lifecycle_contract() {
    let cases = lean_response_transition_cases();
    assert_eq!(cases.len(), 12);

    let expected_names = [
        "begin_emits_streaming_empty",
        "write_tokens_advances_progress",
        "write_reasoning_no_token_bump",
        "flush_pending_is_abstract_noop",
        "reset_tail_clears_but_preserves_tokens",
        "finalize_complete_clears_and_materializes",
        "finalize_error_inference_failed_clears",
        "finalize_error_idle_timeout_requires_deadline",
        "recover_interrupted_keeps_content",
        "observe_idempotent_finalize_is_noop",
        "set_interrupted_at_does_not_change_status",
        "bridge_completed_pairs_request_committed",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<BTreeSet<_>>(),
        expected_names
    );

    for case in cases {
        drive_streaming_response_case(case).await;
    }
}

async fn drive_streaming_response_case(case: &lean_vocab_test::LeanResponseTransitionCase) {
    assert!(case.legal, "streaming case {} should be legal", case.name);
    assert!(
        case.post_token_count >= case.pre_token_count,
        "streaming case {} should not decrease token count",
        case.name
    );

    let db = test_db(&format!("streaming-{}", case.name)).await;
    let request_id = format!("{}-{}", case.name, uuid::Uuid::new_v4());
    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    let created_at = chrono::Utc::now().to_rfc3339();
    let request_status = if case.pre_status == "complete" {
        "completed"
    } else {
        "processing"
    };
    let request_doc_id = create_request(
        &db.node,
        &request_id,
        &session_id,
        request_status,
        &created_at,
    )
    .await;
    let writer = DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::from_millis(0));

    let doc_id = if case.pre_status == "complete" {
        create_manual_response(
            &db.node,
            &request_id,
            &session_id,
            &case.pre_status,
            case.pre_token_count,
            case.pre_materialized_seq,
        )
        .await
    } else {
        let doc_id = writer
            .begin(&session_id, &request_id, AGENT_NAME)
            .await
            .expect("begin streaming response");
        seed_streaming_tail(&writer, &doc_id, case.pre_token_count, &case.pre_live_tail).await;
        doc_id
    };

    assert_streaming_response_shape(&db.node, &doc_id, case, ResponsePhase::Pre).await;

    match case.action.as_str() {
        "begin" => {}
        "write_tokens" => {
            let delta = case
                .post_token_count
                .checked_sub(case.pre_token_count)
                .expect("write_tokens delta");
            writer
                .write_tokens(&doc_id, &tokens(delta))
                .await
                .expect("write tokens");
            writer.flush_pending(&doc_id).await.expect("flush tokens");
        }
        "write_reasoning" => {
            writer
                .write_reasoning(&doc_id, "reasoning trace")
                .await
                .expect("write reasoning");
            writer
                .flush_pending(&doc_id)
                .await
                .expect("flush reasoning");
        }
        "flush" => {
            writer.flush_pending(&doc_id).await.expect("flush pending");
        }
        "reset_tail" => {
            writer.reset_tail(&doc_id).await.expect("reset tail");
        }
        "finalize_complete" => {
            if let Some(sequence) = case.post_materialized_seq {
                mark_materialized(db.node.clone(), &request_id, sequence as u32).await;
            }
            writer
                .finalize(&doc_id, defra_agent::streaming::StreamStatus::Complete)
                .await
                .expect("finalize complete");
        }
        "finalize_error" => {
            if let Some(reason) = case.error_reason.as_deref() {
                writer
                    .set_error_message(&doc_id, reason)
                    .await
                    .expect("set error reason");
            }
            writer
                .finalize(&doc_id, defra_agent::streaming::StreamStatus::Error)
                .await
                .expect("finalize error");
        }
        "recover_interrupted" => {
            let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
                .await
                .expect("recover streaming response");
            assert_eq!(report.responses_recovered, 1, "{}", case.name);
            assert_eq!(report.requests_recovered, 1, "{}", case.name);
        }
        "observe_idempotent_finalize" => {
            writer
                .finalize(&doc_id, defra_agent::streaming::StreamStatus::Complete)
                .await
                .expect("idempotent finalize");
        }
        "set_interrupted_at" => {
            let interrupted_at = chrono::Utc::now().to_rfc3339();
            assert!(
                writer
                    .write_interrupted_at(&doc_id, &interrupted_at)
                    .await
                    .expect("write interrupted_at"),
                "{}: interrupted_at update should match response",
                case.name
            );
        }
        other => panic!("unsupported streaming action {other:?} for {}", case.name),
    }

    assert_streaming_response_shape(&db.node, &doc_id, case, ResponsePhase::Post).await;
    assert_request_bridge_shape(&db.node, &request_doc_id, case).await;
}

#[derive(Clone, Copy)]
enum ResponsePhase {
    Pre,
    Post,
}

async fn assert_streaming_response_shape(
    node: &EmbeddedNode,
    doc_id: &str,
    case: &lean_vocab_test::LeanResponseTransitionCase,
    phase: ResponsePhase,
) {
    let row = load_streaming_response_row(node, doc_id).await;
    let (status, live_tail, token_count, materialized_sequence) = match phase {
        ResponsePhase::Pre => (
            case.pre_status.as_str(),
            case.pre_live_tail.as_str(),
            case.pre_token_count,
            case.pre_materialized_seq,
        ),
        ResponsePhase::Post => (
            case.post_status.as_str(),
            case.post_live_tail.as_str(),
            case.post_token_count,
            case.post_materialized_seq,
        ),
    };
    let phase_name = match phase {
        ResponsePhase::Pre => "pre",
        ResponsePhase::Post => "post",
    };

    assert_eq!(
        row.status.as_str(),
        status,
        "{} {phase_name}: status",
        case.name
    );
    assert_eq!(
        live_tail_shape(&row),
        live_tail,
        "{} {phase_name}: live tail",
        case.name
    );
    assert_eq!(
        row.token_count as usize, token_count,
        "{} {phase_name}: token_count",
        case.name
    );
    assert_eq!(
        row.materialized_message_sequence
            .map(|sequence| sequence as usize),
        materialized_sequence,
        "{} {phase_name}: materialized sequence",
        case.name
    );

    if matches!(phase, ResponsePhase::Post) {
        match case.error_reason.as_deref() {
            Some("daemonRestartRecovery") => {
                assert!(
                    row.content.contains("Response interrupted"),
                    "{}: recovery reason should be visible in recovered content",
                    case.name
                );
            }
            Some(reason) => {
                assert_eq!(
                    row.error_message.as_deref(),
                    Some(reason),
                    "{}: error reason",
                    case.name
                );
            }
            None => {}
        }

        if case.action == "set_interrupted_at" {
            assert!(
                row.interrupted_at
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "{}: interrupted_at",
                case.name
            );
        }
    }
}

async fn assert_request_bridge_shape(
    node: &EmbeddedNode,
    request_doc_id: &str,
    case: &lean_vocab_test::LeanResponseTransitionCase,
) {
    let Some(expected_state) = case.expected_request_state.as_deref() else {
        return;
    };
    let snapshot = fetch_request_snapshot(node, request_doc_id).await;
    assert_eq!(
        snapshot.lifecycle_state.as_str(),
        expected_state,
        "{}: request lifecycle_state",
        case.name
    );
    let expected_status = match expected_state {
        "completed" => "completed",
        "failed" => "error",
        other => other,
    };
    assert_eq!(
        snapshot.status.as_str(),
        expected_status,
        "{}: request status",
        case.name
    );
    assert_eq!(
        case.expected_request_persistence.as_deref(),
        Some("committed"),
        "{}: terminal bridge persistence",
        case.name
    );
}

async fn seed_streaming_tail(
    writer: &DefraStreamWriter,
    doc_id: &str,
    token_count: usize,
    live_tail: &str,
) {
    if token_count > 0 {
        writer
            .write_tokens(doc_id, &tokens(token_count))
            .await
            .expect("seed tokens");
        writer.flush_pending(doc_id).await.expect("seed flush");
    } else if live_tail == "nonEmpty" {
        writer
            .write_reasoning(doc_id, "seed reasoning")
            .await
            .expect("seed reasoning");
        writer
            .flush_pending(doc_id)
            .await
            .expect("seed reasoning flush");
    }
}

fn tokens(count: usize) -> String {
    (0..count)
        .map(|index| format!("t{index}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn create_manual_response(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    token_count: usize,
    materialized_sequence: Option<usize>,
) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at = if matches!(status, "complete" | "error") {
        now.as_str()
    } else {
        ""
    };
    let materialized_fields = materialized_sequence
        .map(|sequence| {
            format!(r#"materialized_message_sequence: {sequence}, materialized_at: "{now}","#)
        })
        .unwrap_or_default();
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let status = escape_graphql_string(status);
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{request_id}",
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                content: "",
                reasoning: "",
                status: "{status}",
                error_message: "",
                token_count: {token_count},
                progress_seq: 0,
                {materialized_fields}
                created_at: "{now}",
                completed_at: "{completed_at}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create manual AgentResponse failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentResponse(filter: {{ response_key: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    support::first_row::<DocIdRow>(&resp, "AgentResponse").doc_id
}

async fn mark_materialized(node: std::sync::Arc<EmbeddedNode>, request_id: &str, sequence: u32) {
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        node,
        "streaming-materialized-session",
        AGENT_NAME,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .expect("resume materialization hook");
    hook.set_active_request_id(Some(request_id.to_string()))
        .await;
    hook.mark_current_response_materialized(sequence)
        .await
        .expect("mark response materialized");
}

async fn load_streaming_response_row(node: &EmbeddedNode, doc_id: &str) -> StreamingResponseRow {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                content
                reasoning
                status
                error_message
                token_count
                materialized_message_sequence
                interrupted_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    support::first_row::<StreamingResponseRow>(&resp, "AgentResponse")
}

fn live_tail_shape(row: &StreamingResponseRow) -> &'static str {
    let content_non_empty = !row.content.trim().is_empty();
    let reasoning_non_empty = row
        .reasoning
        .as_deref()
        .is_some_and(|reasoning| !reasoning.trim().is_empty());
    if content_non_empty || reasoning_non_empty {
        "nonEmpty"
    } else {
        "empty"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocIdRow {
    doc_id: String,
}

impl<'de> Deserialize<'de> for DocIdRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Row {
            #[serde(rename = "_docID")]
            doc_id: String,
        }

        Row::deserialize(deserializer).map(|row| Self { doc_id: row.doc_id })
    }
}

pub(super) fn generated_compaction_reducer_cases_pin_contract() {
    let cases = lean_compaction_reducer_cases();
    assert_eq!(cases.len(), 10);

    let expected_names = [
        "identity_reducer_is_no_op",
        "identity_preserves_pair_atomicity",
        "identity_preserves_message_order",
        "strip_preserves_pair_atomicity",
        "strip_preserves_message_order",
        "strip_is_strictly_idempotent",
        "reduction_blocked_when_response_streaming",
        "reduction_allowed_when_response_terminal",
        "no_orphaned_tool_results_after_strip",
        "reapply_preserves_view_coherent",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<BTreeSet<_>>(),
        expected_names
    );

    for case in cases {
        drive_compaction_reducer_case(case);
    }
}

fn drive_compaction_reducer_case(case: &lean_vocab_test::LeanCompactionReducerCase) {
    assert!(case.legal, "compaction case {} should be legal", case.name);

    let input = compaction_messages_for_case(case);
    assert_eq!(
        input.len(),
        case.pre_message_count,
        "{}: pre_message_count",
        case.name
    );

    let reduced = apply_compaction_reducer(case, input.clone());
    assert_eq!(
        reduced.len(),
        case.post_message_count,
        "{}: post_message_count",
        case.name
    );
    assert_eq!(
        preserves_pair_closure(&input, &reduced),
        case.preserves_pairs,
        "{}: preserves_pairs",
        case.name
    );
    assert_eq!(
        abstract_prompt_view(&input) == abstract_prompt_view(&reduced),
        case.preserves_order,
        "{}: preserves_order",
        case.name
    );

    let structurally_identity = abstract_prompt_view(&input) == abstract_prompt_view(&reduced);
    if case.reducer_is_identity {
        assert!(
            structurally_identity,
            "{}: reducer should be identity on the Lean structural projection",
            case.name
        );
    } else {
        assert_ne!(
            reduced, input,
            "{}: terminal safe reduction should be able to change runtime payloads",
            case.name
        );
    }

    if case.name == "strip_is_strictly_idempotent" {
        let reapplied = defra_agent::compaction::strip_tool_results(reduced.clone()).0;
        assert_eq!(
            abstract_prompt_view(&reduced),
            abstract_prompt_view(&reapplied),
            "{}: strip is idempotent on the Lean structural projection",
            case.name
        );
    }

    if case.name == "reapply_preserves_view_coherent" {
        let reapplied = apply_compaction_reducer(case, reduced.clone());
        assert!(
            pair_closed(&reapplied),
            "{}: reapply preserves pair closure",
            case.name
        );
        assert_eq!(
            abstract_prompt_view(&reduced),
            abstract_prompt_view(&reapplied),
            "{}: reapply preserves ordering projection",
            case.name
        );
    }
}

fn apply_compaction_reducer(
    case: &lean_vocab_test::LeanCompactionReducerCase,
    input: Vec<Message>,
) -> Vec<Message> {
    match case.reducer.as_str() {
        "identity" => input,
        "strip_tool_results" => defra_agent::compaction::strip_tool_results(input).0,
        "any_valid" if case.safe_to_reduce => defra_agent::compaction::strip_tool_results(input).0,
        "any_valid" => input,
        other => panic!("unsupported compaction reducer {other:?} for {}", case.name),
    }
}

fn compaction_messages_for_case(case: &lean_vocab_test::LeanCompactionReducerCase) -> Vec<Message> {
    match case.pre_message_count {
        0 => Vec::new(),
        1 => vec![compaction_tool_result_message(
            "call-1",
            "large terminal payload",
        )],
        2 => vec![
            compaction_tool_call_message("call-1"),
            compaction_tool_result_message("call-1", "large tool payload"),
        ],
        3 => vec![
            compaction_text_message("user", "first"),
            compaction_tool_call_message("call-1"),
            compaction_tool_result_message("call-1", "large tool payload"),
        ],
        other => panic!(
            "unsupported compaction pre_message_count {other} for {}",
            case.name
        ),
    }
}

fn compaction_text_message(role: &str, text: &str) -> Message {
    match role {
        "user" => Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        },
        "assistant" => Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        },
        other => panic!("unsupported compaction text role {other:?}"),
    }
}

fn compaction_tool_call_message(call_id: &str) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: json!({ "file_path": "/tmp/compaction-contract.txt" }),
            },
            signature: None,
            additional_params: None,
        })),
    }
}

fn compaction_tool_result_message(call_id: &str, payload: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: payload.to_string(),
            })),
        })),
    }
}

fn preserves_pair_closure(pre: &[Message], post: &[Message]) -> bool {
    !pair_closed(pre) || pair_closed(post)
}

fn pair_closed(messages: &[Message]) -> bool {
    let call_ids = messages
        .iter()
        .flat_map(assistant_tool_call_ids)
        .collect::<HashSet<_>>();
    messages
        .iter()
        .flat_map(user_tool_result_ids)
        .all(|call_id| call_ids.contains(&call_id))
}

fn abstract_prompt_view(messages: &[Message]) -> Vec<String> {
    messages.iter().flat_map(message_shape).collect()
}

fn message_shape(message: &Message) -> Vec<String> {
    match message {
        Message::System { .. } => vec!["system".to_string()],
        Message::Assistant { content, .. } => content
            .iter()
            .map(|item| match item {
                AssistantContent::Text(_) => "assistant:text".to_string(),
                AssistantContent::ToolCall(tool_call) => {
                    format!("assistant:tool_call:{}", tool_call_id(tool_call))
                }
                other => format!("assistant:{other:?}"),
            })
            .collect(),
        Message::User { content } => content
            .iter()
            .map(|item| match item {
                UserContent::Text(_) => "user:text".to_string(),
                UserContent::ToolResult(tool_result) => {
                    format!("user:tool_result:{}", tool_result_id(tool_result))
                }
                other => format!("user:{other:?}"),
            })
            .collect(),
    }
}

fn assistant_tool_call_ids(message: &Message) -> Vec<String> {
    let Message::Assistant { content, .. } = message else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::ToolCall(tool_call) => Some(tool_call_id(tool_call)),
            _ => None,
        })
        .collect()
}

fn user_tool_result_ids(message: &Message) -> Vec<String> {
    let Message::User { content } = message else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|item| match item {
            UserContent::ToolResult(tool_result) => Some(tool_result_id(tool_result)),
            _ => None,
        })
        .collect()
}

fn tool_call_id(tool_call: &ToolCall) -> String {
    tool_call
        .call_id
        .clone()
        .unwrap_or_else(|| tool_call.id.clone())
}

fn tool_result_id(tool_result: &ToolResult) -> String {
    tool_result
        .call_id
        .clone()
        .unwrap_or_else(|| tool_result.id.clone())
}
