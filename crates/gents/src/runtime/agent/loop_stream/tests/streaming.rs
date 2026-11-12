#[tokio::test]
async fn unmet_output_obligation_blocks_terminal_and_continues_with_runtime_reminder() {
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
            vec![
                crate::runtime::agent::output_obligation::ActiveOutputObligation {
                    tool_name: "write_result".to_string(),
                    contract: gents_documents::WriteToolOutputObligation {
                        scope: gents_documents::WriteToolOutputObligationScope::Request,
                        minimum_writes: 1,
                        expected_count_field: None,
                    },
                },
            ],
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

    let mut saw_pending = false;
    let mut saw_final = false;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            LoopStreamItem::OutputObligationPending { reminder } => {
                saw_pending = true;
                assert!(format!("{reminder:?}").contains("write_result"));
            }
            LoopStreamItem::Final { .. } => saw_final = true,
            LoopStreamItem::Text(text) if saw_pending && text == "continuing" => break,
            _ => {}
        }
    }

    assert!(saw_pending);
    assert!(!saw_final);
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert!(format!("{:?}", histories[1]).contains("configured output obligation is unmet"));
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn retry_entry_does_not_publish_recorded_prompt_again() {
    for case in &gents_lean_vocab::lean_contract_snapshot().retry_entry_cases {
        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("continued".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let mut loop_config = config(1);
        loop_config.resume_from_history = case.resume;
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
        assert_eq!(authored, usize::from(case.publish), "{case:?}");
    }
}
