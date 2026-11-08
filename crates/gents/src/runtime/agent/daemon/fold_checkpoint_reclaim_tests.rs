async fn create_user_message(
    db: &crate::support::TestDb,
    author: (&crate::KeyIdentity, &crate::ConfigAccess),
    agent_config: &ResolvedAgent,
    session_id: &str,
    content: &str,
    queued_after: Option<&str>,
    retry_parent: Option<&AgentRequest>,
) -> AgentRequest {
    use crate::request::lifecycle::{
        ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec, RetryLink,
    };
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let (user, client) = author;
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            request_id: request_id.clone(),
            node_did: db.node_identity.did().to_owned(),
            requester_did: Some(user.did().to_owned()),
            agent_did: agent_config.agent_did.clone(),
            session_id: session_id.to_owned(),
            content: content.to_owned(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.input.queue = queued_after.map(|active| gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(active.to_owned()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    spec.retry = retry_parent.map(|parent| RetryLink {
        parent_request_id: Some(parent.request_id.clone()),
        parent_request_doc_id: Some(parent.doc_id.clone()),
        root_request_id: parent.request_id.clone(),
        retry_count: 1,
        max_retries: 3,
    });
    let create = crate::request::lifecycle::build_signed_client_request(
        client,
        spec,
        RequestSigner::Identity(user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(client, &create)
        .await
        .unwrap();
    let response = client
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(&request_id)
        ))
        .await
        .unwrap();
    let doc_id = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    crate::request::admission::load_request_for_admission_test_as(
        &db.node,
        doc_id,
        &agent_config.agent_did,
    )
    .await
    .unwrap()
}

async fn authored_user_entries(
    access: &crate::ConfigAccess,
    request: &AgentRequest,
) -> Vec<(String, crate::inference::llm::message::Message)> {
    let response = access.execute(&format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }}, role: {{ _eq: "user" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key }} }}"#,
        crate::store::graphql::escape_graphql_string(&request.doc_id)
    )).await.unwrap();
    let mut entries = Vec::new();
    for row in response["data"]["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message(
            access,
            row["_docID"].as_str().unwrap(),
            &request.node_did,
            request.requester_did.as_deref(),
        )
        .await
        .unwrap();
        entries.push((row["message_key"].as_str().unwrap().to_owned(), message));
    }
    entries
}

async fn admit_client_claim(
    node: &Arc<defra_node::Node>,
    config: &Arc<ResolvedAgent>,
    request: &AgentRequest,
) -> (
    AgentRequest,
    crate::request::admission::AgentRequestAdmissionVerifier,
    Option<crate::request::admission::ClientClaimAdmission>,
) {
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        config.node_identity().clone(),
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(request, config.agent_identity.as_ref())
        .await
        .unwrap();
    assert!(admission.is_some());
    (request, verifier, admission)
}

