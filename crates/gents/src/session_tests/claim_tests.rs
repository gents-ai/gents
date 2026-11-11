use std::sync::Arc;
use gents_session::request::lifecycle::claim::*;

    fn test_node_did() -> String {
        use gents_store::identity::NodeIdentity as _;
        gents_automation::triggers::callback::test_root_identity()
            .did()
            .to_owned()
    }

    fn test_agent_did() -> String {
        gents_automation::triggers::callback::test_agent_did()
    }

    const TEST_BACKEND_ID: &str = "backend-order";

    async fn test_node() -> Arc<EmbeddedNode> {
        // The node runs under the fixture root with native ACP enabled, so
        // request rows are authorized by their creator and working Agent, as in
        // production. The managed Agent signer bound by `test_lifecycle` names
        // this node.
        let node = Arc::new(
            gents_automation::triggers::callback::test_node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();
        gents_automation::triggers::callback::seed_test_agent(&node, &test_node_did()).await;
        node
    }

    /// The runtime root's access: it creates request rows and grants their
    /// working Agent, as the creating owner does in production.
    fn root_access(node: &Arc<EmbeddedNode>) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(test_node_did()).unwrap(),
        )
    }

    /// The fixture Agent's managed access, used for every lifecycle write a
    /// request performs (claim, begin, goal and requeue writes).
    async fn load_agent_goal(
        node: &Arc<EmbeddedNode>,
        session: &str,
    ) -> anyhow::Result<Option<gents_automation::goal::GoalDocument>> {
        agent_access(node)
            .transact_readonly("test.claim_agent_goal", |txn| {
                Box::pin(async move {
                    gents_automation::goal::load_canonical_goal_in_txn(txn, &test_node_did(), session).await
                })
            })
            .await
    }

    /// The working Agent owns the sessions its claims and goals observe.
    async fn ensure_agent_session(
        node: &Arc<EmbeddedNode>,
        session: &str,
        node_did: &str,
        requester_did: Option<&str>,
    ) -> anyhow::Result<()> {
        agent_access(node)
            .transact_write("test.claim_agent_session", |txn| {
                Box::pin(async move {
                    gents_session::session::ensure_session_in_txn(
                        txn,
                        session,
                        node_did,
                        &test_agent_did(),
                        requester_did,
                        None,
                        None,
                        "2026-09-01T00:00:00Z",
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
    }

    fn agent_access(node: &Arc<EmbeddedNode>) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(test_agent_did()).unwrap(),
            gents_automation::triggers::callback::test_agent_identity().as_ref(),
        )
        .unwrap()
    }

    fn test_lifecycle(
        node: Arc<EmbeddedNode>,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: &str,
    ) -> RequestLifecycle {
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node,
            request,
            deadline_duration_secs,
            execution_origin,
            backend_id,
        );
        let agent = gents_automation::triggers::callback::test_agent_identity();
        lifecycle
            .bind_managed_agent(agent.as_ref())
            .expect("bind the fixture managed Agent signer");
        lifecycle
    }

    async fn insert_pending_request(
        node: &std::sync::Arc<EmbeddedNode>,
        request_id: &str,
        session_id: &str,
        created_at: &str,
        input: Option<&gents_protocol::request_input::RequestInput>,
        execution_origin: &str,
    ) -> AgentRequest {
        insert_pending_request_for_owner(
            node,
            &test_node_did(),
            request_id,
            session_id,
            created_at,
            input,
            execution_origin,
        )
        .await
    }

    async fn insert_pending_request_for_owner(
        node: &std::sync::Arc<EmbeddedNode>,
        owner: &str,
        request_id: &str,
        session_id: &str,
        created_at: &str,
        input: Option<&gents_protocol::request_input::RequestInput>,
        execution_origin: &str,
    ) -> AgentRequest {
        let escaped_owner = escape_graphql_string(owner);
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_created_at = escape_graphql_string(created_at);
        let input_literal =
            gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(input).unwrap())
                .unwrap();
        let escaped_execution_origin = escape_graphql_string(execution_origin);
        let escaped_agent_did = escape_graphql_string(&test_agent_did());
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{escaped_request_id}",
                    purpose: "normal",
                    node_did: "{escaped_owner}",
                    agent_did: "{escaped_agent_did}",
                    session_id: "{escaped_session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{escaped_request_id}",
                    superseded_by_request: "",
                    content: "same-session request",
                    input: {input_literal},
                    lifecycle_state: "pending",
                    backend_id: "",
                    execution_origin: "{escaped_execution_origin}",
                    failure_reason: "",
                    created_at: "{escaped_created_at}",
                    retry_count: 0,
                    max_retries: {max_retries},
                    request_hop: 0
                }}) {{ {fields} }}
            }}"#,
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
            fields = gents_protocol::row::AGENT_REQUEST_FIELDS,
        );
        let agent = identity::Did::new(test_agent_did()).unwrap();
        root_access(node)
            .transact_write("test.insert_pending_request", |txn| {
                let mutation = &mutation;
                let agent = &agent;
                Box::pin(async move {
                    let response = txn.execute_local_response(mutation).await?;
                    let request = gents_session::request::watcher::agent_request_from_mutation_response(
                        &response,
                        "create_AgentRequest",
                    )?
                    .ok_or_else(|| anyhow::anyhow!("created request receipt missing"))?;
                    txn.add_actor_relationship(
                        "AgentRequest",
                        &request.doc_id,
                        "working_agent",
                        agent,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(request)
                })
            })
            .await
            .unwrap()
    }

    async fn durable_request_row(
        node: &Arc<EmbeddedNode>,
        doc_id: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let doc_id = escape_graphql_string(doc_id);
        let response = agent_access(node)
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
                gents_protocol::row::AGENT_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap()
    }

    #[tokio::test]
    async fn durable_claim_and_begin_use_real_owners_without_arming_local_renewal() {
        let node = test_node().await;
        let request = insert_pending_request(
            &node,
            "durable-claim",
            "durable-session",
            "2023-11-14T22:13:25Z",
            None,
            "interactive",
        )
        .await;
        let doc_id = request.doc_id.clone();
        let mut lifecycle = test_lifecycle(
            node.clone(),
            request,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        lifecycle.set_execution_lease_duration(std::time::Duration::from_secs(5));
        let now = chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:25Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let outcome = lifecycle
            .claim_pending_durable_with_inputs(|| now, || (now, "lean-generation-7".into()))
            .await
            .unwrap();
        let DurableClaimOutcome::Claimed(receipt) = outcome else {
            panic!("fresh request was not durably claimed")
        };
        assert_eq!(receipt.request.doc_id, doc_id);
        assert_eq!(receipt.execution_generation, "lean-generation-7");
        assert_eq!(receipt.lease_ms, 5_000);
        assert_eq!(lifecycle.state, LocalLifecycleState::Pending);
        assert!(lifecycle.execution_lease.is_none());
        assert!(lifecycle.renewal_task.is_none());

        let claimed = durable_request_row(&node, &doc_id).await;
        assert_eq!(
            claimed.lifecycle_state,
            Some(RequestLifecycleState::Claimed)
        );
        assert_eq!(
            claimed.execution_generation.as_deref(),
            Some("lean-generation-7")
        );
        assert_eq!(
            claimed.execution_lease_expires_at.as_deref(),
            Some("2023-11-14T22:13:30+00:00")
        );
        let agent = agent_access(&node);
        RequestLifecycle::begin_owned_execution_at_with_access(
            &agent,
            &doc_id,
            "lean-generation-7",
            || now,
        )
        .await
        .unwrap();
        RequestLifecycle::begin_owned_execution_at_with_access(
            &agent,
            &doc_id,
            "lean-generation-7",
            || now + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
        assert_eq!(
            durable_request_row(&node, &doc_id).await.lifecycle_state,
            Some(RequestLifecycleState::Processing)
        );
        assert!(
            RequestLifecycle::begin_owned_execution_at_with_access(
                &agent,
                &doc_id,
                "lean-generation-7",
                || now + chrono::Duration::seconds(6),
            )
            .await
            .is_err(),
            "expired replay must not acknowledge ownership"
        );
    }

    #[tokio::test]
    async fn claim_queue_uses_arrival_order_and_scopes_sessions_to_owner() {
        let node = test_node().await;
        let _foreign = insert_pending_request_for_owner(
            &node,
            "did:test:foreign-owner",
            "foreign-first",
            "shared-session-id",
            "2025-01-01T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        let first = insert_pending_request(
            &node,
            "z-first",
            "shared-session-id",
            "2026-01-02T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        let second = insert_pending_request(
            &node,
            "a-second",
            "shared-session-id",
            "2026-01-01T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        for index in 0..63 {
            insert_pending_request(
                &node,
                &format!("session-later-{index}"),
                "shared-session-id",
                "2026-01-01T00:00:02Z",
                None,
                "interactive",
            )
            .await;
        }
        for (request, expected) in [(&first, true), (&second, false)] {
            let allowed = agent_access(&node)
                .transact_write("test.claim_queue_arrival_order", |txn| {
                    Box::pin(
                        async move { gents_session::request::lifecycle::query::claim_queue_allows(txn, request).await },
                    )
                })
                .await
                .unwrap();
            assert_eq!(allowed, expected);
        }
    }

    #[tokio::test]
    async fn serial_claim_considers_active_owner_requests_across_pages() {
        let node = test_node().await;
        let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
        let queue_cases = contract["trigger_delivery"]["queues"].as_array().unwrap();
        let identity = |source_doc_id: &str| gents_protocol::trigger_delivery::FireIdentity {
            owner_did: test_node_did(),
            trigger_id: "serial-trigger".into(),
            source_collection: "Work".into(),
            source_doc_id: source_doc_id.into(),
        };
        let first_identity = identity("serial-first-source");
        let next_identity = identity("serial-next-source");
        let mut cohort = vec![
            (first_identity.clone(), "serial-first-session".to_owned()),
            (next_identity.clone(), "serial-next-session".to_owned()),
        ];
        cohort.extend((0..63).map(|index| {
            (
                identity(&format!("serial-later-source-{index}")),
                format!("serial-later-session-{index}"),
            )
        }));
        let first_request_id = first_identity.request_id();
        let next_request_id = next_identity.request_id();
        let first = insert_pending_request(
            &node,
            &first_request_id,
            "serial-first-session",
            "2026-01-01T00:00:00Z",
            None,
            "scheduled",
        )
        .await;
        let next = insert_pending_request(
            &node,
            &next_request_id,
            "serial-next-session",
            "2026-01-01T00:00:01Z",
            None,
            "scheduled",
        )
        .await;
        for (identity, session_id) in cohort.iter().skip(2) {
            let request_id = identity.request_id();
            insert_pending_request(
                &node,
                &request_id,
                session_id,
                "2026-01-01T00:00:02Z",
                None,
                "scheduled",
            )
            .await;
        }
        let receipts = cohort
            .iter()
            .map(|(identity, session_id)| {
                let fire_key = identity.fire_key();
                let request_id = identity.request_id();
                serde_json::json!({
                    "fire_key": fire_key,
                    "owner_did": identity.owner_did.as_str(),
                    "trigger_id": identity.trigger_id.as_str(),
                    "source_collection": identity.source_collection.as_str(),
                    "source_doc_id": identity.source_doc_id.as_str(),
                    "request_id": request_id,
                    "session_id": session_id,
                    "task_id": "serial-task",
                    "queued_serial": true,
                    "emit_outcome": false,
                    "created_at": "2026-01-01T00:00:00Z"
                })
            })
            .collect::<Vec<_>>();
        // The firing Agent stages each receipt beside the request it creates.
        agent_access(&node).transact_write(
            "test.seed_serial_claim_receipts",
            |txn| {
                let receipts = receipts.clone();
                Box::pin(async move {
                    for receipt in receipts {
                        txn.execute_with_variables(
                            "mutation($input: TriggerFireMutationInputArg!) { create_TriggerFire(input: $input) { _docID } }",
                            &serde_json::json!({"input": receipt}),
                        )
                        .await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await
        .unwrap();

        for (name, request) in [
            ("serial_first_claims", &first),
            ("serial_second_waits", &next),
        ] {
            let expected = queue_cases
                .iter()
                .find(|case| case["name"] == name)
                .unwrap()["can_claim"]
                .as_bool()
                .unwrap();
            let allowed = agent_access(&node)
                .transact_write("test.serial_claim_trigger_cohort", |txn| {
                    Box::pin(
                        async move { gents_session::request::lifecycle::query::claim_queue_allows(txn, request).await },
                    )
                })
                .await
                .unwrap();
            assert_eq!(allowed, expected, "{name}");
        }
    }

    #[tokio::test]
    async fn queued_task_goal_changes_only_when_its_request_claims() {
        for stopped in [false, true] {
            for task_budget in [None, Some(50)] {
                let node = test_node().await;
                let session = "task-goal-session";
                ensure_agent_session(&node, session, &test_node_did(), None)
                    .await
                    .unwrap();
                let original = gents_automation::goal::set_goal_from_access(
                    &agent_access(&node),
                    &test_node_did(),
                    session,
                    Some("finish original work"),
                    Some(gents_protocol::goal_status::GoalStatus::Active),
                    Some(Some(100)),
                    None,
                )
                .await
                .unwrap();
                let first = insert_pending_request(
                    &node,
                    "prior-request",
                    session,
                    "2026-01-01T00:00:00Z",
                    None,
                    "interactive",
                )
                .await;
                let first_doc_id = first.doc_id.clone();
                let second = insert_pending_request(
                    &node,
                    "task-request",
                    session,
                    "2026-01-01T00:00:01Z",
                    None,
                    "interactive",
                )
                .await;
                let assigned_doc_id = second.doc_id.clone();
                let receipt = serde_json::json!({
                    "fire_key": "task-goal-fire", "owner_did": test_node_did(),
                    "task_id": "goal-task", "created_at": "2026-01-01T00:00:01Z",
                    "trigger_id": "task-goal-trigger", "source_collection": "Work",
                    "source_doc_id": "assignment", "request_id": second.request_id,
                    "session_id": session, "queued_serial": true, "emit_outcome": false,
                    "goal_id": original.goal_id, "goal_objective": "perform next assignment",
                    "goal_token_budget": task_budget, "goal_assignment_applied": false,
                });
                agent_access(&node).transact_write("test.task_assignment", |txn| {
            let receipt = receipt.clone();
            Box::pin(async move {
                txn.execute_with_variables("mutation($input: TriggerFireMutationInputArg!) { create_TriggerFire(input: $input) { _docID } }",
                    &serde_json::json!({"input": receipt})).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
                let mut first_lifecycle = test_lifecycle(
                    node.clone(),
                    first,
                    60,
                    ExecutionOrigin::Interactive,
                    TEST_BACKEND_ID,
                );
                let mut second_lifecycle = test_lifecycle(
                    node.clone(),
                    second,
                    60,
                    ExecutionOrigin::Interactive,
                    TEST_BACKEND_ID,
                );
                let now = chrono::Utc::now();
                assert!(first_lifecycle
                    .claim_pending_durable_with_inputs(|| now, || (now, "first".into()))
                    .await
                    .unwrap()
                    .was_claimed());
                assert!(matches!(
                    second_lifecycle
                        .claim_pending_durable_with_inputs(|| now, || (now, "second".into()))
                        .await
                        .unwrap(),
                    DurableClaimOutcome::NotClaimed(ClaimOutcome::Queued)
                ));
                let unchanged = load_agent_goal(&node, session).await.unwrap().unwrap();
                assert_eq!(unchanged.objective, original.objective);
                assert_eq!(
                    unchanged.continuation_sequence(),
                    original.continuation_sequence()
                );
                let finish = format!(
                    r#"mutation {{
            update_AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}
            create_InferenceCall(input: {{call_id: "prior-usage", request_doc_id: "{}", request_id: "prior-request", node_did: "{}", backend_id: "test-backend", call_state: "completed", prompt_tokens: 100, completion_tokens: 0}}) {{_docID}}
        }}"#,
                    escape_graphql_string(&first_doc_id),
                    escape_graphql_string(&first_doc_id),
                    test_node_did(),
                );
                agent_access(&node)
                    .transact_write("test.finish_prior_assignment", |txn| {
                        let finish = &finish;
                        Box::pin(async move {
                            txn.execute(finish).await?;
                            Ok::<_, anyhow::Error>(())
                        })
                    })
                    .await
                    .unwrap();
                if stopped {
                    gents_automation::goal::set_goal_from_access(
                        &agent_access(&node),
                        &test_node_did(),
                        session,
                        None,
                        Some(gents_protocol::goal_status::GoalStatus::Complete),
                        None,
                        None,
                    )
                    .await
                    .unwrap();
                }
                assert!(second_lifecycle
                    .claim_pending_durable_with_inputs(|| now, || (now, "second".into()))
                    .await
                    .unwrap()
                    .was_claimed());
                let assigned = load_agent_goal(&node, session).await.unwrap().unwrap();
                assert_eq!(assigned.objective, "perform next assignment");
                assert_eq!(
                    assigned.assignment_root_request_doc_id.as_deref(),
                    Some(assigned_doc_id.as_str())
                );
                assert_eq!(assigned.token_budget, task_budget);
                assert_eq!(
                    assigned.parsed_status(),
                    Some(gents_protocol::goal_status::GoalStatus::Active)
                );
                assert_eq!(
                    assigned.continuation_sequence(),
                    original.continuation_sequence() + 1
                );
                assert_eq!(
                    assigned.token_usage_baseline.unwrap_or_default(),
                    if stopped { 100 } else { 0 }
                );
                assert_eq!(
                    gents_automation::goal::refresh_goal_usage_from_access(&agent_access(&node), &assigned)
                        .await
                        .unwrap(),
                    if stopped { 0 } else { 100 }
                );
                assert!(!gents_automation::goal::update_goal_fields_if_status_from_access(
                    &agent_access(&node),
                    &original,
                    gents_protocol::goal_status::GoalStatus::Active,
                    "objective: \"stale controller\""
                )
                .await
                .unwrap());
            }
        }
    }

    #[tokio::test]
    async fn concurrent_serial_claims_follow_durable_arrival_order() {
        let node = test_node().await;
        let first = insert_pending_request(
            &node,
            "z-first",
            "first-session",
            "2026-01-02T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        let second = insert_pending_request(
            &node,
            "a-second",
            "second-session",
            "2026-01-01T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        for request in [&first, &second] {
            let input = serde_json::json!({
                "fire_key": format!("test-fire:{}", request.request_id),
                "owner_did": test_node_did(),
                "trigger_id": "queued-serial-trigger",
                "source_collection": "Work",
                "source_doc_id": request.request_id,
                "request_id": request.request_id,
                "session_id": request.session_id,
                "queued_serial": true,
                "goal_assignment_applied": false,
            });
            agent_access(&node).transact_write(
                "test.serial_claim_receipt", |txn| {
                    let input = input.clone();
                    Box::pin(async move {
                        txn.execute_with_variables(
                            "mutation($input: TriggerFireMutationInputArg!) { create_TriggerFire(input: $input) { _docID } }",
                            &serde_json::json!({"input": input}),
                        ).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                },
            ).await.unwrap();
        }
        let first_doc_id = first.doc_id.clone();
        let second_doc_id = second.doc_id.clone();
        let mut first_lifecycle = test_lifecycle(
            node.clone(),
            first,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        let mut second_lifecycle = test_lifecycle(
            node.clone(),
            second,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        let now = chrono::Utc::now();
        let (second_result, first_result) = tokio::join!(
            second_lifecycle
                .claim_pending_durable_with_inputs(|| now, || (now, "second-generation".into())),
            first_lifecycle
                .claim_pending_durable_with_inputs(|| now, || (now, "first-generation".into())),
        );
        assert!(matches!(
            second_result.unwrap(),
            DurableClaimOutcome::NotClaimed(ClaimOutcome::Queued)
        ));
        assert!(first_result.unwrap().was_claimed());
        assert_eq!(
            durable_request_row(&node, &first_doc_id)
                .await
                .lifecycle_state,
            Some(RequestLifecycleState::Claimed)
        );
        assert_eq!(
            durable_request_row(&node, &second_doc_id)
                .await
                .lifecycle_state,
            Some(RequestLifecycleState::Pending)
        );
        assert!(matches!(
            second_lifecycle
                .claim_pending_durable_with_inputs(|| now, || (now, "second-generation".into()))
                .await
                .unwrap(),
            DurableClaimOutcome::NotClaimed(ClaimOutcome::Queued)
        ));
    }

    #[tokio::test]
    async fn claim_preserves_same_session_ordering() {
        let node = test_node().await;
        let first = insert_pending_request(
            &node,
            "same-session-request-1",
            "same-session",
            "2026-01-01T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        let first_doc_id = first.doc_id.clone();
        let refresh = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{first_doc_id}" }} }},
                    input: {{ content: "refreshed before claim" }}
                ) {{ _docID }}
            }}"#
        );
        let refresh_response = node.execute(&refresh).await;
        assert!(
            refresh_response.has_errors(),
            "immutable request content changed"
        );
        let second = insert_pending_request(
            &node,
            "same-session-request-2",
            "same-session",
            "2026-01-01T00:00:01Z",
            None,
            "interactive",
        )
        .await;

        let mut first_lifecycle = test_lifecycle(
            node.clone(),
            first,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        let mut second_lifecycle = test_lifecycle(
            node.clone(),
            second,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );

        assert_eq!(
            first_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            first_lifecycle.request().content,
            "same-session request",
            "claim must preserve immutable request semantics"
        );
        let claimed_cid = first_lifecycle
            .request_commit_cid()
            .expect("successful claim records its exact DefraDB commit")
            .to_string();

        let commits = gents_store::graphql::composite_commits(
            node.as_ref(),
            &first_doc_id,
            "verify_exact_claim_version",
        )
        .await
        .unwrap();
        assert_eq!(
            commits.first().map(|commit| commit.cid.as_str()),
            Some(claimed_cid.as_str()),
            "the recorded claim CID must remain the latest durable request version"
        );
        assert!(
            commits.iter().any(|commit| commit.cid == claimed_cid),
            "the recorded claim CID must be a real composite commit"
        );
        assert_eq!(
            second_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        );
    }

    /// Agent-authored background work: the managed Agent signs both its prior
    /// request and the local-control wake, as the runtime does.
    async fn signed_background_claim_fixture(
        node: &Arc<EmbeddedNode>,
        session: &str,
    ) -> (AgentRequest, String) {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestQueue};
        let access = root_access(node);
        let agent = agent_access(node);
        let identity = gents_automation::triggers::callback::test_agent_identity();
        let identity = identity.as_ref();
        let (node_did, agent_did) = (test_node_did(), test_agent_did());
        ensure_agent_session(node, session, &node_did, Some(&agent_did))
            .await
            .unwrap();
        let mut parent = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "snapshot-parent",
            &node_did,
            &agent_did,
            &agent_did,
            session,
            "Prior work",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(&agent_did),
        );
        gents_session::request::admission::sign_agent_request_create(identity, &mut parent)
            .await
            .unwrap();
        let parent_doc = create_fixture_request(&access, &parent.graphql_mutation().unwrap()).await;
        agent.write("test.snapshot_finished_parent", &format!("mutation {{update_AgentRequest(docID: \"{}\", input: {{lifecycle_state: \"completed\"}}) {{_docID}}}}",escape_graphql_string(&parent_doc))).await.unwrap();
        let mut wake = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "snapshot-wake",
            &node_did,
            &agent_did,
            &agent_did,
            session,
            "Review background results",
            "scheduled",
            "2026-10-21T00:00:01Z",
            AgentRequestAdmissionRecord::runtime_local_control(&agent_did, &parent.request_id),
        );
        wake.input.queue = Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session}")),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        });
        wake.caused_by_parent_request_id = Some(parent.request_id);
        wake.caused_by_parent_request_doc_id = Some(parent_doc.clone());
        gents_session::request::admission::sign_agent_request_create(identity, &mut wake)
            .await
            .unwrap();
        let doc = create_fixture_request(&access, &wake.graphql_mutation().unwrap()).await;
        let response = agent
            .execute(&format!(
                "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
                escape_graphql_string(&doc),
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
        (row.try_into().unwrap(), parent_doc)
    }

    /// Creates a signed request as its runtime owner and grants the request's
    /// working Agent, the relationship every request row carries in production.
    async fn create_fixture_request(
        access: &gents_store::config_client::ConfigAccess,
        mutation: &str,
    ) -> String {
        access
            .transact_write("test.fixture_signed_request", |txn| {
                Box::pin(async move {
                    let response = txn.execute(mutation).await?;
                    let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                    txn.add_actor_relationship(
                        "AgentRequest",
                        &doc,
                        "working_agent",
                        &identity::Did::new(test_agent_did())?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn background_claim_snapshots_transcript_before_successor_input() {
        let node = test_node().await;
        let (node_did, agent_did) = (test_node_did(), test_agent_did());
        let agent = agent_access(&node);
        let session_id = "background-claim-snapshot";
        let (request, source_doc) = signed_background_claim_fixture(&node, session_id).await;
        gents_session::session::import_history_observation_as(
            &agent,
            &source_doc,
            session_id,
            &node_did,
            Some(&agent_did),
            "first notification",
            "background-completion-notification:child-1:agent",
            1,
            None,
        )
        .await;
        gents_session::session::import_history_observation_as(
            &agent,
            &source_doc,
            session_id,
            &node_did,
            Some(&agent_did),
            "prior context",
            "prior-context",
            2,
            None,
        )
        .await;

        let request_doc_id = request.doc_id.clone();
        let mut lifecycle = test_lifecycle(
            node.clone(),
            request,
            60,
            ExecutionOrigin::Scheduled,
            TEST_BACKEND_ID,
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            lifecycle.background_completion_input_through_sequence(),
            Some(2)
        );
        let snapshot = agent
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                    background_completion_input_through_sequence
                    background_completion_notification_keys_json
                }} }}"#,
                escape_graphql_string(&request_doc_id)
            ))
            .await
            .unwrap();
        let row = &snapshot["data"]["AgentRequest"][0];
        assert_eq!(
            row["background_completion_input_through_sequence"].as_i64(),
            Some(2)
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(
                row["background_completion_notification_keys_json"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            vec!["background-completion-notification:child-1:agent"]
        );

        gents_session::session::import_history_observation_as(
            &agent,
            &source_doc,
            session_id,
            &node_did,
            Some(&agent_did),
            "successor notification",
            "background-completion-notification:child-2:agent",
            3,
            None,
        )
        .await;
        let history = session::load_sequenced_history_projection_on(
            &agent,
            session_id,
            &node_did,
            Some(&agent_did),
            lifecycle.background_completion_input_through_sequence(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            history.len(),
            2,
            "successor input must stay out of this attempt"
        );
    }

    #[tokio::test]
    async fn background_claim_keeps_fork_imports_out_of_notification_input() {
        let node = test_node().await;
        let (node_did, agent_did) = (test_node_did(), test_agent_did());
        let agent = agent_access(&node);
        let session_id = "background-import-snapshot";
        let origin_key = "background-completion-notification:owned:tool";
        let (request, source_doc) = signed_background_claim_fixture(&node, session_id).await;
        gents_session::session::import_history_observation_as(
            &agent,
            &source_doc,
            session_id,
            &node_did,
            Some(&agent_did),
            "notification",
            origin_key,
            1,
            None,
        )
        .await;
        let access = &agent;
        let response = access
            .execute(&format!(
                "{{ AgentMessage(filter: {{ message_key: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
                escape_graphql_string(origin_key),
                gents_protocol::output::rows::AGENT_MESSAGE_FIELDS
            ))
            .await
            .unwrap();
        let origin = gents_protocol::output::rows::decode_transcript_message_row(
            &response["data"]["AgentMessage"][0],
        )
        .unwrap();
        let mut imported = origin.message;
        imported.message_key = "background-completion-notification:imported:tool".to_owned();
        imported.request_doc_id = None;
        imported.publication = gents_protocol::output::MessagePublication::Fork {
            origin_message_doc_id: origin.doc_id,
        };
        imported.sequence = 2;
        access
            .transact_write("test.claim.import", |txn| {
                let imported = imported.clone();
                Box::pin(async move {
                    txn.execute_with_variables(
                        gents_protocol::output::rows::CREATE_AGENT_MESSAGE_MUTATION,
                        &gents_store::config_client::session_children::fork_message_bound_variables(
                            txn, &imported,
                        )
                        .await?,
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await
            .unwrap();
        let doc_id = request.doc_id.clone();
        let mut lifecycle = test_lifecycle(
            node.clone(),
            request,
            60,
            ExecutionOrigin::Scheduled,
            TEST_BACKEND_ID,
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        let response = access.execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ background_completion_notification_keys_json }} }}", escape_graphql_string(&doc_id))).await.unwrap();
        let keys: Vec<String> = serde_json::from_str(
            response["data"]["AgentRequest"][0]["background_completion_notification_keys_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let contract = gents_lean_vocab::lean_contract_snapshot()
            .wake_input_cases
            .iter()
            .find(|case| case.name == "imported_notification_stays_in_history")
            .unwrap();
        let expected = contract
            .current_keys
            .iter()
            .map(|key| {
                assert_eq!(key, "a");
                origin_key.to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(keys, expected);
        assert_eq!(
            lifecycle.background_completion_input_through_sequence(),
            Some(2)
        );
        drop(lifecycle);
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn claim_pins_configured_budget_and_reclaim_preserves_unlimited_or_zero() {
        for (index, configured, durable_override) in [
            (0, Some(50_u64), None),
            (1, None, None),
            (2, Some(1_u64), Some(0_i64)),
        ] {
            let node = test_node().await;
            let mut request = insert_pending_request(
                &node,
                &format!("budget-{index}"),
                &format!("budget-session-{index}"),
                "2026-01-01T00:00:00Z",
                None,
                "interactive",
            )
            .await;
            request.max_total_tokens = Some(999);
            let mut first = test_lifecycle(
                node.clone(),
                request,
                3600,
                ExecutionOrigin::Interactive,
                TEST_BACKEND_ID,
            );
            first.set_configured_max_total_tokens(configured);
            assert!(matches!(
                first.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ));
            assert_eq!(
                first.request().max_total_tokens,
                configured.map(|value| value as i64)
            );
            let mut resumed_request = first.request().clone();
            let expected = durable_override.or(resumed_request.max_total_tokens);
            let budget_patch = durable_override
                .map(|limit| format!(", max_total_tokens: {limit}"))
                .unwrap_or_default();
            let mutation = format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending"{budget_patch} }}) {{ _docID }} }}"#,
                escape_graphql_string(&resumed_request.doc_id)
            );
            agent_access(&node)
                .transact_write("test.requeue_budget", |txn| {
                    let mutation = &mutation;
                    Box::pin(async move {
                        txn.execute_local_response(mutation).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            resumed_request.max_total_tokens = expected;
            let mut resumed = test_lifecycle(
                node.clone(),
                resumed_request,
                3600,
                ExecutionOrigin::Interactive,
                TEST_BACKEND_ID,
            );
            resumed.set_configured_max_total_tokens(Some(10_000));
            assert!(matches!(
                resumed.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ));
            assert_eq!(resumed.request().max_total_tokens, expected);
        }
    }


    #[tokio::test]
    async fn pending_interrupt_and_expiry_release_workspace_bindings_atomically() {
        use crate::session_tests::admission_workspace_cleanup_tests::Fixture;

        for interrupted in [true, false] {
            let fixture = Fixture::new().await;
            let (_, mutations) =
                gents_store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
                    || async {
                        let mut lifecycle = fixture.lifecycle();
                        if interrupted {
                            lifecycle
                                .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                                .await
                        } else {
                            lifecycle.transition_pending_to_dead_stale().await
                        }
                    },
                    || fixture.observe(),
                )
                .await;
            assert!(
                mutations >= 2,
                "terminal and binding must share the transaction"
            );
            let observed = fixture.observe().await;
            assert_eq!(
                observed["AgentRequest"][0]["lifecycle_state"],
                if interrupted { "interrupted" } else { "dead" }
            );
            assert_eq!(
                observed["WorkspaceBinding"][0]["lifecycle_state"],
                "released"
            );

            fixture.write_binding(&fixture.binding).await;
            let before_replay = fixture.observe().await;
            let mut replay = fixture.lifecycle();
            if interrupted {
                replay
                    .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                    .await
                    .unwrap();
            } else {
                replay.transition_pending_to_dead_stale().await.unwrap();
            }
            assert_eq!(
                fixture.observe().await,
                before_replay,
                "replayed terminal cannot release again"
            );
        }
    }

    #[tokio::test]
    async fn pending_interrupt_and_expiry_cannot_release_a_claimed_workspace_binding() {
        use crate::session_tests::admission_workspace_cleanup_tests::Fixture;

        for interrupted in [true, false] {
            let fixture = Fixture::new().await;
            let doc_id = escape_graphql_string(&fixture.request.doc_id);
            fixture
                .access
                .write(
                    "test.pending_workspace_terminal_lost_cas",
                    &format!(
                        r#"mutation {{ update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ lifecycle_state: "claimed" }}
                ) {{ _docID }} }}"#
                    ),
                )
                .await
                .unwrap();
            let before = fixture.observe().await;
            let mut lifecycle = fixture.lifecycle();
            let result = if interrupted {
                lifecycle
                    .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                    .await
            } else {
                lifecycle.transition_pending_to_dead_stale().await
            };
            assert!(
                result.is_err(),
                "a claimed request lost the pending terminal CAS"
            );
            assert_eq!(fixture.observe().await, before);
        }
    }
