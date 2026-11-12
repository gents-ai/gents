#[tokio::test]
async fn dispatch_receipt_loss_gates_real_hook_loop_invocation() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().canonical_dispatch_observation_cases;
    let cases: Vec<_> = cases
        .iter()
        .filter(|case| case.inputs[0].policy_allows)
        .collect();
    assert!(cases.iter().any(|case| case.inputs[0].acknowledged));
    assert!(cases.iter().any(|case| !case.inputs[0].acknowledged));
    for case in cases {
        let input = &case.inputs[0];
        let expected = &case.expected[0];
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let (db, hook, writer, mut lifecycle) = owned_test_hook_with_policy(policy).await;
            let node = db.node.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let model = ScriptedModel::new_turns(vec![
                vec![
                    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        "dispatch-call".into(),
                        "echo".into(),
                        serde_json::json!({}),
                    )),
                    RawStreamingChoice::FinalResponse(()),
                ],
                vec![
                    RawStreamingChoice::Message("done".into()),
                    RawStreamingChoice::FinalResponse(()),
                ],
            ]);
            let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
                name: "echo".into(),
                output: "observed".into(),
                calls: calls.clone(),
            })];
            let stream = run_loop_stream(
                model,
                Some(hook.clone()),
                TaggedMessage::unassociated(Message::user("run echo")),
                Vec::new(),
                Arc::new(tools),
                owned_config(4),
            );
            let collect = collect_owned_scripted_stream(
                stream,
                &hook,
                &writer,
                &mut lifecycle,
                gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            );
            let collected = if input.acknowledged {
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                        Some("test.unrelated_transaction"), collect,
                    ).await;
                assert!(
                    !fired,
                    "unrelated commits must not consume a targeted fault"
                );
                collected
            } else {
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                        Some("tool_call.start_running_canonical"), collect,
                    ).await;
                assert!(fired, "{}: dispatch commit must be reached", case.name);
                collected
            };
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(expected.may_invoke),
                "{}: policy {policy:?}",
                case.name
            );
            if expected.may_invoke {
                assert!(collected.error.is_none(), "{:?}", collected.error);
                assert_eq!(collected.tool_results, ["observed"]);
                assert_eq!(collected.final_text.as_deref(), Some("done"));
            } else {
                assert!(
                    collected.error.is_some(),
                    "lost authority must stop the loop"
                );
                assert!(collected.tool_results.is_empty());
                let rows = lifecycle
                    .access()
                    .unwrap()
                    .execute("query { AgentToolCall { lifecycle_state } }")
                    .await
                    .unwrap();
                let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "publication must precede dispatch");
                assert_eq!(
                    rows[0]["lifecycle_state"], "running",
                    "receipt loss must not roll back the committed dispatch"
                );
            }
            node.shutdown().await.expect("node shutdown");
        }
    }
}

