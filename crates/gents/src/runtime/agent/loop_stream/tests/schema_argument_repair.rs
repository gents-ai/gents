#[tokio::test]
async fn schema_argument_repairs_drive_the_owned_loop() {
    let cases = gents_lean_vocab::families::schema_argument_repair_owned_loop::cases();
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    let mut probes = Vec::new();
    let mut turn = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        for observed in &case.input.parses {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&observed.raw).ok(),
                observed.accepted.then(|| observed.parsed.0.clone()),
                "{}: native decoder premise",
                case.input.name
            );
        }
        let name = format!("repair_case_{index}");
        let admitted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatched = Arc::new(std::sync::Mutex::new(Vec::new()));
        probes.push((admitted.clone(), dispatched.clone()));
        tools.push(Box::new(SchemaArgumentProbe {
            name: name.clone(),
            schema: case.input.native_schema.0.clone(),
            admitted,
            dispatched,
            calls: calls.clone(),
        }));
        turn.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            format!("repair-{index}"),
            name,
            case.input.native_input.0.clone(),
        )));
    }
    turn.push(RawStreamingChoice::FinalResponse(()));
    let model = ScriptedModel::new_turns(vec![
        turn,
        vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let collected = collect_owned_scripted_stream(
        run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("exercise schema repairs")),
            Vec::new(),
            Arc::new(tools),
            owned_config(4),
        ),
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(calls.load(Ordering::SeqCst), cases.len());
    let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
    assert_eq!(rows.len(), cases.len());
    assert!(rows.iter().all(|row| row["lifecycle_state"] == "completed"));
    let access = lifecycle.access().unwrap();
    let requests = model.seen_requests().await;
    let threaded = requests
        .last()
        .unwrap()
        .chat_history
        .iter()
        .map(gents_inference::llm::rig_compat::from_rig_message)
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flat_map(|content| content.into_iter())
        .filter_map(|content| match content {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(threaded.len(), cases.len());
    for (index, case) in cases.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("repair-{index}"))
            .unwrap();
        let durable = gents_session::tool_call::query::result::load_tool_call_arguments(
            &access,
            row["_docID"].as_str().unwrap(),
            &lifecycle.request().node_did,
            &lifecycle.request().session_id,
            lifecycle.request().requester_did.as_deref(),
        )
        .await
        .unwrap();
        let durable = serde_json::from_str::<serde_json::Value>(&durable).unwrap();
        let (admitted, dispatched) = &probes[index];
        let observed = [
            admitted.lock().unwrap().clone(),
            dispatched.lock().unwrap().clone(),
            vec![durable],
            vec![threaded[index].function.arguments.clone()],
        ];
        assert!(
            observed.iter().all(|values| {
                !values.is_empty() && values.iter().all(|value| *value == observed[0][0])
            }),
            "{}: admission, dispatch, durable arguments and replay disagree: {observed:?}",
            case.input.name
        );
        case.check(gents_lean_vocab::family::Json(observed[0][0].clone()));
    }
    node.shutdown().await.expect("node shutdown");
}
