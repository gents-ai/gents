// Owned-loop request assembly, repair, compaction, and dispatch-boundary tests.
#[tokio::test]
async fn loop_entry_sanitizes_a_recovered_checkpoint_as_one_projection() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("continued".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let call = Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(crate::llm::message::ToolCall {
            id: "restored-call".to_string(),
            call_id: Some("restored-call".to_string()),
            function: crate::llm::message::ToolFunction {
                name: "read".to_string(),
                arguments: serde_json::json!({}),
            },
            signature: None,
            additional_params: None,
        })],
    };
    let result = Message::User {
        content: vec![UserContent::ToolResult(crate::llm::message::ToolResult {
            id: "restored-call".to_string(),
            call_id: Some("restored-call".to_string()),
            content: vec![ToolResultContent::text("restored result")],
        })],
    };

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        result,
        vec![call],
        Arc::new(Vec::new()),
        config(0),
    ))
    .await;

    assert_eq!(collected.error, None);
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert!(history_has_tool_call(&histories[0], "read"));
    assert!(history_has_tool_result_text(
        &histories[0],
        "restored result"
    ));
}

#[tokio::test]
async fn context_message_is_sent_before_prompt() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("ok".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut cfg = config(0);
    cfg.context_message = Some(Message::user(
        "<context>\nnow=2026-06-15T00:00:00Z\n</context>",
    ));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("actual prompt"),
        Vec::new(),
        Arc::new(Vec::new()),
        cfg,
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert!(matches!(
        &histories[0][0],
        Message::User { content }
            if matches!(first_content(content), UserContent::Text(text) if text.text.starts_with("<context>"))
    ));
}
/// Provider-request invariants: what every completion request the loop emits
/// must satisfy, independent of the scenario that produced it. Mirrors the
/// provider-side contract that `sanitize_history_for_provider` enforces for
/// loaded history — the loop must satisfy it by construction for the messages
/// it threads itself.
fn assert_provider_request_invariants(turn: usize, history: &[Message]) {
    let mut pending_call_keys: Vec<String> = Vec::new();
    for message in history {
        match message {
            Message::Assistant { content, .. } => {
                assert!(
                    pending_call_keys.is_empty(),
                    "turn {turn}: assistant message before prior turn's tool calls were resolved"
                );
                // Ordering: no text or reasoning after a tool call.
                let mut seen_tool_call = false;
                for item in content.iter() {
                    match item {
                        AssistantContent::ToolCall(tool_call) => {
                            seen_tool_call = true;
                            pending_call_keys.push(
                                tool_call
                                    .call_id
                                    .clone()
                                    .unwrap_or_else(|| tool_call.id.clone()),
                            );
                        }
                        _ => assert!(
                            !seen_tool_call,
                            "turn {turn}: assistant content after a tool call (providers reject)"
                        ),
                    }
                }
            }
            Message::User { content } => {
                let has_tool_results = content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_)));
                assert!(
                    has_tool_results || pending_call_keys.is_empty(),
                    "turn {turn}: ordinary user content before prior tool calls were resolved"
                );
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        let key = tool_result
                            .call_id
                            .clone()
                            .unwrap_or_else(|| tool_result.id.clone());
                        let position = pending_call_keys.iter().position(|call| call == &key);
                        assert!(
                            position.is_some(),
                            "turn {turn}: tool result '{key}' without a preceding tool call"
                        );
                        pending_call_keys.remove(position.unwrap());
                    }
                }
            }
            Message::System { .. } => assert!(
                pending_call_keys.is_empty(),
                "turn {turn}: system message before prior tool calls were resolved"
            ),
        }
    }
    assert!(
        pending_call_keys.is_empty(),
        "turn {turn}: unpaired tool calls reached the provider: {pending_call_keys:?}"
    );
}

