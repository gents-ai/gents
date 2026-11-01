use super::*;

const GROUPED_PROFILE: gents_loop::provider_input::ProviderInputProfile =
    gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions;

pub(super) fn generated_compaction_reducer_cases_pin_contract() {
    let cases = lean_compaction_reducer_cases();
    assert_eq!(cases.len(), 20);

    let expected_names = [
        "identity_reducer_is_no_op",
        "identity_preserves_pair_atomicity",
        "identity_preserves_message_order",
        "strip_preserves_pair_atomicity",
        "strip_preserves_message_order",
        "strip_is_strictly_idempotent",
        "reduction_blocked_when_header_loading",
        "reduction_allowed_for_stable_published_prefix",
        "no_orphaned_tool_results_after_strip",
        "reapply_preserves_view_coherent",
        "summarize_retains_straddling_turn",
        "summarize_drops_whole_turns",
        "summarize_oversized_complete_turn",
        "summarize_blocked_when_header_loading",
        "summarize_cannot_split_a_leading_turn",
        "provider_view_is_idempotent",
        "provider_view_drops_orphaned_result",
        "closed_turn_allows_projection_after_unresolved_history",
        "open_raw_tail_blocks_projected_prefix",
        "unpublished_closed_turn_blocks_projected_prefix",
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
    generated_compaction_cursor_cases_pin_contract();
}

fn generated_compaction_cursor_cases_pin_contract() {
    let cases = lean_compaction_cursor_cases();
    assert_eq!(cases.len(), 3);
    let expected_names = [
        "cursor_skips_orphan_and_compacted_turn",
        "cursor_rejects_split_inside_tool_pair",
        "cursor_accepts_complete_tool_pair",
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

    let rows = vec![
        (10, compaction_tool_result_message("orphan", "discard me")),
        (20, compaction_text_message("user", "compacted")),
        (40, compaction_tool_call_message("call-1")),
        (50, compaction_tool_result_message("call-1", "result")),
        (90, compaction_text_message("assistant", "active")),
    ];
    for case in cases {
        let expected_sequence = case.expected_cursor.map(|split| rows[split - 1].0);
        let actual =
            gents::compaction::compacted_through_sequence(GROUPED_PROFILE, &rows, case.compacted);
        assert_eq!(actual, expected_sequence, "{}: canonical cursor", case.name);

        if let Some(cursor) = actual {
            let (full, _) = gents::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter().map(|(_, message)| message.clone()).collect(),
            );
            let (filtered, _) = gents::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter()
                    .filter(|(sequence, _)| *sequence > cursor)
                    .map(|(_, message)| message.clone())
                    .collect(),
            );
            let (prefix, _) = gents::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter()
                    .filter(|(sequence, _)| *sequence <= cursor)
                    .map(|(_, message)| message.clone())
                    .collect(),
            );
            assert_eq!(prefix.len(), case.compacted, "{}: cursor prefix", case.name);
            assert_eq!(
                prefix.len() + filtered.len(),
                full.len(),
                "{}: cursor partitions the canonical view",
                case.name
            );
        }
    }
}

fn drive_compaction_reducer_case(case: &lean_vocab_test::LeanCompactionReducerCase) {
    assert!(case.legal, "compaction case {} should be legal", case.name);

    let input = compaction_messages_for_case(case);
    assert_eq!(
        gents::compaction::session_prefix_is_stable(&input) && case.publication_ready,
        case.projected_prefix_ready,
        "{}: session compaction uses the modeled published provider-prefix boundary",
        case.name,
    );
    assert_eq!(
        input.len(),
        case.pre_message_count,
        "{}: pre_message_count",
        case.name
    );

    if case.reducer == "summarize" {
        check_summarize_gate_and_split(case, input);
        return;
    }

    let reduced = apply_compaction_reducer(case, input.clone());
    assert_eq!(
        reduced.len(),
        case.post_message_count,
        "{}: post_message_count",
        case.name
    );
    assert_eq!(
        reduced.len(),
        case.retained_count,
        "{}: retained_count",
        case.name
    );
    assert_eq!(
        preserves_pair_closure(&input, &reduced),
        case.preserves_pairs,
        "{}: preserves_pairs",
        case.name
    );
    assert_eq!(
        is_subsequence(
            &abstract_prompt_view(&reduced),
            &abstract_prompt_view(&input)
        ),
        case.preserves_order,
        "{}: preserves_order",
        case.name
    );

    assert_eq!(
        reduced == input,
        case.reducer_is_identity,
        "{}: identity must compare full runtime payloads",
        case.name
    );
    let reapplied = apply_compaction_reducer(case, reduced.clone());
    assert_eq!(
        reapplied == reduced,
        case.reducer_is_idempotent,
        "{}: reapplication must compare full runtime payloads",
        case.name
    );

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
        "strip" => gents::compaction::strip_tool_results(input).0,
        "provider_view" => gents::compaction::provider_view(GROUPED_PROFILE, input).0,
        other => panic!("unsupported compaction reducer {other:?} for {}", case.name),
    }
}

