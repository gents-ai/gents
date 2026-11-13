#[tokio::test]
async fn loop_terminal_follows_the_lean_model() {
    let preamble = gents_loop::output_obligation::continuation_message(&[]);
    let preamble = preamble.split(": ").next().unwrap().to_owned();
    for case in gents_lean_vocab::families::completion_retry_output_obligation_loop_terminal::cases::<
        String,
    >() {
        let (db, _user, _user_access, binding) = gents_inference::test_support::signed_test_node(
            "unmet-output-obligation",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let actor = identity::Did::new(&binding.agent_did).unwrap();
        let model = ScriptedModel::new_turns(vec![
            vec![
                RawStreamingChoice::Message("premature answer".to_string()),
                RawStreamingChoice::FinalResponse(()),
            ],
            vec![
                RawStreamingChoice::Message("continuing".to_string()),
                RawStreamingChoice::FinalResponse(()),
            ],
        ]);
        let mut loop_config = config(2);
        loop_config.output_obligation_gate = Some(Arc::new(
            crate::runtime::agent::output_obligation::OutputObligationGate::new(
                node.clone(),
                actor,
                "request-doc-unmet",
                case.input
                    .obligations
                    .iter()
                    .map(|obligation| {
                        crate::runtime::agent::output_obligation::ActiveOutputObligation {
                            tool_name: obligation.tool.clone(),
                            contract: gents_documents::WriteToolOutputObligation {
                                scope: gents_documents::WriteToolOutputObligationScope::Request,
                                minimum_writes: obligation.minimum_writes as usize,
                                expected_count_field: None,
                            },
                        }
                    })
                    .collect(),
            ),
        )
            as Arc<dyn gents_loop::output_obligation::OutputObligationCheck>);
        let stream = run_loop_stream(
            model.clone(),
            None::<gents_toolcalls::call::hook::DefraSessionHook>,
            TaggedMessage::unassociated(Message::user("do the work")),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        );
        futures::pin_mut!(stream);

        let mut reminder = None;
        let mut saw_final = false;
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                LoopStreamItem::OutputObligationPending { reminder: sent } => {
                    reminder = Some(format!("{sent:?}"))
                }
                LoopStreamItem::Final { .. } => saw_final = true,
                LoopStreamItem::Text(text) if reminder.is_some() && text == "continuing" => break,
                _ => {}
            }
        }
        let histories = model.seen_histories().await;
        let next_turn = histories.get(1).map(|history| format!("{history:?}"));
        let handling = match (&reminder, saw_final, histories.len()) {
            (None, true, 1) => "release",
            (Some(_), false, 2)
                if next_turn
                    .as_deref()
                    .is_some_and(|next| next.contains(&preamble)) =>
            {
                "remind"
            }
            other => panic!("{:?}: unexpected loop observation {other:?}", case.input),
        };
        let reminded_tools = case
            .input
            .obligations
            .iter()
            .map(|obligation| obligation.tool.clone())
            .filter(|tool| {
                reminder
                    .as_deref()
                    .is_some_and(|reminder| reminder.contains(&format!("`{tool}`")))
            })
            .collect();
        case.check(
            gents_lean_vocab::families::completion_retry_output_obligation_loop_terminal::TerminalAnswer {
                handling: handling.to_owned(),
                reminded_tools,
            },
        );
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn retry_entry_does_not_publish_recorded_prompt_again() {
    for case in gents_lean_vocab::families::contracts_retry_entry_publish::cases() {
        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("continued".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let mut loop_config = config(1);
        loop_config.resume_from_history = case.input.resume;
        let context = gents_inference::rendered_request::RenderedRequestContext {
            request_doc_id: "retry-input-doc".into(),
            request_commit_cid: "bafy-retry".into(),
            request_id: "retry-input".into(),
            node_did: "did:key:agent".into(),
            requester_did: String::new(),
            agent_did: "general".into(),
            session_id: "retry-session".into(),
            model_name: "test-model".into(),
            provider_family: None,
        };
        let scope = gents_inference::rendered_request::scope::test_scope(
            context,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        );
        loop_config.on_rendered_request = Some(
            gents_inference::rendered_request::scope::ambient_arming_sink(
                gents_inference::rendered_request::scope::CaptureScopeKind::Inference,
            ),
        );
        let authored = gents_inference::rendered_request::scope::scope_request(scope, async {
            let stream = run_loop_stream(
                model,
                Some(gents_loop::session_hook::NoopSessionHook),
                TaggedMessage::unassociated(Message::user("durable frontier")),
                Vec::new(),
                Arc::new(Vec::new()),
                loop_config,
            );
            futures::pin_mut!(stream);
            let mut authored = 0;
            while let Some(item) = stream.next().await {
                match item.unwrap() {
                    LoopStreamItem::AuthoredInputReady { reply, .. } => {
                        authored += 1;
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    LoopStreamItem::SteeringBoundary { reply } => {
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    LoopStreamItem::FinishOrIntake { reply, .. } => {
                        let _ = reply.send(Ok(gents_loop::loop_stream::FinishOrIntake::Finished));
                    }
                    _ => {}
                }
            }
            authored
        })
        .await;
        case.check(
            gents_lean_vocab::families::contracts_retry_entry_publish::RetryEntryPublication {
                publish: match authored {
                    0 => false,
                    1 => true,
                    more => panic!("{:?}: authored input announced {more} times", case.input),
                },
            },
        );
    }
}
