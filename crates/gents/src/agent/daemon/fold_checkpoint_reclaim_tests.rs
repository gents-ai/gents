async fn create_user_message(
    node: &defra_node::EmbeddedNode,
    behavior: &ResolvedBehavior,
    session_id: &str,
    content: &str,
    queued_after: Option<&str>,
) -> AgentRequest {
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        uuid::Uuid::new_v4().to_string(),
        behavior.agent_did(),
        behavior.agent_did(),
        &behavior.behavior_id,
        session_id,
        content,
        "interactive",
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            behavior.agent_did(),
        ),
    );
    create.input.queue = queued_after.map(|active| gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(active.to_owned()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    crate::sign_agent_request_create(behavior.principal_identity().as_ref(), &mut create)
        .await
        .unwrap();
    let response = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let doc_id = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .expect("created request receipt")["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    crate::request_admission::load_request_for_admission_test(node, &doc_id)
        .await
        .unwrap()
}

async fn authored_user_entries(
    node: &Arc<defra_node::EmbeddedNode>,
    request: &AgentRequest,
) -> Vec<(String, crate::llm::message::Message)> {
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }}, role: {{ _eq: "user" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key }} }}"#,
            crate::graphql::escape_graphql_string(&request.doc_id)
        ))
        .await;
    let mut entries = Vec::new();
    for row in response.data.unwrap()["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            row["_docID"].as_str().unwrap(),
            &request.agent_did,
            request.requester_did.as_deref(),
        )
        .await
        .unwrap();
        entries.push((row["message_key"].as_str().unwrap().to_owned(), message));
    }
    entries
}