#[tokio::test]
async fn policy_rejection_settles_pending_call_without_dispatch_election() {
    let cases: Vec<_> = gents_lean_vocab::lean_contract_snapshot()
        .canonical_dispatch_observation_cases
        .iter()
        .filter_map(|case| {
            case.expected_after_policy_settlement
                .as_ref()
                .map(|settlement| (case, settlement))
        })
        .collect();
    assert!(!cases.is_empty());
    for (case, settlement) in cases {
        assert!(case
            .inputs
            .iter()
            .all(|input| input.acknowledged && !input.policy_allows));
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            for owner in PolicyDeniedOwner::ALL {
                let (db, hook, writer, mut lifecycle) = owned_test_hook_with_policy(policy).await;
                let node = db.node.clone();
                let root = tempfile::tempdir().unwrap();
                let denied = owner.denied_call(&node, root.path());
                let model = ScriptedModel::new_turns(vec![
                    vec![
                        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                            "policy-call".into(),
                            denied.tool_name.into(),
                            denied.arguments,
                        )),
                        RawStreamingChoice::FinalResponse(()),
                    ],
                    vec![
                        RawStreamingChoice::Message("done".into()),
                        RawStreamingChoice::FinalResponse(()),
                    ],
                ]);
                let stream = run_loop_stream(
                    model,
                    Some(hook.clone()),
                    TaggedMessage::unassociated(Message::user("run the denied call")),
                    Vec::new(),
                    Arc::new(denied.tools),
                    owned_config(4),
                );
                let collect = collect_owned_scripted_stream(
                    stream,
                    &hook,
                    &writer,
                    &mut lifecycle,
                    gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
                );
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                        Some("tool_call.start_running_canonical"), collect,
                    ).await;

                let rows = lifecycle
                    .access()
                    .unwrap()
                    .execute(
                        "query { AgentToolCall { lifecycle_state started_at tool_failure_class } }",
                    )
                    .await
                    .unwrap();
                let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "publication precedes admission");
                let row = &rows[0];
                assert_eq!(
                    (
                        row["lifecycle_state"] == "failed",
                        row["lifecycle_state"] == "running",
                        row["started_at"].is_string(),
                        row["tool_failure_class"].as_str(),
                    ),
                    (
                        settlement.failed,
                        settlement.running,
                        settlement.started,
                        settlement.failure_class.as_deref(),
                    ),
                    "{}/{}: policy {policy:?}",
                    case.name,
                    owner.owner(),
                );
                assert_eq!(
                    fired,
                    case.expected.iter().any(|expected| expected.running),
                    "{}/{}: the dispatch election commits only for a modeled Running call",
                    case.name,
                    owner.owner(),
                );
                assert!(collected.error.is_none(), "{:?}", collected.error);
                assert_eq!(collected.tool_results.len(), 1);
                assert_eq!(collected.final_text.as_deref(), Some("done"));
                assert!(root.path().exists());

                let completion = lifecycle
                    .terminalize_owned(
                        gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
                        writer.terminal_output(&lifecycle.request().doc_id).await,
                        None,
                    )
                    .await;
                assert_eq!(
                    completion.is_ok(),
                    settlement.completion_accepted,
                    "{}/{}: {completion:?}",
                    case.name,
                    owner.owner(),
                );
                node.shutdown().await.expect("node shutdown");
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn tool_does_not_execute_when_provider_stalls_before_turn_closure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new_stalling(vec![RawStreamingChoice::ToolCall(
        RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
        calls: calls.clone(),
    })];
    let stream = run_loop_stream(
        model,
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("use the echo tool then stall")),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);
    let first = stream.next().await.expect("should observe tool intent");
    assert!(
        matches!(first, Ok(LoopStreamItem::ToolCall { .. })),
        "first item should be streamed intent: {first:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .is_err(),
        "a stalled provider must neither accept its turn nor dispatch a tool"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn toolset_is_attached_to_every_completion_request_in_the_loop() {
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("use the echo tool")),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let seen_tools = model.seen_tools().await;
    assert_eq!(
        seen_tools.len(),
        2,
        "expected two completion turns; got {seen_tools:?}"
    );
    for (turn, tools) in seen_tools.iter().enumerate() {
        assert!(
            tools.contains(&"echo".to_string()),
            "completion request for turn {} must advertise the toolset; got {seen_tools:?}",
            turn + 1
        );
    }
}