fn check_summarize_gate_and_split(
    case: &lean_vocab_test::LeanCompactionReducerCase,
    input: Vec<Message>,
) {
    let production_gate_open = gents::compaction::safe_to_reduce(&input);
    assert_eq!(
        production_gate_open,
        case.turn_boundary && case.provider_fixpoint,
        "{}: production safe_to_reduce must agree with the modelled \
         turn-boundary and sanitizer-fixpoint gate components",
        case.name
    );
    let native_order = with_leading_reasoning(&input);
    assert_eq!(
        gents::compaction::history::normalize_assistant_content_order(
            GROUPED_PROFILE,
            native_order.clone()
        ) == native_order,
        native_order.iter().all(|message| !matches!(
            message,
            Message::Assistant { content, .. }
                if content.iter().any(|item| matches!(item, AssistantContent::Text(_)))
        )),
        "{}: native-order fixture differs from its provider order exactly when it has assistant text",
        case.name
    );
    assert_eq!(
        gents::compaction::safe_to_reduce(&native_order),
        case.turn_boundary && case.provider_fixpoint,
        "{}: gate on native-order rows",
        case.name
    );
    assert_eq!(
        production_gate_open && case.publication_ready,
        case.safe_to_reduce,
        "{}: structural checks plus modeled publication readiness",
        case.name
    );

    let boundary = gents::compaction::pair_safe_boundary(&input, case.split_index);
    assert_eq!(
        boundary, case.safe_boundary,
        "{}: production pair_safe_boundary must match Compaction.pairSafeBoundary",
        case.name
    );
    if pair_closed(&input) {
        for budget in 0usize.. {
            let (old, recent) = gents::compaction::split_for_summary(
                input.clone(),
                budget,
                gents::BackendProviderKind::OpenAiCompatible,
                gents::OpenAiWireApi::ChatCompletions,
                "conformance-model",
            )
            .expect("provider-shaped split");
            assert!(
                pair_closed(&recent),
                "{}: split_for_summary orphaned a tool result at budget {budget}",
                case.name
            );
            if old.is_empty() {
                break;
            }
        }
    }

    assert_eq!(
        case.gate_open
            .expect("summarize cases carry a modelled gate"),
        boundary > 0,
        "{}: the modelled reducer gate must agree with the production boundary",
        case.name
    );
}

fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut cursor = haystack.iter();
    needle
        .iter()
        .all(|item| cursor.any(|candidate| candidate == item))
}

fn compaction_messages_for_case(case: &lean_vocab_test::LeanCompactionReducerCase) -> Vec<Message> {
    case.input_rows
        .iter()
        .map(|row| match row {
            lean_vocab_test::LeanCompactionInputRow::Ordinary { role, text } => {
                compaction_text_message(role, text)
            }
            lean_vocab_test::LeanCompactionInputRow::AssistantToolCalls { role, call_ids } => {
                assert_eq!(role, "assistant");
                Message::Assistant {
                    request_id: None,
                    id: None,
                    content: call_ids
                        .iter()
                        .flat_map(|call_id| match compaction_tool_call_message(call_id) {
                            Message::Assistant { content, .. } => content,
                            _ => unreachable!(),
                        })
                        .collect(),
                }
            }
            lean_vocab_test::LeanCompactionInputRow::ToolResult { role, call_id } => {
                assert_eq!(role, "user");
                compaction_tool_result_message(call_id, "large tool payload")
            }
        })
        .collect()
}

pub(super) fn compaction_messages_for_count(count: usize) -> Vec<Message> {
    match count {
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
        4 => vec![
            compaction_text_message("user", "first"),
            compaction_tool_call_message("call-1"),
            compaction_tool_result_message("call-1", "large tool payload"),
            compaction_text_message("assistant", "reply"),
        ],
        other => panic!("unsupported compaction message_count {other}"),
    }
}

fn with_leading_reasoning(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .cloned()
        .map(|message| match message {
            Message::Assistant {
                id, mut content, ..
            } if content
                .iter()
                .any(|item| matches!(item, AssistantContent::Text(_))) =>
            {
                content.insert(
                    0,
                    AssistantContent::Reasoning(gents::llm::message::Reasoning::new("thinking")),
                );
                Message::Assistant {
                    request_id: None,
                    id,
                    content,
                }
            }
            other => other,
        })
        .collect()
}

fn compaction_text_message(role: &str, text: &str) -> Message {
    match role {
        "user" => Message::User {
            content: vec![UserContent::Text(Text {
                text: text.to_string(),
            })],
        },
        "assistant" => Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::Text(Text {
                text: text.to_string(),
            })],
        },
        other => panic!("unsupported compaction text role {other:?}"),
    }
}

fn compaction_tool_call_message(call_id: &str) -> Message {
    Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: json!({ "file_path": "/tmp/compaction-contract.txt" }),
            },
            signature: None,
            additional_params: None,
        })],
    }
}

fn compaction_tool_result_message(call_id: &str, payload: &str) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: payload.to_string(),
            })],
        })],
    }
}

fn preserves_pair_closure(pre: &[Message], post: &[Message]) -> bool {
    !pair_closed(pre) || pair_closed(post)
}

fn pair_closed(messages: &[Message]) -> bool {
    let call_ids = messages
        .iter()
        .flat_map(assistant_tool_call_ids)
        .collect::<RapidHashSet<_>>();
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