#[tokio::test]
async fn every_request_in_a_tool_loop_satisfies_provider_invariants() {
    // Conformance guard for the loop's own threading: across a multi-tool,
    // multi-turn run, every request's history must pair calls with results and
    // keep assistant content provider-ordered — by construction, no sanitizer.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let model = ScriptedModel::new_turns(vec![
        // Turn 1: text + reasoning + two tool calls in one assistant turn.
        vec![
            RawStreamingChoice::Message("let me check".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-2".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        // Turn 2: one more tool call.
        echo_tool_turn(),
        // Turn 3: final text.
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("run the tools"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "expected three completion turns; got {histories:?}"
    );
    for (index, history) in histories.iter().enumerate() {
        assert_provider_request_invariants(index + 1, history);
    }
}

#[tokio::test]
async fn dirty_caller_history_is_sanitized_at_loop_entry() {
    // Chokepoint guarantee: EVERY owned-loop consumer (daemon, oneshot,
    // compaction summarize, title, subagent children) sends provider-valid
    // history because the loop sanitizes the caller-provided history at entry
    // — no call site can forget the sanitizer. Feed a dirty history (unpaired
    // call, orphaned result, text-after-call ordering) and assert the request
    // on the wire satisfies the provider invariants.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let unpaired_call = crate::llm::message::ToolCall {
        id: "call-unpaired".to_string(),
        call_id: Some("call-unpaired".to_string()),
        function: crate::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let paired_call = crate::llm::message::ToolCall {
        id: "call-paired".to_string(),
        call_id: Some("call-paired".to_string()),
        function: crate::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let dirty_history = vec![
        // Orphaned result: its call was compacted away.
        Message::User {
            content: vec![UserContent::tool_result(
                "call-gone".to_string(),
                vec![crate::llm::message::ToolResultContent::text("orphaned")],
            )],
        },
        // Misordered assistant turn (text AFTER calls) with one unpaired call.
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::ToolCall(paired_call),
                AssistantContent::ToolCall(unpaired_call),
                AssistantContent::Text(crate::llm::message::Text {
                    text: "stale ordering".to_string(),
                }),
            ],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                "call-paired".to_string(),
                vec![crate::llm::message::ToolResultContent::text("ok")],
            )],
        },
    ];

    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("hi".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("continue"),
        dirty_history,
        Arc::new(Vec::new()),
        config(1),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert_provider_request_invariants(1, &histories[0]);
    // The unpaired call is gone but the paired exchange survives.
    let kept_calls: Vec<String> = histories[0]
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        kept_calls,
        vec!["call-paired".to_string()],
        "unpaired call dropped, paired call kept; history: {:?}",
        histories[0]
    );
}

#[test]
fn assembles_context_immediately_before_prompt() {
    // Fences Lean `PromptAssembly.Template.assembleWithContext_tail`: when a
    // per-request context message is present, the assembly ends with exactly
    // [contextPreamble, prompt] — context immediately precedes the prompt.
    let context = Message::user("<context>\nseat: x\n</context>");
    let prompt = Message::user("hello");

    let with_context = super::assemble_new_messages(Some(context.clone()), prompt.clone());
    assert_eq!(with_context.len(), 2);
    assert!(super::is_request_context_message(&with_context[0]));
    assert_eq!(with_context[1], prompt);
    // Context is the immediately-preceding entry before the prompt.
    assert_eq!(&with_context[with_context.len() - 2], &context);

    // Without a context message, the prompt is the sole (last) entry.
    let without = super::assemble_new_messages(None, prompt.clone());
    assert_eq!(without, vec![prompt]);
}

#[test]
fn is_request_context_message_only_matches_context_user_text() {
    assert!(super::is_request_context_message(&Message::user(
        "<context>\nx\n</context>"
    )));
    assert!(!super::is_request_context_message(&Message::user(
        "an ordinary prompt"
    )));
    assert!(!super::is_request_context_message(&Message::assistant(
        "hi"
    )));
}

