use super::*;

const ARGS: &str = r#"{"file_path":"/tmp/transcript-contract.txt"}"#;

async fn fixture(
    name: &str,
) -> (
    Arc<defra_node::EmbeddedNode>,
    DefraSessionHook,
    String,
    String,
) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:general",
        FailurePolicy::default(),
    );
    let session = hook.session_id().await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        &session,
        "general",
        "did:test:general",
        "general",
    )
    .await
    .unwrap();
    let request = format!("native-transcript-{name}");
    bind_interruptible_request(
        node.as_ref(),
        &hook,
        &request,
        &session,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    (node, hook, session, request)
}

fn tool_turn(ids: &[String]) -> Message {
    Message::Assistant {
        id: None,
        content: ids
            .iter()
            .map(|id| {
                AssistantContent::ToolCall(ToolCall {
                    id: id.clone(),
                    call_id: Some(id.clone()),
                    function: ToolFunction {
                        name: "read".into(),
                        arguments: serde_json::from_str(ARGS).unwrap(),
                    },
                    signature: None,
                    additional_params: None,
                })
            })
            .collect(),
    }
}

async fn accept_turn(
    hook: &DefraSessionHook,
    call_ids: &[usize],
    logical_result_ids: &[usize],
) -> (u32, Vec<String>) {
    let provider_source = if logical_result_ids.is_empty() {
        call_ids
    } else {
        assert_eq!(call_ids.len(), logical_result_ids.len());
        logical_result_ids
    };
    let provider_ids = provider_source
        .iter()
        .map(|id| format!("result-{id}"))
        .collect::<Vec<_>>();
    let internal_ids = call_ids
        .iter()
        .map(|id| format!("internal-{id}"))
        .collect::<Vec<_>>();
    let calls = internal_ids
        .iter()
        .zip(&provider_ids)
        .map(|(internal, provider)| (internal.as_str(), Some(provider.as_str())))
        .collect::<Vec<_>>();
    let sequence = publish_and_adopt_tool_turns(hook, &calls, tool_turn(&provider_ids)).await;
    (sequence, provider_ids)
}

async fn dispatch(hook: &DefraSessionHook, call_id: usize, provider_id: &str) {
    let internal = format!("internal-{call_id}");
    assert!(matches!(
        hook.on_tool_call("read", Some(provider_id.into()), &internal, ARGS)
            .await,
        ToolCallHookAction::Continue
    ));
}

async fn complete(hook: &DefraSessionHook, call_id: usize, provider_id: &str, payload: &str) {
    let internal = format!("internal-{call_id}");
    assert!(matches!(
        hook.on_tool_result(
            "read",
            Some(provider_id.into()),
            &internal,
            ARGS,
            &crate::tool_call_lifecycle::ToolOutcome::Completed(payload.into())
        )
        .await,
        HookAction::Continue
    ));
}

async fn history(node: &defra_node::EmbeddedNode, session: &str) -> Vec<Message> {
    crate::session::load_history(node, session, "did:test:general", None)
        .await
        .unwrap()
}