#[tokio::test]
async fn seeded_replay_of_a_folded_turn_keeps_authored_input_through_checkpoint_restore() {
    for (published_before_crash, persist_checkpoint) in [(0, false), (1, false), (2, true)] {
        let (db, user, client, agent_config) = held_daemon_fixture("daemon-fold-checkpoint").await;
        let node = db.node.clone();
        let agent_access = held_daemon_agent_access(&db, &agent_config);
        let session_id = uuid::Uuid::new_v4().to_string();
        let head = create_user_message(
            &db,
            (&user, &client),
            &agent_config,
            &session_id,
            "how are we looking",
            None,
            None,
        )
        .await;
        let folded = create_user_message(
            &db,
            (&user, &client),
            &agent_config,
            &session_id,
            "we should move faster",
            Some(&head.request_id),
            None,
        )
        .await;
        let folded_key = crate::request::lifecycle::queue::folded_input_key(&folded.doc_id);

        let writer = crate::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent_config.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        let (head, verifier, admission) = admit_client_claim(&node, &agent_config, &head).await;
        let (_, folded_admission) = verifier
            .verify_folded_for_claim(&folded.doc_id, agent_config.agent_identity.as_ref())
            .await
            .unwrap();
        assert!(folded_admission.is_some());
        let mut first = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
            node.clone(),
            &agent_config.agent_did,
            agent_config.node_did(),
            head.clone(),
            30,
        );
        first
            .bind_managed_agent(agent_config.agent_identity.as_ref())
            .unwrap();
        first.set_client_claim_admission(admission);
        first.set_steering_admission(verifier, agent_config.agent_identity.clone());
        first.set_fold_admitted(vec![folded.doc_id.clone()]);
        assert_eq!(
            first.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        first.begin_owned_execution(&writer).await.unwrap();
        let input = [
            ("prompt".to_owned(), head.content.clone()),
            (folded_key.clone(), folded.content.clone()),
        ];
        for (key, content) in input.iter().take(published_before_crash) {
            writer
                .publish_authored_message(
                    &first,
                    key,
                    &crate::inference::llm::message::Message::user(content),
                )
                .await
                .unwrap();
        }
        if persist_checkpoint {
            let commit_cid = first.request_commit_cid().unwrap().to_owned();
            let boundary = crate::turn::provider_context_reduction::capture_source_boundary(
                &agent_access,
                &session_id,
                agent_config.node_did(),
                Some(user.did()),
                &head.doc_id,
                &commit_cid,
            )
            .await
            .unwrap();
            let prefix = vec![crate::inference::llm::message::Message::user(
                "an earlier, larger history",
            )];
            let suffix = vec![
                crate::inference::llm::message::Message::user(head.content.clone()),
                crate::inference::llm::message::Message::user(folded.content.clone()),
            ];
            let row = |_| crate::turn::provider_context_reduction::ReplayRowAssociation {
                source: None,
                physical_header: None,
                block_indices: Vec::new(),
            };
            let signer = agent_config.agent_identity.managed_agent_signer().unwrap();
            crate::turn::provider_context_reduction::persist(
                &node,
                &signer,
                crate::turn::provider_context_reduction::NewProviderContextReduction {
                    agent_did: &agent_config.agent_did,
                    node_did: agent_config.node_did(),
                    requester_did: Some(user.did()),
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
                        &crate::turn::provider_context_reduction::ReplayAssociations {
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
            crate::turn::provider_context_reduction::load_unconsumed_for_request(
                &agent_access,
                &head.doc_id
            )
            .await
            .unwrap()
            .is_some(),
            persist_checkpoint
        );
        agent_access.write(
            "test.fold_checkpoint_reclaim",
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending", deadline: null }}) {{ _docID }} }}"#,
                crate::store::graphql::escape_graphql_string(&head.doc_id)
            ),
        )
        .await
        .unwrap();

        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(WakeInputModel {
                provider_inputs: provider_inputs.clone(),
                title_calls: Arc::new(AtomicUsize::new(0)),
                title_shape_mismatches: Arc::new(AtomicUsize::new(0)),
                successor: None,
            }),
            preamble,
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    node.clone(), agent_config.agent_identity.as_ref(),
                ).unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: crate::runtime::status::RuntimeStatusHandle::new(
                node.clone(),
                agent_config.node_did().to_string(),
            ),
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(reclaimed, shutdown_rx)
            .await
            .unwrap();

        let case = format!("published {published_before_crash}, checkpoint {persist_checkpoint}");
        assert_eq!(
            persisted_request_state(&client, &head.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
            "{case}"
        );
        assert_eq!(
            persisted_request_state(&client, &folded.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
            "{case}"
        );
        assert_eq!(
            authored_user_entries(&client, &head).await,
            input
                .iter()
                .map(|(key, content)| (
                    gents_protocol::output::rows::authored_message_key(&head.doc_id, key),
                    crate::inference::llm::message::Message::user(content.clone()),
                ))
                .collect::<Vec<_>>(),
            "{case}: each authored key exactly once with its admitted content"
        );
        let inputs = provider_inputs.lock().unwrap().clone();
        assert_eq!(
            inputs.len(),
            1,
            "{case}: one inference answers both messages"
        );
        let sent = &inputs[0];
        let first_at = sent.find(&head.content).expect("head reaches the provider");
        let folded_at = sent
            .find(&folded.content)
            .expect("folded message reaches the provider");
        assert!(first_at < folded_at, "{case}: queue order");
        if persist_checkpoint {
            assert!(
                crate::turn::provider_context_reduction::load_unconsumed_for_request(
                    &agent_access,
                    &head.doc_id
                )
                .await
                .unwrap()
                .is_none(),
                "{case}: the restored checkpoint was the dispatched projection"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
}

async fn scripted_daemon(
    node: &Arc<defra_node::Node>,
    agent_config: &Arc<ResolvedAgent>,
    provider_inputs: &Arc<std::sync::Mutex<Vec<String>>>,
) -> AgentDaemon<WakeInputModel> {
    let prompt_builder = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_did,
        &[],
        false,
        &[],
    );
    let preamble = prompt_builder.preamble().to_string();
    AgentDaemon::new(AgentDaemonInit {
        node: node.clone(),
        agent_config: agent_config.clone(),
        provider_family: None,
        model: Arc::new(WakeInputModel {
            provider_inputs: provider_inputs.clone(),
            title_calls: Arc::new(AtomicUsize::new(0)),
            title_shape_mismatches: Arc::new(AtomicUsize::new(0)),
            successor: None,
        }),
        preamble,
        loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
        prompt_builder,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: Some(
            crate::inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                node.clone(),
                agent_config.agent_identity.as_ref(),
            )
            .unwrap(),
        ),
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status: crate::runtime::status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        ),
        slot_generation: 1,
        request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            agent_config.node_identity().clone(),
            crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    })
    .unwrap()
}

/// Lean `SessionQueue.FoldCases.retrySelectionCases`: whether the retry
/// resumes and answers the selected message comes from the Lean cases; a
/// message the retry does not answer stays queued and runs exactly once on
/// its own turn.
#[tokio::test]
async fn generated_retry_selection_cases_bind_to_the_daemon() {
    let cases = crate::lean_vocab_test::lean_retry_selection_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(case.selected.len(), 1, "{}: one queued message", case.name);
        let (db, user, client, agent_config) = held_daemon_fixture("daemon-retry-selection").await;
        let node = db.node.clone();
        let session_id = uuid::Uuid::new_v4().to_string();
        let parent = create_user_message(
            &db,
            (&user, &client),
            &agent_config,
            &session_id,
            "how are we looking",
            None,
            None,
        )
        .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent_config.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        let (parent, _verifier, admission) =
            admit_client_claim(&node, &agent_config, &parent).await;
        let mut failed = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
            node.clone(),
            &agent_config.agent_did,
            agent_config.node_did(),
            parent.clone(),
            30,
        );
        failed
            .bind_managed_agent(agent_config.agent_identity.as_ref())
            .unwrap();
        failed.set_client_claim_admission(admission);
        assert_eq!(
            failed.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        failed.begin_owned_execution(&writer).await.unwrap();
        if case.parent_published {
            writer
                .publish_authored_message(
                    &failed,
                    "prompt",
                    &crate::inference::llm::message::Message::user(parent.content.clone()),
                )
                .await
                .unwrap();
        }
        failed
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Failed,
                gents_protocol::output::TerminalOutput::NoMessage,
                Some("provider unavailable"),
            )
            .await
            .unwrap();
        drop(failed);

        let retry = create_user_message(
            &db,
            (&user, &client),
            &agent_config,
            &session_id,
            &parent.content,
            None,
            Some(&parent),
        )
        .await;
        let queued = create_user_message(
            &db,
            (&user, &client),
            &agent_config,
            &session_id,
            "and ship it today",
            Some(&retry.request_id),
            None,
        )
        .await;
        let queued_key = crate::request::lifecycle::queue::folded_input_key(&queued.doc_id);
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut daemon = scripted_daemon(&node, &agent_config, &provider_inputs).await;
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(retry.clone(), shutdown_rx.clone())
            .await
            .unwrap();
        let retry_diagnostic = client.execute(
            &format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason }} }}"#,
                crate::store::graphql::escape_graphql_string(&retry.doc_id)),
        ).await.unwrap();
        assert_eq!(
            persisted_request_state(&client, &retry.doc_id).await,
            gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
            "{}: {:?}",
            case.name,
            retry_diagnostic
        );
        let retry_keys = authored_user_entries(&client, &retry)
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
            retry_keys.contains(&gents_protocol::output::rows::authored_message_key(
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
                persisted_request_state(&client, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
                "{}",
                case.name
            );
            assert_eq!(provider_inputs.lock().unwrap().len(), 1, "{}", case.name);
        } else {
            assert_eq!(
                persisted_request_state(&client, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
                "{}: the unanswered message stays queued",
                case.name
            );
            daemon
                .process_request(queued.clone(), shutdown_rx)
                .await
                .unwrap();
            assert_eq!(
                persisted_request_state(&client, &queued.doc_id).await,
                gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
                "{}",
                case.name
            );
            let inputs = provider_inputs.lock().unwrap().clone();
            assert_eq!(
                inputs.len(),
                2,
                "{}: it runs exactly once, on its own turn",
                case.name
            );
            assert!(inputs[1].contains(&queued.content), "{}", case.name);
        }
        node.shutdown().await.expect("node shutdown");
    }
}