/// A salvageable corrupt-arguments payload must run the intended typed tool,
/// persist object-shaped arguments, and re-enter the provider as an object.
#[tokio::test]
async fn corrupt_589_tool_args_salvage_runs_and_history_stays_object_shaped() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct DescribeTool;
    #[derive(Debug, thiserror::Error)]
    #[error("describe tool error")]
    struct DescribeToolError;
    #[derive(serde::Deserialize)]
    struct DescribeArgs {
        tool_name: String,
    }
    impl Tool for DescribeTool {
        const NAME: &'static str = "describe_tool";
        type Error = DescribeToolError;
        type Args = DescribeArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(format!("described:{}", args.tool_name))
        }
    }

    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    // The wire parser could not parse the corrupt bytes, so rig carries them as
    // a raw Value::String — exactly the shape persisted in the production store.
    let poison = serde_json::Value::String(crate::test_support::CORRUPT_TOOL_ARGS_589.to_string());
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                poison,
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(DescribeTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("describe list_hosts"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut tool_results = Vec::new();
    while let Some(item) = stream.next().await {
        if let LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
            StreamedUserContent::ToolResult { tool_result, .. },
        )) = item.expect("loop item should be Ok")
        {
            tool_results.push(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .to_string(),
            );
        }
    }

    // (a) The intended call ran: salvage recovered `tool_name: list_hosts`.
    assert_eq!(
        tool_results,
        vec!["described:list_hosts".to_string()],
        "the salvageable #589 payload must run the intended call, not waste a turn"
    );

    // (b) The next provider request carries object-shaped arguments. (The
    // durable AgentMessage fence lives in the StreamProcessor harness —
    // `stream_processor::tests::corrupt_tool_call_arguments_persist_object_shaped`
    // — since the bare generator does not persist assistant turns.)
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
    drop(node);
}

/// Valid JSON that is not an object must fail
/// `argumentInvalid` with the model notified (never run), and neither the
/// durable history nor the next provider request may carry the non-object.
#[tokio::test]
async fn nonobject_tool_args_never_reach_durable_history_or_provider() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct StrictTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        tool_name: String,
    }
    impl Tool for StrictTool {
        const NAME: &'static str = "describe_tool";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            panic!("the tool must not run on non-object arguments");
        }
    }

    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                serde_json::json!([]),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("describe"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);
    while let Some(item) = stream.next().await {
        item.expect("loop must not fail; non-object args are notified, not raised");
    }

    // The started call terminalized failed/argumentInvalid — never a live
    // completed call carrying poison (#589's persist gate).
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }")
        .await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|v| v.as_str()) == Some("describe_tool")
                && row.get("lifecycle_state").and_then(|v| v.as_str()) == Some("failed")
                && row.get("tool_failure_class").and_then(|v| v.as_str()) == Some("argumentInvalid")
        }),
        "a non-object-args call must terminalize failed/argumentInvalid, got: {rows:?}"
    );

    // The next provider request carries object-shaped args — the [] never
    // re-egresses.
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
}

/// Every tool call inside a (native) history must carry object-shaped
/// arguments — the provider-render precondition (#590).
fn assert_all_history_tool_args_object_shaped(history: &[Message]) {
    for message in history {
        if let Message::Assistant { content, .. } = message {
            for item in content {
                if let AssistantContent::ToolCall(tool_call) = item {
                    assert!(
                        tool_call.function.arguments.is_object(),
                        "non-object tool-call arguments reached the provider: {:?}",
                        tool_call.function.arguments
                    );
                }
            }
        }
    }
}

/// The repair pass must sanitize loaded history as well as run-threaded
/// messages.
#[tokio::test(start_paused = true)]
async fn repair_sanitizes_poisoned_tool_args_in_loaded_history() {
    let poison = format!("bad{}value", '\u{0007}');

    // The poison lives ONLY in the loaded conversation history — a prior turn's
    // persisted tool call. The current run produces nothing dirty.
    let poisoned_call = crate::llm::message::ToolCall {
        id: "call-historic".to_string(),
        call_id: Some("call-historic".to_string()),
        function: crate::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({ "note": poison }),
        },
        signature: None,
        additional_params: None,
    };
    let history = vec![
        Message::user("earlier"),
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(poisoned_call)],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                "call-historic",
                vec![ToolResultContent::text("ok")],
            )],
        },
    ];
    assert!(
        history_has_control_char_tool_arg(&history),
        "the fixture must actually carry poisoned history"
    );

    // Two identical parse-400s: resample once, then repair.
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        history,
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("repaired"));
    assert_eq!(collected.error, None);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "parse failure, resample, and the repaired retry"
    );
    assert!(
        history_has_control_char_tool_arg(&histories[0]),
        "the poisoned history must reach the provider before repair: {:?}",
        histories[0]
    );
    assert!(
        !history_has_control_char_tool_arg(&histories[2]),
        "repair must sanitize tool arguments in the LOADED HISTORY, not only \
         the run-threaded messages — otherwise it re-issues the same poisoned \
         input and fails identically (#652): {:?}",
        histories[2]
    );
}

