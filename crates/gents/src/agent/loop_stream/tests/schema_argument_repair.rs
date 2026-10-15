struct SchemaArgumentProbe {
    name: String,
    schema: serde_json::Value,
    expected: serde_json::Value,
    calls: Arc<AtomicUsize>,
}

impl ToolDyn for SchemaArgumentProbe {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "schema argument fixture".into(),
                parameters: self.schema.clone(),
            }
        })
    }

    fn admit(&self, args: &str) -> Result<(), ToolError> {
        let args: serde_json::Value = serde_json::from_str(args).unwrap();
        assert_eq!(args, self.expected, "{}: admission", self.name);
        Ok(())
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let args: serde_json::Value = serde_json::from_str(&args).unwrap();
            assert_eq!(args, self.expected, "{}: dispatch", self.name);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("accepted".into())
        })
    }
}

#[tokio::test]
async fn generated_schema_argument_repair_cases_drive_owned_loop() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().schema_argument_repair_cases;
    assert!(!cases.is_empty());
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    let mut turn = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        for observed in case["parses"].as_array().unwrap() {
            let parsed =
                serde_json::from_str::<serde_json::Value>(observed["raw"].as_str().unwrap()).ok();
            assert_eq!(
                parsed.is_some(),
                observed["accepted"].as_bool().unwrap(),
                "{}: native decoder acceptance",
                case["name"]
            );
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                observed["parsed"],
                "{}: native decoder",
                case["name"]
            );
        }
        let name = format!("repair_case_{index}");
        tools.push(Box::new(SchemaArgumentProbe {
            name: name.clone(),
            schema: case["native_schema"].clone(),
            expected: case["native_expected"].clone(),
            calls: calls.clone(),
        }));
        turn.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            format!("repair-{index}"),
            name,
            case["native_input"].clone(),
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
    let rows = tool_call_rows(&node).await;
    assert_eq!(rows.len(), cases.len());
    assert!(rows.iter().all(|row| row["lifecycle_state"] == "completed"));
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    for (index, case) in cases.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("repair-{index}"))
            .unwrap();
        let arguments = crate::tool_call_lifecycle::load_tool_call_arguments(
            &access,
            row["_docID"].as_str().unwrap(),
            "did:test:test",
            &lifecycle.request().session_id,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&arguments).unwrap(),
            case["native_expected"],
            "{}: durable accepted arguments",
            case["name"]
        );
    }
    let requests = model.seen_requests().await;
    let threaded = requests
        .last()
        .unwrap()
        .chat_history
        .iter()
        .map(crate::llm::rig_compat::from_rig_message)
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
            call.function.arguments, case["native_expected"],
            "{}: replay",
            case["name"]
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn config_container_repairs_preserve_native_validation_and_the_invalid_budget() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity: Arc<dyn crate::NodeIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(identity_dir.path().join("config-repair.key"), None)
            .unwrap(),
    );
    let owner = identity.did().to_owned();
    let (node, hook, writer, mut lifecycle) = owned_test_hook_with_identity(identity.clone()).await;
    crate::test_support::install_test_agent(&node, &owner, "general").await;
    let grants = crate::tool_surface::SelfConfigToolConfig {
        enabled: true,
        agent_id: "general".into(),
        categories: ["profile".to_string()].into_iter().collect(),
        preview: true,
        no_lockout: false,
        ..Default::default()
    };
    let tools = crate::self_config::build_self_config_tools(
        node.clone(),
        owner,
        Some(identity),
        &grants,
        Arc::new(crate::plugin::executor::PluginExecutor::default()),
    );
    let mut calls = (0..10)
        .map(|index| {
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                format!("config-repair-{index}"),
                "config".into(),
                serde_json::json!({
                    "argv": ["profile", "preview"],
                    "set": serde_json::json!({"display_name": "{\"literal\":true}"}).to_string(),
                    "options": serde_json::json!({"agent": "general"}).to_string()
                }),
            ))
        })
        .collect::<Vec<_>>();
    calls.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        "config-invalid-after-repair".into(),
        "config".into(),
        serde_json::json!({
            "argv": ["profile", "preview"],
            "set": serde_json::json!({"display_name": []}).to_string(),
            "options": serde_json::json!({"agent": "general"}).to_string()
        }),
    )));
    calls.push(RawStreamingChoice::FinalResponse(()));
    let model = ScriptedModel::new_turns(vec![
        calls,
        vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let collected = collect_owned_scripted_stream(
        run_loop_stream(
            model,
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("preview profiles")),
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
    assert_eq!(collected.tool_results.len(), 11);
    let rows = tool_call_rows(&node).await;
    assert_eq!(rows.len(), 11);
    for index in 0..10 {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("config-repair-{index}"))
            .unwrap();
        assert_eq!(
            row["lifecycle_state"], "completed",
            "{}",
            collected.tool_results[index]
        );
    }
    let refused = rows
        .iter()
        .find(|row| row["tool_call_id"] == "config-invalid-after-repair")
        .unwrap();
    assert_eq!(refused["lifecycle_state"], "failed");
    assert!(
        collected.tool_results[10].contains("display_name"),
        "{}",
        collected.tool_results[10]
    );
    node.shutdown().await;
}