/// Seeded replay: a folded turn stops after publishing some or all of its
/// admitted input; with all of it published, a first-turn reduction
/// checkpoint is persisted behind it. The test then re-pends the same
/// physical request directly (production lease expiry terminalizes it
/// instead) and runs it through the daemon. The replayed request restores
/// the checkpoint without treating its provider tail as the prompt,
/// publishes each authored key exactly once, and consumes the folded message
/// once.
#[tokio::test]
async fn seeded_replay_of_a_folded_turn_keeps_authored_input_through_checkpoint_restore() {
    for (published_before_crash, persist_checkpoint) in [(0, false), (1, false), (2, true)] {
        let data_path = std::env::temp_dir()
            .join(format!("daemon-fold-checkpoint-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let identity: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(data_path.join("agent.key"), None).unwrap(),
        );
        let behavior = test_behavior_with_identity(identity);
        crate::test_support::install_test_behavior(
            node.as_ref(),
            behavior.agent_did(),
            &behavior.behavior_id,
        )
        .await;
        let session_id = uuid::Uuid::new_v4().to_string();
        crate::session::ensure_session_with_behavior_id_and_requester_did(
            node.as_ref(),
            &session_id,
            &behavior.behavior_id,
            behavior.agent_did(),
            &behavior.behavior_id,
            Some(behavior.agent_did()),
        )
        .await
        .unwrap();
        let head = create_user_message(&node, &behavior, &session_id, "how are we looking", None)
            .await;
        let folded = create_user_message(
            &node,
            &behavior,
            &session_id,
            "we should move faster",
            Some(&head.request_id),
        )
        .await;
        let folded_key = crate::lifecycle::queue::folded_input_key(&folded.doc_id);

        // First execution: claim with the selection, publish part of the
        // input, maybe persist the reduction checkpoint, then crash.
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            behavior.agent_did(),
            Duration::ZERO,
        );
        let mut first = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            &behavior.behavior_id,
            behavior.agent_did(),
            head.clone(),
            30,
        );
        first.set_fold_admitted(vec![folded.doc_id.clone()]);
        assert_eq!(
            first.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        first.begin_owned_execution(&writer).await.unwrap();
        let input = [
            ("prompt".to_owned(), head.content.clone()),
            (folded_key.clone(), folded.content.clone()),
        ];
        for (key, content) in input.iter().take(published_before_crash) {
            writer
                .publish_authored_message(&first, key, &crate::llm::message::Message::user(content))
                .await
                .unwrap();
        }
        if persist_checkpoint {
            let commit_cid = first.request_commit_cid().unwrap().to_owned();
            let boundary = crate::provider_context_reduction::capture_source_boundary(
                node.as_ref(),
                &session_id,
                behavior.agent_did(),
                Some(behavior.agent_did()),
                &head.doc_id,
                &commit_cid,
            )
            .await
            .unwrap();
            let prefix = vec![crate::llm::message::Message::user("an earlier, larger history")];
            let suffix = vec![
                crate::llm::message::Message::user(head.content.clone()),
                crate::llm::message::Message::user(folded.content.clone()),
            ];
            let row = |_| crate::provider_context_reduction::ReplayRowAssociation {
                source: None,
                physical_header: None,
                block_indices: Vec::new(),
            };
            crate::provider_context_reduction::persist(
                node.as_ref(),
                crate::provider_context_reduction::NewProviderContextReduction {
                    agent_did: behavior.agent_did(),
                    requester_did: Some(behavior.agent_did()),
                    session_id: &session_id,
                    request_id: &head.request_id,
                    request_doc_id: &head.doc_id,
                    request_commit_cid: &commit_cid,
                    reduction_index: 1,
                    turn_index: 0,
                    parent_reduction_key: None,
                    producer_call: None,
                    source_boundary: &boundary,
                    compacted_prefix: &prefix,
                    retained_suffix: &suffix,
                    checkpoint_messages: &suffix,
                    replay_associations:
                        &crate::provider_context_reduction::ReplayAssociations {
                            required: Vec::new(),
                            prefix_rows: prefix.iter().map(row).collect(),
                            retained_rows: suffix.iter().map(row).collect(),
                        },
                    summary: "",
                    original_tokens: 100,
                    compacted_tokens: 20,
                },
            )
            .await
            .unwrap();
        }
        let mut reclaimed = first.request().clone();
        reclaimed.deadline = None;
        drop(first);
        assert_eq!(
            crate::provider_context_reduction::load_unconsumed_for_request(
                node.as_ref(),
                &head.doc_id
            )
            .await
            .unwrap()
            .is_some(),
            persist_checkpoint
        );
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.fold_checkpoint_reclaim",
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending", deadline: null }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&head.doc_id)
            ),
        )
        .await
        .unwrap();

        let prompt_builder = LayeredPromptBuilder::for_behavior(
            &behavior.system_prompt,
            &behavior.behavior_id,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let request_identity = behavior.principal_identity().clone();
        let mut daemon = BehaviorDaemon::new(
            node.clone(),
            behavior.clone(),
            None,
            Arc::new(WakeInputModel {
                provider_inputs: provider_inputs.clone(),
                title_calls: Arc::new(AtomicUsize::new(0)),
                title_shape_mismatches: Arc::new(AtomicUsize::new(0)),
            }),
            preamble,
            Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            FailurePolicy::default(),
            Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
            Arc::new(StartupBarrier::ready_for_test()),
            crate::runtime_status::RuntimeStatusHandle::new(
                node.clone(),
                behavior.agent_did().to_string(),
            ),
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        )
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon.process_request(reclaimed, shutdown_rx).await.unwrap();

        let case = format!("published {published_before_crash}, checkpoint {persist_checkpoint}");
        assert_eq!(
            persisted_request_state(&node, &head.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
            "{case}"
        );
        assert_eq!(
            persisted_request_state(&node, &folded.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
            "{case}"
        );
        assert_eq!(
            authored_user_entries(&node, &head).await,
            input
                .iter()
                .map(|(key, content)| (
                    crate::session::canonical_rows::authored_message_key(&head.doc_id, key),
                    crate::llm::message::Message::user(content.clone()),
                ))
                .collect::<Vec<_>>(),
            "{case}: each authored key exactly once with its admitted content"
        );
        let inputs = provider_inputs.lock().unwrap().clone();
        assert_eq!(inputs.len(), 1, "{case}: one inference answers both messages");
        let sent = &inputs[0];
        let first_at = sent.find(&head.content).expect("head reaches the provider");
        let folded_at = sent.find(&folded.content).expect("folded message reaches the provider");
        assert!(first_at < folded_at, "{case}: queue order");
        if persist_checkpoint {
            assert!(
                crate::provider_context_reduction::load_unconsumed_for_request(
                    node.as_ref(),
                    &head.doc_id
                )
                .await
                .unwrap()
                .is_none(),
                "{case}: the restored checkpoint was the dispatched projection"
            );
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }
}

async fn create_retry(
    node: &defra_node::EmbeddedNode,
    behavior: &ResolvedBehavior,
    parent: &AgentRequest,
) -> AgentRequest {
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        uuid::Uuid::new_v4().to_string(),
        behavior.agent_did(),
        behavior.agent_did(),
        &behavior.behavior_id,
        &parent.session_id,
        &parent.content,
        "interactive",
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            behavior.agent_did(),
        ),
    );
    create.retry_parent_request = Some(parent.request_id.clone());
    create.retry_parent_request_doc_id = Some(parent.doc_id.clone());
    create.retry_root_request = Some(parent.request_id.clone());
    create.retry_count = 1;
    crate::sign_agent_request_create(behavior.principal_identity().as_ref(), &mut create)
        .await
        .unwrap();
    let response = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let doc_id = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .expect("created retry receipt")["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    crate::request_admission::load_request_for_admission_test(node, &doc_id)
        .await
        .unwrap()
}

async fn scripted_daemon(
    node: &Arc<defra_node::EmbeddedNode>,
    behavior: &Arc<ResolvedBehavior>,
    provider_inputs: &Arc<std::sync::Mutex<Vec<String>>>,
) -> BehaviorDaemon<WakeInputModel> {
    let prompt_builder = LayeredPromptBuilder::for_behavior(
        &behavior.system_prompt,
        &behavior.behavior_id,
        &[],
        false,
        &[],
    );
    let preamble = prompt_builder.preamble().to_string();
    BehaviorDaemon::new(
        node.clone(),
        behavior.clone(),
        None,
        Arc::new(WakeInputModel {
            provider_inputs: provider_inputs.clone(),
            title_calls: Arc::new(AtomicUsize::new(0)),
            title_shape_mismatches: Arc::new(AtomicUsize::new(0)),
        }),
        preamble,
        Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
        prompt_builder,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            behavior.agent_did().to_string(),
        ),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            behavior.principal_identity().clone(),
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap()
}

/// Lean `SessionQueue.FoldCases.retrySelectionCases`: a failed request is
/// retried while a fresh message waits queued behind the retry, so the
/// retry's claim selects it. Whether the retry resumes and whether it answers
/// the selected message come from `CurrentInput.admitResume` and
/// `CurrentInput.answersSelection`. A message the retry does not answer stays
/// queued, is absent from the retry's input and publications, and then runs
/// exactly once on its own turn.
#[tokio::test]
async fn generated_retry_selection_cases_bind_to_the_daemon() {
    let cases = crate::lean_vocab_test::lean_retry_selection_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(case.selected.len(), 1, "{}: one queued message", case.name);
        let data_path =
            std::env::temp_dir().join(format!("daemon-retry-selection-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let identity: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(data_path.join("agent.key"), None).unwrap(),
        );
        let behavior = test_behavior_with_identity(identity);
        crate::test_support::install_test_behavior(
            node.as_ref(),
            behavior.agent_did(),
            &behavior.behavior_id,
        )
        .await;
        let session_id = uuid::Uuid::new_v4().to_string();
        crate::session::ensure_session_with_behavior_id_and_requester_did(
            node.as_ref(),
            &session_id,
            &behavior.behavior_id,
            behavior.agent_did(),
            &behavior.behavior_id,
            Some(behavior.agent_did()),
        )
        .await
        .unwrap();
        let parent =
            create_user_message(&node, &behavior, &session_id, "how are we looking", None).await;
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            behavior.agent_did(),
            Duration::ZERO,
        );
        let mut failed = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            &behavior.behavior_id,
            behavior.agent_did(),
            parent.clone(),
            30,
        );
        assert_eq!(
            failed.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        failed.begin_owned_execution(&writer).await.unwrap();
        if case.parent_published {
            writer
                .publish_authored_message(
                    &failed,
                    "prompt",
                    &crate::llm::message::Message::user(parent.content.clone()),
                )
                .await
                .unwrap();
        }
        failed
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Failed,
                gents_protocol::output::TerminalOutput::NoMessage,
                Some("provider unavailable"),
            )
            .await
            .unwrap();
        drop(failed);

        let retry = create_retry(&node, &behavior, &parent).await;
        let queued = create_user_message(
            &node,
            &behavior,
            &session_id,
            "and ship it today",
            Some(&retry.request_id),
        )
        .await;
        let queued_key = crate::lifecycle::queue::folded_input_key(&queued.doc_id);
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut daemon = scripted_daemon(&node, &behavior, &provider_inputs).await;
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(retry.clone(), shutdown_rx.clone())
            .await
            .unwrap();
        assert_eq!(
            persisted_request_state(&node, &retry.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
            "{}",
            case.name
        );
        let retry_keys = authored_user_entries(&node, &retry)
            .await
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        let retry_input = provider_inputs.lock().unwrap()[0].clone();
        assert_eq!(
            retry_keys.is_empty(),
            case.resume,
            "{}: a resumed retry publishes no input",
            case.name
        );
        let answered = !case.answered.is_empty();
        assert_eq!(
            retry_keys.contains(&crate::session::canonical_rows::authored_message_key(
                &retry.doc_id,
                &queued_key
            )),
            answered,
            "{}: the retry publishes the selected message only when it answers it",
            case.name
        );
        assert_eq!(
            retry_input.contains(&queued.content),
            answered,
            "{}: the selected message reaches the retry's provider only when answered",
            case.name
        );
        if answered {
            assert_eq!(
                persisted_request_state(&node, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
                "{}",
                case.name
            );
            assert_eq!(provider_inputs.lock().unwrap().len(), 1, "{}", case.name);
        } else {
            assert_eq!(
                persisted_request_state(&node, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
                "{}: the unanswered message stays queued",
                case.name
            );
            daemon
                .process_request(queued.clone(), shutdown_rx)
                .await
                .unwrap();
            assert_eq!(
                persisted_request_state(&node, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
                "{}",
                case.name
            );
            let inputs = provider_inputs.lock().unwrap().clone();
            assert_eq!(inputs.len(), 2, "{}: it runs exactly once, on its own turn", case.name);
            assert!(inputs[1].contains(&queued.content), "{}", case.name);
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }
}
