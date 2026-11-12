use super::*;

const GROUPED_PROFILE: gents_loop::provider_input::ProviderInputProfile =
    gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions;

pub(super) fn compaction_reducers_follow_the_lean_model() {
    for case in gents_lean_vocab::families::contracts_compaction_reducer_gate::cases::<String>() {
        let label = format!("{:?}", case.input);
        let input = compaction_messages_for_rows(&case.input.input_rows);
        let structurally_safe = gents_inference::compaction::safe_to_reduce(&input);
        let native_order = with_leading_reasoning(&input);
        assert_eq!(
            gents_inference::compaction::history::normalize_assistant_content_order(
                GROUPED_PROFILE,
                native_order.clone()
            ) == native_order,
            native_order.iter().all(|message| !matches!(
                message,
                Message::Assistant { content, .. }
                    if content.iter().any(|item| matches!(item, AssistantContent::Text(_)))
            )),
            "{label}: native-order fixture differs from its provider order exactly when it has assistant text"
        );
        assert_eq!(
            gents_inference::compaction::safe_to_reduce(&native_order),
            structurally_safe,
            "{label}: gate on native-order rows"
        );
        if pair_closed(&input) {
            for budget in 0usize.. {
                let (old, recent) = gents_inference::compaction::split_for_summary(
                    input.clone(),
                    budget,
                    gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
                    gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
                    "conformance-model",
                )
                .expect("provider-shaped split");
                assert!(
                    pair_closed(&recent),
                    "{label}: split_for_summary orphaned a tool result at budget {budget}"
                );
                if old.is_empty() {
                    break;
                }
            }
        }
        let turn_boundary = gents_inference::compaction::session_prefix_is_stable(&input);
        case.check(
            gents_lean_vocab::families::contracts_compaction_reducer_gate::CompactionReducerGateOutput {
                projected_prefix_ready: turn_boundary && case.input.publication_ready,
                pre_message_count: input.len() as u64,
                turn_boundary,
                provider_fixpoint: gents_inference::compaction::sanitize_association_with_sources(
                    input.clone(),
                )
                .into_iter()
                .map(|sourced| sourced.message)
                .eq(input.iter().cloned()),
                safe_to_reduce: structurally_safe && case.input.publication_ready,
                safe_boundary: gents_inference::compaction::pair_safe_boundary(
                    &input,
                    usize::try_from(case.input.split_index).unwrap(),
                ) as u64,
            },
        );
    }
    for case in gents_lean_vocab::families::contracts_compaction_reducer_gate_open::cases::<String>() {
        let input = compaction_messages_for_rows(&gents_lean_vocab::family::recode::<
            _,
            Vec<gents_lean_vocab::families::contracts_compaction_reducer_gate::CompactionRowWire>,
        >(&case.input.input_rows));
        case.check(match case.input.reducer.as_str() {
            "summarize" => Some(
                gents_inference::compaction::pair_safe_boundary(
                    &input,
                    usize::try_from(case.input.split_index).unwrap(),
                ) > 0,
            ),
            "strip" | "provider_view" => None,
            other => panic!("no production compaction reducer {other:?}"),
        });
    }
    for case in gents_lean_vocab::families::contracts_compaction_reducer_apply::cases::<String>() {
        let label = format!("{:?}", case.input);
        let input = compaction_messages_for_rows(&gents_lean_vocab::family::recode::<
            _,
            Vec<gents_lean_vocab::families::contracts_compaction_reducer_gate::CompactionRowWire>,
        >(&case.input.input_rows));
        let reduced = apply_compaction_reducer(&case.input.reducer, input.clone());
        let reapplied = apply_compaction_reducer(&case.input.reducer, reduced.clone());
        if case.input.name == "reapply_preserves_view_coherent" {
            assert!(
                pair_closed(&reapplied),
                "{label}: reapply preserves pair closure"
            );
            assert_eq!(
                abstract_prompt_view(&reduced),
                abstract_prompt_view(&reapplied),
                "{label}: reapply preserves ordering projection"
            );
        }
        case.check(
            gents_lean_vocab::families::contracts_compaction_reducer_apply::CompactionReducerApplyOutput {
                post_message_count: reduced.len() as u64,
                retained_count: reduced.len() as u64,
                preserves_pairs: preserves_pair_closure(&input, &reduced),
                preserves_order: is_subsequence(
                    &abstract_prompt_view(&reduced),
                    &abstract_prompt_view(&input),
                ),
                reducer_is_identity: reduced == input,
                reducer_is_idempotent: reapplied == reduced,
            },
        );
    }
    compaction_cursor_follows_the_lean_model();
}

