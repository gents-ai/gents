use super::*;
pub fn assert_provider_request_invariants(turn: usize, history: &[Message]) {
    let mut pending_call_keys: Vec<String> = Vec::new();
    for message in history {
        match message {
            Message::Assistant { content, .. } => {
                assert!(
                    pending_call_keys.is_empty(),
                    "turn {turn}: assistant message before prior turn's tool calls were resolved"
                );
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

pub fn assert_all_history_tool_args_object_shaped(history: &[Message]) {
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

pub fn sole_user_text(message: &Message) -> String {
    match message {
        Message::User { content } => match content.as_slice() {
            [UserContent::Text(text)] => text.text.clone(),
            other => panic!("layer fence built an unexpected user message: {other:?}"),
        },
        other => panic!("layer fence built an unexpected message: {other:?}"),
    }
}

pub fn classify_slot(message: &Message, is_last: bool, conversation_index: &mut usize) -> String {
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

fn observed_authored_key(
    message_key: &str,
    head_doc_id: &str,
    docs: &rapidhash::RapidHashMap<u64, String>,
) -> gents_lean_vocab::families::fold_queue_contracts_turn_input::AuthoredKeyWire<String> {
    let key =
        |authored: &str| gents_protocol::output::rows::authored_message_key(head_doc_id, authored);
    if message_key == key("context") {
        return gents_lean_vocab::families::fold_queue_contracts_turn_input::AuthoredKeyWire {
            kind: "context".to_owned(),
            request_id: None,
        };
    }
    if message_key == key("prompt") {
        return gents_lean_vocab::families::fold_queue_contracts_turn_input::AuthoredKeyWire {
            kind: "prompt".to_owned(),
            request_id: None,
        };
    }
    let request_id = docs
        .iter()
        .find(|(_, doc)| {
            message_key
                == key(&gents_session::request::lifecycle::queue::folded_input_key(
                    doc,
                ))
        })
        .map(|(request_id, _)| *request_id)
        .unwrap_or_else(|| panic!("authored message key {message_key} names no turn input"));
    gents_lean_vocab::families::fold_queue_contracts_turn_input::AuthoredKeyWire {
        kind: "folded".to_owned(),
        request_id: Some(request_id),
    }
}

fn trailing_user_texts(messages: &[Message]) -> Vec<String> {
    let start = messages
        .iter()
        .rposition(|message| !matches!(message, Message::User { .. }))
        .map_or(0, |index| index + 1);
    messages[start..].iter().map(sole_user_text).collect()
}

pub async fn drive_generated_turn_input(
    case: &gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputCase,
    first_turn_compaction: bool,
) -> gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputObservation<String> {
    let (db, hook, writer, mut lifecycle, docs) = owned_test_hook_with_folded_case(case).await;
    let node = db.node.clone();
    let head = lifecycle.request().clone();
    assert_eq!(
        lifecycle
            .folded_selection()
            .iter()
            .map(|row| row.request_doc_id.clone())
            .collect::<Vec<_>>(),
        case.folded
            .iter()
            .map(|row| docs[&row.request_id].clone())
            .collect::<Vec<_>>(),
        "{}: native claim selects the modeled queue",
        case.name,
    );
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("answered".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut config = owned_config(0);
    config.context_message = case.context.clone().map(Message::user);
    config.folded_prompts = case
        .folded
        .iter()
        .map(|folded| gents_loop::loop_stream::FoldedPrompt {
            key: gents_session::request::lifecycle::queue::folded_input_key(
                &docs[&folded.request_id],
            ),
            message: Message::user(folded.content.clone()),
        })
        .collect();
    let compactor_saw = Arc::new(std::sync::Mutex::new(Vec::<Message>::new()));
    let published_before_reduction = Arc::new(std::sync::Mutex::new(0_usize));
    if first_turn_compaction {
        config.max_tokens = Some(6_000);
        config.context_window = 6_500;
        config.compaction_threshold = 0.25;
        let saw = compactor_saw.clone();
        let published_before_reduction = published_before_reduction.clone();
        let compaction_reader = lifecycle.access().unwrap();
        let request_doc_id = head.doc_id.clone();
        config.turn_compactor = Some(Arc::new(move |request| {
            *saw.lock().unwrap() = request
                .messages
                .iter()
                .map(|row| row.message.clone())
                .collect();
            let published_before_reduction = published_before_reduction.clone();
            let reader = compaction_reader.clone();
            let request_doc_id = request_doc_id.clone();
            Box::pin(async move {
                let observed = agent_response(&reader, &format!(
                        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }}, role: {{ _eq: "user" }} }}) {{ _docID }} }}"#,
                        gents_store::graphql::escape_graphql_string(&request_doc_id)
                    )).await;
                *published_before_reduction.lock().unwrap() = observed.data.unwrap()
                    ["AgentMessage"]
                    .as_array()
                    .unwrap()
                    .len();
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![TaggedMessage::unassociated(Message::user(
                        "compacted prompt",
                    ))],
                    reduction_key: "folded-first-turn".to_string(),
                    commit: TurnReductionCommit::new(|| async { Ok(()) }),
                })
            })
        }));
    }
    let collected = collect_owned_scripted_stream(
        run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user(case.head.clone())),
            if first_turn_compaction {
                vec![
                    TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
                    TaggedMessage::unassociated(Message::assistant("noted")),
                ]
            } else {
                Vec::new()
            },
            Arc::new(Vec::new()),
            config,
        ),
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert_eq!(collected.error, None, "{}", case.name);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        1,
        "{}: one inference answers every message",
        case.name
    );
    let provider_input = if first_turn_compaction {
        assert_eq!(
            histories[0].last(),
            Some(&Message::user("compacted prompt"))
        );
        trailing_user_texts(&compactor_saw.lock().unwrap())
    } else {
        trailing_user_texts(&histories[0])
    };

    let scoped_doc_id = gents_store::graphql::escape_graphql_string(&head.doc_id);
    let observed = agent_response(&lifecycle.access().unwrap(), &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{scoped_doc_id}" }}, role: {{ _eq: "user" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key }} }}"#
        )).await;
    assert!(!observed.has_errors(), "{:?}", observed.errors);
    let observed = observed.data.unwrap();
    let headers = observed["AgentMessage"].as_array().unwrap();
    if first_turn_compaction {
        assert_eq!(
            *published_before_reduction.lock().unwrap(),
            headers.len(),
            "{}: the admitted input is accepted before any reduction",
            case.name
        );
    }
    let mut authored = Vec::with_capacity(headers.len());
    for header in headers {
        let (_, native) = gents_session::session::load_canonical_message(
            &lifecycle.access().unwrap(),
            header["_docID"].as_str().unwrap(),
            &head.node_did,
            head.requester_did.as_deref(),
        )
        .await
        .unwrap();
        authored.push(
            gents_lean_vocab::families::fold_queue_contracts_turn_input::AuthoredWire {
                key: observed_authored_key(
                    header["message_key"].as_str().unwrap(),
                    &head.doc_id,
                    &docs,
                ),
                content: sole_user_text(&native),
            },
        );
    }
    for doc in docs.values() {
        let response = agent_query(
            &lifecycle.access().unwrap(),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state superseded_by_request_doc_id }} }}"#,
                gents_store::graphql::escape_graphql_string(doc)
            ),
        )
        .await;
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "superseded", "{}", case.name);
        assert_eq!(
            row["superseded_by_request_doc_id"],
            head.doc_id.as_str(),
            "{}",
            case.name
        );
    }

    let history = gents_session::session::load_sequenced_history_for_request(
        &node,
        lifecycle.request(),
        None,
        None,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        false,
    )
    .await
    .unwrap();
    assert!(history
        .iter()
        .all(|row| !matches!(&row.message, Message::User { .. })));
    let resumed_history = gents_session::session::load_sequenced_history_for_request(
        &node,
        lifecycle.request(),
        None,
        None,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        true,
    )
    .await
    .unwrap();
    assert!(resumed_history
        .iter()
        .any(|row| matches!(&row.message, Message::User { .. })));
    gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputObservation {
        authored,
        provider_input,
    }
}