// ---------------------------------------------------------------------------
// Generated PromptAssembly contract consumers.
//
// These live in the crate rather than in `tests/conformance/prompt_assembly.rs`
// because they drive `pub(crate)` production entry points: `assemble_new_messages`
// and `repair_provider_input`. The sanitize family, whose entry point is public,
// is fenced from the integration test.
// ---------------------------------------------------------------------------

/// Text of a single-item user text message, for slot classification.
fn sole_user_text(message: &Message) -> String {
    match message {
        Message::User { content } => match content.as_slice() {
            [UserContent::Text(text)] => text.text.clone(),
            other => panic!("layer fence built an unexpected user message: {other:?}"),
        },
        other => panic!("layer fence built an unexpected message: {other:?}"),
    }
}

/// Name the `PromptAssembly.Slot` a message occupies.
fn classify_slot(message: &Message, is_last: bool, conversation_index: &mut usize) -> String {
    if super::is_request_context_message(message) {
        return "contextPreamble".to_string();
    }
    if is_last {
        return "prompt".to_string();
    }
    let text = sole_user_text(message);
    if let Some(body) = text.strip_prefix("<system-reminder>\n") {
        if body.starts_with("Continuation checkpoints from earlier conversation") {
            return "summaryReminder".to_string();
        }
        if let Some(rest) = body.strip_prefix("skill-") {
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            return format!("skillReminder:{digits}");
        }
    }
    let slot = format!("conversation:{conversation_index}");
    *conversation_index += 1;
    slot
}

/// Fences the fixed layer order of the assembled request against Lean
/// `PromptAssembly.Template.assembleWithContext`, whose `assembleWithContext_tail`
/// theorem pins the tail as `[contextPreamble, prompt]`.
///
/// The summary/conversation layers come from the production
/// `LayeredPromptBuilder::build`, and the tail from the production
/// `assemble_new_messages`. The skill-reminder prepend is *mirrored* from
/// `agent/daemon/request.rs` rather than driven, because it happens inline in
/// that function's async request flow; the reminders themselves are built by the
/// production `LayeredPromptBuilder::system_reminder`.
#[tokio::test]
async fn generated_layer_cases_pin_the_assembled_request_order() {
    use crate::lean_vocab_test::lean_prompt_assembly_layer_cases;
    use crate::prompt::{LayeredPromptBuilder, PromptBuilder};

    let cases = lean_prompt_assembly_layer_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly layer cases"
    );

    for case in cases {
        let builder = LayeredPromptBuilder::for_behavior(
            "system prompt",
            "fence",
            &["bash"],
            false,
            &[],
        );

        let conversation = (0..case.conversation_len)
            .map(|index| Message::user(format!("conversation-{index}")))
            .collect::<Vec<_>>();
        let summaries = (0..case.summary_count)
            .map(|index| format!("summary-{index}"))
            .collect::<Vec<_>>();
        let skill_reminders = (0..case.skill_count)
            .map(|index| LayeredPromptBuilder::system_reminder(&format!("skill-{index}")))
            .collect::<Vec<_>>();

        let built = builder
            .build(&conversation, &summaries)
            .await
            .expect("build layered prompt");

        let mut assembled = skill_reminders;
        assembled.extend(built.messages);
        assembled.extend(super::assemble_new_messages(
            Some(Message::user("<context>\nnow: t\n</context>")),
            Message::user("prompt"),
        ));

        // The preamble is a field on the completion request, not a message.
        assert!(
            !builder.preamble().is_empty(),
            "the preamble slot must be carried by the system-prompt field"
        );
        let mut slots = vec!["preamble".to_string()];
        let mut conversation_index = 0usize;
        let assembled_len = assembled.len();
        for (position, message) in assembled.iter().enumerate() {
            slots.push(classify_slot(
                message,
                position + 1 == assembled_len,
                &mut conversation_index,
            ));
        }

        assert_eq!(
            slots, case.slots,
            "assembled layer order drifted from the Lean model on case {:?}",
            case.name
        );
    }
}

