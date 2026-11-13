#[tokio::test]
async fn dispatch_receipt_loss_gates_real_hook_loop_invocation() {
    for case in gents_lean_vocab::families::dispatch_observation_contracts_owned_receipt::cases() {
        let input = &case.input;
        assert!(
            input.policy_allows,
            "{}: the counting tool admits every call",
            input.name
        );
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
                assert!(fired, "{}: dispatch commit must be reached", input.name);
                collected
            };
            let rows = lifecycle
                .access()
                .unwrap()
                .execute("query { AgentToolCall { lifecycle_state } }")
                .await
                .unwrap();
            let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
            assert_eq!(rows.len(), 1, "publication must precede dispatch");
            let loop_failed = collected.error.is_some();
            case.check(Some(gents_lean_vocab::families::dispatch_observation_contracts_owned_receipt::OwnedReceiptOutput {
                invocations: calls.load(Ordering::SeqCst) as u64,
                loop_failed,
                delivered_result: !loop_failed
                    && collected.tool_results == ["observed"]
                    && collected.final_text.as_deref() == Some("done"),
                stopped_with_running_call: loop_failed
                    && collected.tool_results.is_empty()
                    && rows[0]["lifecycle_state"] == "running",
            }));
            node.shutdown().await.expect("node shutdown");
        }
    }
}

#[tokio::test]
async fn policy_rejection_settles_pending_call_without_dispatch_election() {
    for case in gents_lean_vocab::families::dispatch_observation_contracts_owned_policy_settlement::cases() {
        assert!(case
            .input
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
                case.check(Some(gents_lean_vocab::families::dispatch_observation_contracts_owned_policy_settlement::PolicySettlementOutput {
                    election_committed: fired,
                    after_policy_settlement: gents_lean_vocab::families::dispatch_observation_contracts_owned_policy_settlement::PolicySettlementWire {
                        failed: row["lifecycle_state"] == "failed",
                        running: row["lifecycle_state"] == "running",
                        started: row["started_at"].is_string(),
                        failure_class: row["tool_failure_class"].as_str().map(str::to_owned),
                        completion_accepted: completion.is_ok(),
                    },
                }));
                node.shutdown().await.expect("node shutdown");
            }
        }
    }
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