fn compaction_cursor_follows_the_lean_model() {
    let rows = vec![
        (10, compaction_tool_result_message("orphan", "discard me")),
        (20, compaction_text_message("user", "compacted")),
        (40, compaction_tool_call_message("call-1")),
        (50, compaction_tool_result_message("call-1", "result")),
        (90, compaction_text_message("assistant", "active")),
    ];
    for case in gents_lean_vocab::families::contracts_compaction_cursor::cases() {
        let compacted = usize::try_from(case.input.compacted).unwrap();
        let actual = gents_inference::compaction::compacted_through_sequence(
            GROUPED_PROFILE,
            &rows,
            compacted,
        );
        if let Some(cursor) = actual {
            let (full, _) = gents_inference::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter().map(|(_, message)| message.clone()).collect(),
            );
            let (filtered, _) = gents_inference::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter()
                    .filter(|(sequence, _)| *sequence > cursor)
                    .map(|(_, message)| message.clone())
                    .collect(),
            );
            let (prefix, _) = gents_inference::compaction::provider_view(
                GROUPED_PROFILE,
                rows.iter()
                    .filter(|(sequence, _)| *sequence <= cursor)
                    .map(|(_, message)| message.clone())
                    .collect(),
            );
            assert_eq!(prefix.len(), compacted, "{:?}: cursor prefix", case.input);
            assert_eq!(
                prefix.len() + filtered.len(),
                full.len(),
                "{:?}: cursor partitions the canonical view",
                case.input
            );
        }
        case.check(actual.map(|sequence| {
            rows.iter()
                .position(|(row, _)| *row == sequence)
                .expect("the cursor names a fixture row") as u64
                + 1
        }));
    }
}

fn apply_compaction_reducer(reducer: &str, input: Vec<Message>) -> Vec<Message> {
    match reducer {
        "identity" => input,
        "strip" => gents_inference::compaction::strip_tool_results(input).0,
        "provider_view" => gents_inference::compaction::provider_view(GROUPED_PROFILE, input).0,
        other => panic!("unsupported compaction reducer {other:?}"),
    }
}

fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut cursor = haystack.iter();
    needle
        .iter()
        .all(|item| cursor.any(|candidate| candidate == item))
}

fn compaction_messages_for_rows(
    rows: &[gents_lean_vocab::families::contracts_compaction_reducer_gate::CompactionRowWire],
) -> Vec<Message> {
    rows.iter()
        .map(|row| match row.kind.as_str() {
            "ordinary" => compaction_text_message(&row.role, row.text.as_deref().unwrap()),
            "assistant_tool_calls" => {
                assert_eq!(row.role, "assistant");
                Message::Assistant {
                    request_id: None,
                    id: None,
                    content: row
                        .call_ids
                        .iter()
                        .flat_map(|call_id| match compaction_tool_call_message(call_id) {
                            Message::Assistant { content, .. } => content,
                            _ => unreachable!(),
                        })
                        .collect(),
                }
            }
            "tool_result" => {
                assert_eq!(row.role, "user");
                compaction_tool_result_message(
                    row.call_id.as_deref().unwrap(),
                    "large tool payload",
                )
            }
            other => panic!("unsupported compaction row kind {other:?}"),
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
                    AssistantContent::Reasoning(gents_inference::llm::message::Reasoning::new(
                        "thinking",
                    )),
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