/// Concrete tool-call arguments denoting each abstract `PromptAssembly.ToolArgs`
/// shape the contract emits.
fn repair_vector(name: &str) -> serde_json::Value {
    match name {
        // A `raw` payload is one whose string leaves still carry literal
        // newlines — exactly what the leaf sanitizer rewrites.
        "object:raw" => serde_json::json!({"k": "line\nbreak"}),
        "object:empty" => serde_json::json!({}),
        "object:sanitized" => serde_json::json!({"k": "no break"}),
        "str:object:raw" => serde_json::Value::String("{\"k\": \"line\\nbreak\"}".to_string()),
        "str:unparsed" => serde_json::Value::String("not json at all".to_string()),
        "array" => serde_json::json!([1, 2]),
        "scalar" => serde_json::json!(123),
        "null" => serde_json::Value::Null,
        other => panic!("generated repair case names an unmodeled shape: {other}"),
    }
}

/// Project repaired arguments back onto the abstract shape, mirroring the
/// `Payload` abstraction in the contract: `empty` is `{}`, `raw` still carries a
/// literal newline in some string leaf, `sanitized` does not.
fn repair_shape(value: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = value else {
        panic!("repair must always yield an object, got {value:?}");
    };
    if map.is_empty() {
        return "object:empty".to_string();
    }
    if has_raw_leaf(value) {
        "object:raw".to_string()
    } else {
        "object:sanitized".to_string()
    }
}

fn has_raw_leaf(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains('\n'),
        serde_json::Value::Array(values) => values.iter().any(has_raw_leaf),
        serde_json::Value::Object(map) => map.values().any(has_raw_leaf),
        _ => false,
    }
}

fn repaired_arguments(arguments: serde_json::Value) -> serde_json::Value {
    // `repair_provider_input` re-sanitizes after rewriting arguments, so the
    // call must be paired or the whole turn is (correctly) dropped.
    let mut history = vec![
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(crate::llm::message::ToolCall {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: crate::llm::message::ToolFunction {
                    name: "echo".to_string(),
                    arguments,
                },
                signature: None,
                additional_params: None,
            })],
        },
        Message::User {
            content: vec![UserContent::ToolResult(crate::llm::message::ToolResult {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                content: vec![ToolResultContent::Text(crate::llm::message::Text {
                    text: "call-1-result".to_string(),
                })],
            })],
        },
    ];
    let mut new_messages = Vec::new();
    super::repair_provider_input(&mut history, &mut new_messages);
    let [Message::Assistant { content, .. }, Message::User { .. }] = history.as_slice() else {
        panic!("repair must rewrite payloads only, never rows: {history:?}");
    };
    let [AssistantContent::ToolCall(tool_call)] = content.as_slice() else {
        panic!("repair dropped the tool call: {content:?}");
    };
    tool_call.function.arguments.clone()
}

/// Fences Lean `PromptAssembly.repairArgs` — `repair_is_payload_only` (repair
/// rewrites argument payloads only, never rows, roles, call ids, or ordering)
/// and `repair_idempotent` (a second pass is a no-op).
#[test]
fn generated_repair_cases_drive_tool_argument_repair() {
    use crate::lean_vocab_test::lean_prompt_assembly_repair_cases;

    let cases = lean_prompt_assembly_repair_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly repair cases"
    );

    for case in cases {
        let input = repair_vector(&case.input);
        let once = repaired_arguments(input.clone());
        assert_eq!(
            repair_shape(&once),
            case.expected,
            "repair disagrees with the Lean model on case {:?}",
            case.name
        );

        let twice = repaired_arguments(once.clone());
        assert_eq!(
            repair_shape(&twice),
            case.expected_twice,
            "repair is not idempotent on case {:?}",
            case.name
        );
        assert_eq!(
            twice, once,
            "repair_idempotent: a second pass must not change the payload ({:?})",
            case.name
        );

        // `repair_is_payload_only`: object inputs keep their shape, and repair
        // never rewrites anything outside the payload.
        if case.payload_only {
            assert!(
                input.is_object(),
                "the contract marks {:?} payload-only, so its input must be an object",
                case.name
            );
        }
    }
}
