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

#[tokio::test]
async fn generated_child_admission_cases_drive_real_dispatcher() {
    let cases = gents_lean_vocab::lean_contract_snapshot()
        .plugin_resource_child_admission_cases
        .as_array()
        .unwrap();
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
    for selected in [false, true] {
        let mut children = Vec::new();
        let mut expected = std::collections::BTreeMap::new();
        for case in cases.iter().filter(|case| case["selected"] == selected) {
            let admitted = case["admitted"] == true;
            let refused = |ordinal: u32| serde_json::json!({"admit": false, "ordinal": ordinal});
            let calls: Vec<(&'static str, serde_json::Value, &'static str)> = match case["route"]
                .as_str()
                .unwrap()
            {
                "ordinary" => vec![("child", serde_json::json!({"admit": admitted}), "completed")],
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
                expected.insert(
                    ordinal,
                    (case.clone(), name, case["expected"] == true, settled),
                );
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
        assert_eq!(completed.len(), expected.len());
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
            let (case, name, admissible, settled) = &expected[&ordinal];
            if observed != *admissible {
                mismatches.push(format!("dispatcher result {observed} for {name} {case}"));
            }
            if invoked.contains(&u64::from(ordinal)) != (*admissible && case["route"] == "ordinary")
            {
                mismatches.push(format!("invocation of {name} {case}"));
            }
            let state = &states[&u64::from(ordinal)];
            if state != if *admissible { *settled } else { "failed" } {
                mismatches.push(format!("durable child {name} is {state} {case}"));
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
}
