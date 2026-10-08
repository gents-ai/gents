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

/// A folded turn crashes after publishing some or all of its admitted input;
/// with all of it published, a first-turn reduction checkpoint was persisted
/// behind it. The reclaimed request restores that checkpoint without
/// treating its provider tail as the prompt, publishes each authored key
/// exactly once, and consumes the folded message once.
#[tokio::test]
async fn reclaimed_folded_turn_keeps_authored_input_through_checkpoint_restore() {
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

        // The runtime reclaims the same physical request.
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

/// Lean `CurrentInput.answersSelection`: a user retry of a failed folded turn.
/// A message the parent consumed is history the resumed retry continues
/// from. A message the parent never published is still queued ahead of the
/// retry, keeps its own turn, and is answered exactly once.
#[tokio::test]
async fn retry_of_a_failed_folded_turn_answers_each_message_once() {
    for published_before_failure in [0, 1, 2] {
        let data_path =
            std::env::temp_dir().join(format!("daemon-fold-retry-{}", uuid::Uuid::new_v4()));
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
        let head =
            create_user_message(&node, &behavior, &session_id, "how are we looking", None).await;
        let folded = create_user_message(
            &node,
            &behavior,
            &session_id,
            "we should move faster",
            Some(&head.request_id),
        )
        .await;
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            behavior.agent_did(),
            Duration::ZERO,
        );
        let mut failed = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            &behavior.behavior_id,
            behavior.agent_did(),
            head.clone(),
            30,
        );
        failed.set_fold_admitted(vec![folded.doc_id.clone()]);
        assert_eq!(
            failed.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        failed.begin_owned_execution(&writer).await.unwrap();
        let input = [
            ("prompt".to_owned(), head.content.clone()),
            (
                crate::lifecycle::queue::folded_input_key(&folded.doc_id),
                folded.content.clone(),
            ),
        ];
        for (key, content) in input.iter().take(published_before_failure) {
            writer
                .publish_authored_message(&failed, key, &crate::llm::message::Message::user(content))
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

        let retry = create_retry(&node, &behavior, &head).await;
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
        daemon
            .process_request(retry.clone(), shutdown_rx.clone())
            .await
            .unwrap();
        let case = format!("published {published_before_failure} before failure");
        if persisted_request_state(&node, &retry.doc_id).await
            == gents_protocol::request_lifecycle::RequestLifecycleState::Pending
        {
            assert!(published_before_failure < 2, "{case}: only a queued message precedes it");
            daemon
                .process_request(folded.clone(), shutdown_rx.clone())
                .await
                .unwrap();
            daemon
                .process_request(retry.clone(), shutdown_rx)
                .await
                .unwrap();
        }

        assert_eq!(
            persisted_request_state(&node, &retry.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
            "{case}"
        );
        let inputs = provider_inputs.lock().unwrap().clone();
        let folded_state = persisted_request_state(&node, &folded.doc_id).await;
        let retry_entries = authored_user_entries(&node, &retry)
            .await
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        if published_before_failure == 2 {
            assert_eq!(inputs.len(), 1, "{case}: one resumed inference");
            assert!(retry_entries.is_empty(), "{case}: a resumed retry publishes nothing");
            assert_eq!(
                folded_state,
                gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
                "{case}: consumed once, by the parent"
            );
            assert!(inputs[0].contains(&folded.content), "{case}: answered from history");
        } else {
            assert_eq!(inputs.len(), 2, "{case}: the queued message, then the retry");
            assert_eq!(
                folded_state,
                gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
                "{case}: the queued message keeps its own turn"
            );
            assert_eq!(
                retry_entries,
                if published_before_failure == 0 {
                    vec![crate::session::canonical_rows::authored_message_key(
                        &retry.doc_id,
                        "prompt",
                    )]
                } else {
                    Vec::new()
                },
                "{case}: the retry publishes only input its parent did not"
            );
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }
}