fn results(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => content.iter().find_map(|item| match item {
                UserContent::ToolResult(result) => Some(result.id.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

async fn completed(name: &str) -> (Arc<defra_node::EmbeddedNode>, DefraSessionHook, String) {
    let case = crate::lean_vocab_test::lean_transcript_case(name);
    let (node, hook, session, request) = fixture(name).await;
    publish_claimed_authored_input(
        &hook,
        &request,
        None,
        user_text_message("run transcript conformance tool"),
    )
    .await;
    let (sequence, ids) = accept_turn(
        &hook,
        &case.action_call_ids,
        &case.action_logical_result_ids,
    )
    .await;
    assert_eq!(sequence as usize, case.assistant_sequence);
    assert_eq!(ids.len(), 1, "completed action exports one provider result");
    assert_eq!(case.action_payload_hashes.len(), 1);
    dispatch(&hook, case.action_call_ids[0], &ids[0]).await;
    complete(
        &hook,
        case.action_call_ids[0],
        &ids[0],
        &format!("payload-{}", case.action_payload_hashes[0]),
    )
    .await;
    (node, hook, session)
}

#[tokio::test]
async fn generated_transcript_cases_drive_reachable_native_canonical_owners() {
    let ordering =
        crate::lean_vocab_test::lean_transcript_case("ordering_user_assistant_tool_result");
    let (node, hook, session) = completed(&ordering.name).await;
    let rows = history(&node, &session).await;
    assert_eq!(rows.len(), ordering.post_message_count);
    assert!(matches!(
        rows.as_slice(),
        [
            Message::User { .. },
            Message::Assistant { .. },
            Message::User { .. }
        ]
    ));
    let ordering_id = format!("result-{}", ordering.logical_result_id);
    assert_eq!(results(&rows), vec![ordering_id.as_str()]);
    let ordering_call = fetch_tool_call_row(&node, &session, &ordering_id).await;
    assert_eq!(
        ordering_call["message_sequence"]
            .as_u64()
            .map(|value| value as usize),
        Some(ordering.assistant_sequence)
    );
    assert_eq!(ordering_call["lifecycle_state"].as_str(), Some("completed"));
    assert_eq!(
        ordering_call["result"].as_str(),
        Some(format!("payload-{}", ordering.payload_hash).as_str())
    );
    assert_eq!(
        crate::session::max_sequence(&node, &session, "did:test:general", None)
            .await
            .unwrap() as usize,
        ordering.result_sequence
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        ordering.post_in_flight_count
    );

    let dedupe = crate::lean_vocab_test::lean_transcript_case("dedupe_duplicate_reuses_sequence");
    // Lost-ack replay occurs inside ToolCallLifecycle's private idempotent
    // terminal-delivery transaction. Re-entering the hook after it relinquishes
    // ownership is an illegal transition, not the modeled duplicate action.
    // Keep the generated case visible, but do not claim runtime coverage here.
    assert_eq!(dedupe.action, "observe_duplicate_tool_result");
    assert!(dedupe.expected_duplicate_reused_sequence);

    let distinct =
        crate::lean_vocab_test::lean_transcript_case("distinct_result_ids_append_distinct_rows");
    assert_eq!(distinct.action, "append_distinct_tool_result");
    assert!(!distinct.expected_pair_closed);
    let parallel =
        crate::lean_vocab_test::lean_transcript_case("parallel_results_share_assistant_turn");
    assert_eq!(
        parallel.action,
        "publish_once_dispatch_in_order_then_complete_each_parallel_result"
    );
    let (node, hook, session, request) = fixture(&parallel.name).await;
    publish_claimed_authored_input(
        &hook,
        &request,
        None,
        user_text_message("run parallel tools"),
    )
    .await;
    let (sequence, ids) = accept_turn(
        &hook,
        &parallel.action_call_ids,
        &parallel.action_logical_result_ids,
    )
    .await;
    assert_eq!(sequence as usize, parallel.assistant_sequence);
    assert_eq!(parallel.action_payload_hashes.len(), ids.len());
    for ((call_id, id), payload_hash) in parallel
        .action_call_ids
        .iter()
        .copied()
        .zip(&ids)
        .zip(parallel.action_payload_hashes.iter().copied())
    {
        dispatch(&hook, call_id, id).await;
        complete(&hook, call_id, id, &format!("payload-{payload_hash}")).await;
    }
    let rows = history(&node, &session).await;
    assert_eq!(rows.len(), parallel.post_message_count);
    assert_eq!(
        results(&rows),
        ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    for (id, payload_hash) in ids.iter().zip(&parallel.action_payload_hashes) {
        let row = fetch_tool_call_row(&node, &session, id).await;
        assert_eq!(
            row["message_sequence"].as_u64().map(|value| value as usize),
            Some(parallel.assistant_sequence)
        );
        assert_eq!(row["lifecycle_state"].as_str(), Some("completed"));
        assert_eq!(
            row["result"].as_str(),
            Some(format!("payload-{payload_hash}").as_str())
        );
    }

    let pair = crate::lean_vocab_test::lean_transcript_case("completed_tool_pair_closed");
    let (node, hook, session) = completed(&pair.name).await;
    let id = format!("result-{}", pair.logical_result_id);
    assert_eq!(
        fetch_tool_call_row(&node, &session, &id).await["lifecycle_state"].as_str(),
        Some("completed")
    );
    assert_eq!(results(&history(&node, &session).await), vec![id.as_str()]);
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        pair.post_in_flight_count
    );

    let abandon = crate::lean_vocab_test::lean_transcript_case("drop_abandon_not_strong_drain");
    let (node, hook, session, _) = fixture(&abandon.name).await;
    let (_, ids) = accept_turn(
        &hook,
        &abandon.action_call_ids,
        &abandon.action_logical_result_ids,
    )
    .await;
    assert_eq!(ids.len(), 1);
    dispatch(&hook, abandon.action_call_ids[0], &ids[0]).await;
    drop(hook);
    assert_eq!(
        fetch_tool_call_row(&node, &session, &ids[0]).await["lifecycle_state"].as_str(),
        Some("running")
    );
}
