use super::*;

const ARGS: &str = r#"{"file_path":"/tmp/transcript-contract.txt"}"#;

async fn fixture(
    name: &str,
) -> (
    gents_inference::test_support::TestDb,
    DefraSessionHook,
    String,
    String,
) {
    let db = gents_inference::test_support::test_db(
        "native-transcript",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let hook = signed_hook(&node, FailurePolicy::default()).await;
    let session = hook.session_id().await.unwrap();
    let request = format!("native-transcript-{name}");
    bind_interruptible_request(
        &hook,
        &request,
        &session,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    (db, hook, session, request)
}

fn tool_turn(ids: &[String]) -> Message {
    Message::Assistant {
        request_id: None,
        id: None,
        content: ids
            .iter()
            .map(|id| {
                AssistantContent::ToolCall(ToolCall {
                    raw_arguments: None,
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
            &gents_loop::tool_call_lifecycle::ToolOutcome::Completed(payload.into())
        )
        .await,
        HookAction::Continue
    ));
}

async fn history(hook: &DefraSessionHook, session: &str) -> Vec<Message> {
    hook_history(hook, session).await.unwrap()
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

fn role(message: &Message) -> String {
    match message {
        Message::System { .. } => "system",
        Message::User { .. } => "user",
        Message::Assistant { .. } => "assistant",
    }
    .to_owned()
}

fn numbered(value: &str, prefix: &str) -> u64 {
    value
        .strip_prefix(prefix)
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{value:?} is not {prefix}<number>"))
}

fn lifecycle(row: &serde_json::Value) -> String {
    row["lifecycle_state"]
        .as_str()
        .unwrap_or_else(|| panic!("tool call row without lifecycle_state: {row}"))
        .to_owned()
}

#[tokio::test]
async fn native_transcript_owners_follow_the_lean_witnesses() {
    for case in gents_lean_vocab::families::transcript_cases_native_completion::cases() {
        let input = &case.input;
        let (db, hook, session, request) = fixture(&input.name).await;
        let node = db.node.clone();
        publish_claimed_authored_input(
            &hook,
            &request,
            None,
            user_text_message("run transcript conformance tools"),
        )
        .await;
        let call_ids = input
            .action_call_ids
            .iter()
            .map(|id| usize::try_from(*id).expect("call id"))
            .collect::<Vec<_>>();
        let logical_result_ids = input
            .action_logical_result_ids
            .iter()
            .map(|id| usize::try_from(*id).expect("logical result id"))
            .collect::<Vec<_>>();
        let (assistant_sequence, ids) = accept_turn(&hook, &call_ids, &logical_result_ids).await;
        assert_eq!(
            input.action_payload_hashes.len(),
            ids.len(),
            "{}",
            input.name
        );
        for ((call_id, id), payload_hash) in call_ids
            .iter()
            .copied()
            .zip(&ids)
            .zip(&input.action_payload_hashes)
        {
            dispatch(&hook, call_id, id).await;
            complete(&hook, call_id, id, &format!("payload-{payload_hash}")).await;
        }
        let rows = history(&hook, &session).await;
        let mut tool_call_states = Vec::new();
        let mut tool_call_results = Vec::new();
        for id in &ids {
            let row = fetch_tool_call_row(&hook, &session, id).await;
            assert_eq!(
                row["message_sequence"].as_u64(),
                Some(u64::from(assistant_sequence)),
                "{}: tool call {id} belongs to the accepted assistant turn",
                input.name
            );
            tool_call_states.push(lifecycle(&row));
            if let Some(result) = row["result"].as_str() {
                tool_call_results.push(numbered(result, "payload-"));
            }
        }
        let max_sequence = gents_session::session::max_sequence_as(
            &node,
            &session,
            &hook.node_did,
            hook.active_requester_did().await.as_deref(),
            hook.actor.clone(),
        )
        .await
        .unwrap();
        let in_flight = hook.cancel_in_flight_tool_calls().await.unwrap();
        case.check(
            gents_lean_vocab::families::transcript_cases_native_completion::NativeCompletionOutput {
                assistant_sequence: u64::from(assistant_sequence),
                post_message_count: rows.len() as u64,
                message_roles: rows.iter().map(role).collect(),
                result_ids: results(&rows)
                    .into_iter()
                    .map(|id| numbered(id, "result-"))
                    .collect(),
                tool_call_states,
                tool_call_results,
                max_sequence: u64::from(max_sequence),
                post_in_flight_count: in_flight as u64,
            },
        );
    }

    for case in gents_lean_vocab::families::transcript_cases_native_abandon::cases() {
        let input = &case.input;
        let (_db, hook, session, _) = fixture(&input.name).await;
        let call_ids = input
            .action_call_ids
            .iter()
            .map(|id| usize::try_from(*id).expect("call id"))
            .collect::<Vec<_>>();
        let logical_result_ids = input
            .action_logical_result_ids
            .iter()
            .map(|id| usize::try_from(*id).expect("logical result id"))
            .collect::<Vec<_>>();
        let (_, ids) = accept_turn(&hook, &call_ids, &logical_result_ids).await;
        for (call_id, id) in call_ids.iter().copied().zip(&ids) {
            dispatch(&hook, call_id, id).await;
        }
        let reader = hook.actor_access().unwrap();
        drop(hook);
        let mut states = Vec::new();
        for id in &ids {
            states.push(lifecycle(
                &fetch_tool_call_row_as(&reader, &session, id).await,
            ));
        }
        case.check(states);
    }
}
