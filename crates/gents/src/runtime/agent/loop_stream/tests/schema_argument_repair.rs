#[tokio::test]
async fn generated_schema_argument_repair_cases_drive_owned_loop() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().schema_argument_repair_cases;
    assert!(!cases.is_empty());
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    let mut turn = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        for observed in &case.parses {
            let parsed = serde_json::from_str::<serde_json::Value>(&observed.raw).ok();
            assert_eq!(
                parsed.is_some(),
                observed.accepted,
                "{}: native decoder acceptance",
                case.name
            );
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                observed.parsed.clone().unwrap_or(serde_json::Value::Null),
                "{}: native decoder",
                case.name
            );
        }
        let name = format!("repair_case_{index}");
        tools.push(Box::new(SchemaArgumentProbe {
            name: name.clone(),
            schema: serde_json::json!({
                "type": case.native_schema.r#type,
                "properties": {
                    "value": serde_json::Value::Object(
                        case.native_schema
                            .properties
                            .value
                            .iter()
                            .map(|(key, field)| (key.clone(), field.clone()))
                            .collect(),
                    ),
                },
                "required": case.native_schema.required,
                "additionalProperties": case.native_schema.additional_properties,
            }),
            expected: serde_json::json!({ "value": case.native_expected.value }),
            calls: calls.clone(),
        }));
        turn.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            format!("repair-{index}"),
            name,
            serde_json::json!({ "value": case.native_input.value }),
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
    for (index, case) in cases.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("repair-{index}"))
            .unwrap();
        let arguments = gents_session::tool_call::query::result::load_tool_call_arguments(
            &access,
            row["_docID"].as_str().unwrap(),
            &lifecycle.request().node_did,
            &lifecycle.request().session_id,
            lifecycle.request().requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&arguments).unwrap(),
            serde_json::json!({ "value": case.native_expected.value }),
            "{}: durable accepted arguments",
            case.name
        );
    }
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
    for (call, case) in threaded.iter().zip(cases) {
        assert_eq!(
            call.function.arguments,
            serde_json::json!({ "value": case.native_expected.value }),
            "{}: replay",
            case.name
        );
    }
    node.shutdown().await.expect("node shutdown");
}