#[tokio::test]
async fn plugin_image_parts_reach_the_provider_whole() {
    const IMAGE_BYTES: usize = 900_000;
    let result = plugin_image_result_seen_by(
        gents_inference::backends::provider::BackendProviderKind::AnthropicApiKey,
        IMAGE_BYTES,
    )
    .await;
    assert_eq!(result.len(), 2, "one text part and one image part");
    assert_eq!(tool_result_text(&result[0]), r#"{"height":2,"width":2}"#);
    let ToolResultContent::Image(image) = &result[1] else {
        panic!("the second part is the image: {:?}", result[1]);
    };
    assert_eq!(
        image.media_type,
        Some(gents_inference::llm::message::ImageMediaType::PNG)
    );
    let gents_inference::llm::message::DocumentSourceKind::Base64(data) = &image.data else {
        panic!("the image arrives as base64 data");
    };
    assert_eq!(
        data.len(),
        IMAGE_BYTES,
        "the image is not cut by the text bound"
    );
}

#[tokio::test]
async fn persisted_plugin_image_result_replays_text_parts_and_a_note() {
    for image_bytes in [64, 400_000] {
        persisted_plugin_image_result_replay(image_bytes).await;
    }
}

#[tokio::test]
async fn plugin_image_parts_become_a_note_where_the_provider_refuses_them() {
    let result = plugin_image_result_seen_by(
        gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
        64,
    )
    .await;
    assert_eq!(
        result.iter().map(tool_result_text).collect::<Vec<_>>(),
        vec![
            r#"{"height":2,"width":2}"#,
            gents_loop::loop_stream::TOOL_RESULT_IMAGE_OMITTED,
        ]
    );
}

#[tokio::test]
async fn completed_terminalization_scans_request_output_once_for_many_tools() {
    const TOOL_TURNS: usize = 6;
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let mut script = (0..TOOL_TURNS)
        .map(|turn| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("call-{turn}"),
                    "echo".into(),
                    serde_json::json!({}),
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    script.push(vec![
        RawStreamingChoice::Message("done".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".into(),
        output: "observed".into(),
    })];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(script),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("run echo")),
        Vec::new(),
        Arc::new(tools),
        owned_config(TOOL_TURNS + 2),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.tool_results.len(), TOOL_TURNS);
    let selection = writer.terminal_output(&lifecycle.request().doc_id).await;
    let (result, scans) =
        gents_session::session::count_request_output_scans(lifecycle.terminalize_owned(
            gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
            selection,
            None,
        ))
        .await;
    assert_eq!(
        result.unwrap(),
        gents_session::request::lifecycle::TerminalizeResult::Won
    );
    assert_eq!(scans, 1, "request output scanned once per accepted tool");
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
#[ignore = "manual terminalization scale measurement"]
async fn terminalization_scale_grid() {
    let grid = std::env::var("TERMINALIZE_SCALE").unwrap_or_else(|_| "24:4096:0,24:4096:40".into());
    for point in grid.split(',') {
        let values = point
            .split(':')
            .map(|v| v.parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        terminalization_with_principal_output(values[0], values[1], values[2]).await;
    }
}

#[tokio::test]
async fn background_output_query_work_is_independent_of_principal_history() {
    use gents_protocol::output::rows::{
        output_segment_create_variables, request_output_segments_query,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{OutputSource, OutputWriter};
    let (db, _hook, _writer, lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let request_id = lifecycle.request().doc_id.clone();
    let template = gents_protocol::output::OutputSegment {
        node_did: lifecycle.request().node_did.clone(),
        requester_did: None,
        session_id: "session".into(),
        request_doc_id: request_id.clone(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: "tool-doc".into(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: "tool-doc".into(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: 6,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::ToolOutput,
            }),
        }],
        payload: "output".into(),
        close: Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![6],
        }),
        created_at: "2026-10-02T00:00:00Z".into(),
    };
    async fn create(node: &defra_node::Node, segment: &gents_protocol::output::OutputSegment) {
        let response = node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(segment).unwrap()),
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    fn scan<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a serde_json::Value> {
        if value.get(field).is_some() {
            return Some(value);
        }
        match value {
            serde_json::Value::Object(object) => {
                object.values().find_map(|value| scan(value, field))
            }
            serde_json::Value::Array(array) => array.iter().find_map(|value| scan(value, field)),
            _ => None,
        }
    }
    create(&node, &template).await;
    let query = request_output_segments_query(&request_id);
    let mut work = Vec::new();
    for noise in [0, 160] {
        if noise > 0 {
            for replica in 0..noise {
                let mut segment = template.clone();
                segment.request_doc_id = format!("noise-request-{replica}");
                segment.session_id = format!("noise-session-{replica}");
                segment.payload = "n".repeat(4096);
                segment.runs[0].bytes = 4096;
                segment.close = Some(gents_protocol::output::SourceClose::Closed {
                    outcome: gents_protocol::output::OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![4096],
                });
                create(&node, &segment).await;
            }
        }
        let response = gents_store::graphql::graphql_with_transaction_retry(
            &node,
            &format!("query @explain(type: execute) {query}"),
            "explain request output",
        )
        .await
        .unwrap();
        let data = response.data.unwrap();
        let request_scan = scan(&data, "indexFetches")
            .unwrap_or_else(|| panic!("request output must report index execution work: {data}"));
        let plan = gents_store::graphql::graphql_with_transaction_retry(
            &node,
            &format!("query @explain {query}"),
            "explain request output plan",
        )
        .await
        .unwrap()
        .data
        .unwrap();
        let plan_scan = scan(&plan, "indexName")
            .unwrap_or_else(|| panic!("request output must use an index scan: {plan}"));
        assert!(
            plan_scan["indexName"]
                .as_str()
                .unwrap()
                .contains("request_doc_id"),
            "{plan}"
        );
        work.push(
            request_scan["indexFetches"]
                .as_u64()
                .expect("execution must report index work"),
        );
        let output = gents_session::session::tool_output_rows::canonical_tool_output(
            &node,
            "tool-doc",
            &request_id,
            "session",
            &lifecycle.request().node_did,
            None,
        )
        .await
        .unwrap();
        assert_eq!(output, "output");
        assert_eq!(
            gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &gents_store::config_client::ConfigAccess::local(node.clone()),
                "tool-doc",
                &request_id,
                "session",
                &lifecycle.request().node_did,
                None,
            )
            .await
            .unwrap(),
            gents_session::session::tool_output_rows::CanonicalToolOutputObservation::Closed(
                output
            )
        );
    }
    assert_eq!(
        work[0], work[1],
        "request query work grew with unrelated history"
    );
    assert!(work[0] > 0);
    drop(lifecycle);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn generated_child_admission_cases_drive_real_dispatcher() {
    let cases =
        gents_lean_vocab::families::tool_policy_plugin_calls_child_admissible::cases::<String>();
    let hook_owned = hook_owned_children();
    let mut driven: Vec<&str> = hook_owned.iter().map(|(name, ..)| *name).collect();
    let mut routes: Vec<&str> = gents_toolcalls::call::hook::persistence::HookOwnedTool::ROUTES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    driven.sort_unstable();
    routes.sort_unstable();
    assert_eq!(
        driven, routes,
        "every hook-owned route is driven exactly once"
    );
    let mut scenarios: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    for (index, case) in cases.iter().enumerate() {
        scenarios[usize::from(case.input.selected)].push(index);
    }
    let mut dispatched: Vec<Vec<bool>> = vec![Vec::new(); cases.len()];
    for (selected, members) in [false, true].into_iter().zip(scenarios) {
        let mut children = Vec::new();
        let mut owners = std::collections::BTreeMap::new();
        for index in members {
            let case = &cases[index];
            let admitted = case.input.admitted;
            let refused = |ordinal: u32| serde_json::json!({"admit": false, "ordinal": ordinal});
            let calls: Vec<(&'static str, serde_json::Value, &'static str)> =
                match case.input.route.as_str() {
                    "ordinary" => {
                        vec![("child", serde_json::json!({"admit": admitted}), "completed")]
                    }
                    "hook_owned" => hook_owned.clone(),
                    route => panic!("unknown route {route}"),
                };
            for (name, mut arguments, settled) in calls {
                let ordinal = children.len() as u32 + 1;
                if !admitted {
                    arguments = refused(ordinal);
                } else if name == "child" {
                    arguments["ordinal"] = ordinal.into();
                }
                children.push((ordinal, name, arguments.to_string()));
                owners.insert(ordinal, (index, name, settled));
            }
        }
        let completed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invoked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ComposingParent {
            children,
            completed: completed.clone(),
        })];
        if selected {
            for name in std::iter::once("child").chain(hook_owned.iter().map(|(name, ..)| *name)) {
                tools.push(Box::new(AdmissionChild {
                    name,
                    invoked: invoked.clone(),
                }));
            }
        }
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let hook = hook
            .with_goal_tool_authority(true, true)
            .with_goal_tools(std::sync::Arc::new(
                gents_automation::goal::goal_tools::GoalToolAnswers,
            ))
            .with_background_tool_registry(
                gents_toolcalls::call::hook::BackgroundToolRegistry::from_tools(
                    vec![Box::new(AdmissionChild {
                        name: "child",
                        invoked: invoked.clone(),
                    })],
                    &["child".into()],
                ),
            );
        allow_general_agent_target(&node, lifecycle.request()).await;
        let stream = run_loop_stream(
            ScriptedModel::new_turns(plugin_call_then_text_turns()),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("compose")),
            Vec::new(),
            Arc::new(tools),
            owned_config(4),
        );
        let collected = gents_tools::runtime_context::scope_request_tool_execution(
            Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
            tokio_util::sync::CancellationToken::new(),
            Box::pin(collect_owned_scripted_stream(
                stream,
                &hook,
                &writer,
                &mut lifecycle,
                gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            )),
        )
        .await;
        assert!(collected.error.is_none(), "{:?}", collected.error);
        let completed = completed.lock().unwrap().clone();
        let invoked = invoked.lock().unwrap().clone();
        assert_eq!(completed.len(), owners.len());
        let rows = lifecycle
            .access()
            .unwrap()
            .execute("query { AgentToolCall { plugin_effect_ordinal lifecycle_state } }")
            .await
            .unwrap();
        let states: std::collections::BTreeMap<u64, String> = rows["data"]["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| {
                Some((
                    row["plugin_effect_ordinal"].as_u64()?,
                    row["lifecycle_state"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let mut mismatches = Vec::new();
        for (ordinal, observed) in completed {
            let (index, name, settled) = &owners[&ordinal];
            let case = &cases[*index];
            dispatched[*index].push(observed);
            if invoked.contains(&u64::from(ordinal)) != (observed && case.input.route == "ordinary")
            {
                mismatches.push(format!("invocation of {name} {:?}", case.input));
            }
            let state = &states[&u64::from(ordinal)];
            if state != if observed { *settled } else { "failed" } {
                mismatches.push(format!("durable child {name} is {state} {:?}", case.input));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        if selected {
            assert!(
                invoked.contains(&SPAWNED_ORDINAL),
                "spawn_process ran the process it admitted"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
    for (case, results) in cases.iter().zip(dispatched) {
        let admissible = *results
            .first()
            .unwrap_or_else(|| panic!("no child of {:?} was dispatched", case.input));
        assert!(
            results.iter().all(|result| *result == admissible),
            "the children of {:?} were dispatched differently: {results:?}",
            case.input
        );
        case.check(admissible);
    }
}

#[tokio::test]
async fn independent_reads_overlap_and_results_keep_call_order() {
    for fail_first in [false, true] {
        assert_overlapping_reads_keep_call_order(fail_first).await;
    }
}

#[tokio::test]
#[ignore = "manual matched multi-read timing"]
async fn measure_parallel_native_read_turns() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let root = tempfile::tempdir().unwrap();
    for index in 0..8 {
        tokio::fs::write(
            root.path().join(format!("{index}.txt")),
            vec![b'x'; 1024 * 1024],
        )
        .await
        .unwrap();
    }
    let mut totals = [Duration::ZERO; 2];
    for round in 0..10 {
        for parallel in [round % 2 == 0, round % 2 != 0] {
            let native = gents_tools::native::ToolSet::builder()
                .read_root(root.path())
                .read_file()
                .build()
                .build_native_tools()
                .unwrap()
                .pop()
                .unwrap();
            let tool: Box<dyn ToolDyn> = if parallel {
                native
            } else {
                Box::new(SerialReadMeasurement(native))
            };
            let mut turn: Vec<_> = (0..8)
                .map(|index| {
                    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        format!("call-{index}"),
                        "read_file".into(),
                        serde_json::json!({"path":format!("{index}.txt"), "max_chars":256}),
                    ))
                })
                .collect();
            turn.push(RawStreamingChoice::FinalResponse(()));
            let model = ScriptedModel::new_turns(vec![
                turn,
                vec![
                    RawStreamingChoice::Message("done".into()),
                    RawStreamingChoice::FinalResponse(()),
                ],
            ])
            .without_capture();
            let started = std::time::Instant::now();
            let stream = run_loop_stream(
                model,
                None::<gents_loop::session_hook::NoopSessionHook>,
                TaggedMessage::unassociated(Message::user("read")),
                Vec::new(),
                Arc::new(vec![tool]),
                config(4),
            );
            futures::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                item.expect("measurement turn succeeds");
            }
            totals[usize::from(parallel)] += started.elapsed();
        }
    }
    tracing::info!(revision = %option_env!("GENTS_BENCH_REVISION").unwrap_or("unspecified"), calls_per_turn = 8, bytes_per_file = 1024 * 1024, turns_per_mode = 10, serial_us = u64::try_from(totals[0].as_micros()).unwrap_or(u64::MAX), parallel_us = u64::try_from(totals[1].as_micros()).unwrap_or(u64::MAX), "matched native read turn measurement");
}
