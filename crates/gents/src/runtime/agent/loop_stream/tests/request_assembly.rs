#[tokio::test]
async fn generated_authored_input_is_durable_before_provider_stream_entry() {
    for case in gents_lean_vocab::families::contracts_queued_steering_trace_publication::cases() {
        let name = case.input.name.clone();
        let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let doc_id = lifecycle.request().doc_id.clone();
        let node_did = lifecycle.request().node_did.clone();
        let requester_did = lifecycle.request().requester_did.clone();
        let prompt = Message::user(case.input.prompt.clone());
        let gate = Arc::new(StreamEntryGate::default());
        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("accepted".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ])
        .with_stream_entry_gate(gate.clone());
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(prompt.clone()),
            Vec::new(),
            Arc::new(Vec::new()),
            owned_config(0),
        );
        let scoped_doc_id = gents_store::graphql::escape_graphql_string(&doc_id);
        let query = format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{scoped_doc_id}" }} }}) {{ _docID message_key }} }}"#
        );
        let reader = lifecycle.access().unwrap();
        let before = agent_response(&reader, &query).await;
        assert!(!before.has_errors(), "{name}: {:?}", before.errors);
        assert!(before.data.unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .is_empty());

        let (collected, authored_at_stream_entry) =
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(
                    async {
                        let collected = collect_owned_scripted_stream(
                            stream,
                            &hook,
                            &writer,
                            &mut lifecycle,
                            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
                        )
                        .await;
                        assert_eq!(collected.error, None, "{name}");
                        assert!(
                            !model.seen_requests().await.is_empty(),
                            "{name} ended before entering the provider stream"
                        );
                        collected
                    },
                    async {
                        gate.entered.notified().await;
                        let observed = agent_response(&reader, &query).await;
                        assert!(!observed.has_errors(), "{name}: {:?}", observed.errors);
                        let observed = observed.data.unwrap();
                        let headers = observed["AgentMessage"].as_array().unwrap().clone();
                        let [header] = headers.as_slice() else {
                            panic!("{name} has no unique authored input")
                        };
                        assert_eq!(
                            header["message_key"],
                            gents_protocol::output::rows::authored_message_key(&doc_id, "prompt"),
                            "{name}"
                        );
                        let header_id = header["_docID"].as_str().unwrap();
                        let (_, native) = gents_session::session::load_canonical_message(
                            &reader,
                            header_id,
                            &node_did,
                            requester_did.as_deref(),
                        )
                        .await
                        .unwrap();
                        assert_eq!(native, prompt, "{name}");
                        gate.release.notify_one();
                        headers.len() as u64
                    }
                )
            })
            .await
            .expect("provider stream did not reach the authored-input handoff");
        assert_eq!(collected.error, None, "{name}");
        let sent = model.seen_requests().await;
        assert!(sent.len() <= 1, "{name} sent its provider request twice");
        case.check(Some(
        gents_lean_vocab::families::contracts_queued_steering_trace_publication::PublicationOutput {
            canonical_authored_count: authored_at_stream_entry,
            provider_send_permitted: sent.len() == 1,
        },
    ));
    }
}

#[tokio::test]
async fn assembled_request_order_follows_the_lean_model() {
    use crate::turn::prompt::{LayeredPromptBuilder, PromptBuilder};

    for case in gents_lean_vocab::families::contracts_prompt_assembly_layer::cases() {
        let input = &case.input;
        let builder =
            LayeredPromptBuilder::for_agent("system prompt", "fence", &["bash"], false, &[]);

        let conversation = (0..input.conversation_len)
            .map(|index| {
                TaggedMessage::unassociated(Message::user(format!("conversation-{index}")))
            })
            .collect::<Vec<_>>();
        let summaries = (0..input.summary_count)
            .map(|index| format!("summary-{index}"))
            .collect::<Vec<_>>();
        let skill_reminders = (0..input.skill_count)
            .map(|index| LayeredPromptBuilder::system_reminder(&format!("skill-{index}")))
            .collect::<Vec<_>>();

        let built = builder
            .build(&conversation, &summaries)
            .await
            .expect("build layered prompt");

        let mut assembled = skill_reminders
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect::<Vec<_>>();
        assembled.extend(built.messages);
        assembled.extend(super::assemble_new_messages(
            None,
            TaggedMessage::unassociated(Message::user("prompt")),
        ));

        assert!(
            !builder.preamble().is_empty(),
            "the preamble slot must be carried by the system-prompt field"
        );
        let mut slots = vec!["preamble".to_string()];
        let mut conversation_index = 0usize;
        let assembled_len = assembled.len();
        for (position, message) in assembled.iter().enumerate() {
            slots.push(classify_slot(
                &message.message,
                position + 1 == assembled_len,
                &mut conversation_index,
            ));
        }

        case.check(slots);
    }
}

#[tokio::test]
async fn folded_prompts_publish_and_send_in_queue_order() {
    let cases = gents_lean_vocab::lean_fold_turn_input_cases();
    assert!(!cases.is_empty());
    for case in cases {
        drive_generated_turn_input(case, false).await;
    }
}

#[tokio::test]
async fn first_turn_compaction_keeps_folded_authored_input() {
    for case in gents_lean_vocab::lean_fold_turn_input_cases() {
        drive_generated_turn_input(case, true).await;
    }
}
