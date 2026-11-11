use anyhow::Result;
use serde_json::json;
use gents_documents::AgentToolConfig;
use gents_protocol::message::{AssistantContent, Message};
use gents_session::session::message::inspection::*;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

    /// The Goal the Goals plugin reports for the inspected session.
    struct ReadWorkerGoal;

    #[tokio::test]
    async fn trigger_worker_inspection_reads_canonical_output_status_and_goal() {
        use gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
        use gents_protocol::tool_call::AwaitMode;
        use gents_store::identity::NodeIdentity;
        let (message, caller, _) =
            super::message_agent_tools_tests::started("agent-session-inspection").await;
        let node = message.admission.node.clone();
        let identity = gents_store::identity::KeyIdentity::load_existing(
            message.admission.path.join("node.key"),
            None,
        )
        .unwrap();
        let caller_access = message.admission.tool.access().unwrap();
        let signer = caller_access.managed_agent_signer().unwrap();
        caller_access
            .transact_write("test.inspection.worker_session", |txn| {
                Box::pin(gents_session::session::ensure_session_in_txn(
                    txn,
                    "read-worker",
                    identity.did(),
                    signer.did(),
                    Some(signer.did()),
                    None,
                    None,
                    "2026-11-04T00:00:00Z",
                ))
            })
            .await
            .unwrap();
        let mut opening = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "read-worker-request",
            identity.did(),
            signer.did(),
            signer.did(),
            "read-worker",
            "spawn",
            "interactive",
            "2026-11-04T00:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                signer.did(),
            ),
        );
        opening.caused_by_trigger_id = Some("shard-assignment".into());
        opening.caused_by_trigger_kind = Some("schedule".into());
        opening.caused_by_correlation = Some("R3a".into());
        gents_session::request::admission::sign_agent_request_create(&signer, &mut opening)
            .await
            .unwrap();
        let created = caller_access
            .write(
                "test.inspection.worker_request",
                &opening.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let request_doc = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
        let escaped = escape_graphql_string(&request_doc);
        let row = caller_access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{_docID: {{_eq: "{escaped}"}}}}) {{{}}}}}"#,
                gents_protocol::row::AGENT_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row["data"]["AgentRequest"][0].clone()).unwrap();
        let mut worker = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
            node.clone(),
            row.try_into().unwrap(),
            60,
        );
        worker.bind_managed_agent(&signer).unwrap();
        assert_eq!(
            worker.claim().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        let mut tool = publish_accepted_on_claimed_request(
            node.clone(),
            &mut worker,
            identity.did(),
            0,
            "bash",
            "worker-check",
            json!({"command": "private tool arguments"}),
            AwaitMode::Foreground,
            true,
        )
        .await
        .unwrap();
        let tools = AgentToolConfig {
            enabled: true,
            targets: Vec::new(),
        };
        let mut queued = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "read-worker-steering",
            identity.did(),
            signer.did(),
            signer.did(),
            "read-worker",
            "continue after verification",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                signer.did(),
            ),
        );
        queued.input.queue = Some(gents_protocol::request_input::RequestQueue {
            delivery: gents_protocol::request_input::QueueDelivery::Steer,
            position: None,
            source: gents_protocol::request_input::QueueSource::Steering,
            policy: gents_protocol::request_input::QueuePolicy::Append,
            key: None,
            queued_after_request_id: Some(worker.request().request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        gents_session::request::admission::sign_agent_request_create(&signer, &mut queued)
            .await
            .unwrap();
        caller_access
            .write(
                "test.inspection.steering",
                &queued.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let mut args = AgentSessionReadArgs {
            session_id: "read-worker".to_owned(),
            view: ReadView::Status,
            ..Default::default()
        };
        let status = agent_session_read(&node, &ReadWorkerGoal, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(status["request"]["request_id"], worker.request().request_id);
        assert_eq!(status["request"]["lifecycle_state"], "processing");
        assert_eq!(status["request"]["causal_hop"], 0);
        // Status read adds only the read-start timestamp; the session's
        // Goal is exposed as-is.
        assert_eq!(
            status["goal"],
            json!({"goal_id": "read-worker-goal", "status": "active", "objective": "verify shard R3a", "objective_truncated": false, "tokens_used": 0, "token_budget": 1000, "active_time_seconds": 0})
        );
        assert!(status["observation_started_at"].is_string());
        chrono::DateTime::parse_from_rfc3339(status["observation_started_at"].as_str().unwrap())
            .unwrap();
        assert_eq!(status["tools_in_flight"]["items"][0]["name"], "bash");
        assert_eq!(status["tools_in_flight"]["items"][0]["status"], "running");
        assert!(
            status["tools_in_flight"]["items"][0]["elapsed_seconds"]
                .as_i64()
                .unwrap()
                >= 0
        );
        assert_eq!(
            status["queued_steering"]["items"][0]["request_id"],
            "read-worker-steering"
        );
        assert_eq!(
            status["queued_steering"]["items"][0]["queued_after_request_id"],
            worker.request().request_id
        );
        assert!(!status.to_string().contains("private tool arguments"));
        tool.complete(&"界".repeat(1000)).await.unwrap();
        let writer = gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
            node.clone(),
            &worker,
            std::time::Duration::ZERO,
        )
        .unwrap();
        writer
            .start_provider_attempt(
                &worker.request().doc_id,
                1,
                0,
                "inference.2".parse().unwrap(),
            )
            .await;
        let report = Message::Assistant {
            request_id: None,
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning {
                    id: Some("private provider id".to_owned()),
                    content: vec![gents_protocol::message::ReasoningContent::Encrypted(
                        "private encrypted reasoning".to_owned(),
                    )],
                }),
                AssistantContent::Text(gents_protocol::message::Text {
                    text: "shard R3a verified".to_owned(),
                }),
            ],
        };
        let published = writer
            .publish_native_turn(&worker, 1, 0, &report)
            .await
            .unwrap();
        worker
            .terminalize_owned(
                gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id,
                },
                None,
            )
            .await
            .unwrap();
        args.view = ReadView::Last;
        let last = agent_session_read(&node, &ReadWorkerGoal, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(last["messages"]["items"][0]["text"], "shard R3a verified");
        assert_eq!(last["request"]["request_id"], "read-worker-steering");
        assert_eq!(last["request"]["lifecycle_state"], "pending");
        assert!(last["request"]["created_at"].is_string());
        // The last view carries messages only; it does not add status fields.
        assert!(last.get("observation_started_at").is_none());
        assert!(!last.to_string().contains("private encrypted reasoning"));
        assert!(!last.to_string().contains("private provider id"));
        args.view = ReadView::Recent;
        args.limit = Some(1);
        let mut messages = Vec::new();
        loop {
            let page = agent_session_read(&node, &ReadWorkerGoal, &caller, &tools, &args)
                .await
                .unwrap();
            assert!(!page.to_string().contains("private tool arguments"));
            assert!(!page.to_string().contains("private encrypted reasoning"));
            messages.extend(page["messages"]["items"].as_array().unwrap().clone());
            args.cursor = page["messages"]["next_cursor"]
                .as_str()
                .map(ToOwned::to_owned);
            if args.cursor.is_none() {
                break;
            }
        }
        assert!(messages.iter().any(|row| row["tools"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "bash" && tool["status"] == "completed")));
        assert!(messages
            .iter()
            .any(|row| row["tools"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |tool| tool["result_preview"] == "界".repeat(TOOL_PREVIEW_CHARS)
                        && tool["preview_truncated"] == true
                )));
        let result_message = messages
            .iter()
            .find(|row| row["role"] == "TOOL")
            .expect("canonical tool result remains inspectable");
        assert_eq!(
            result_message["text"], "",
            "full tool output must not appear in the message body"
        );
        let scope = gents_protocol::session::session_scope_filter(
            identity.did(),
            "read-worker",
            Some(signer.did()),
        );
        let observation = gents_protocol::session::SessionObservation {
            reply_preview: None,
            last_activity_at: chrono::Utc::now().to_rfc3339(),
            preview: None,
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: "unobserved-request-document".to_owned(),
                request_id: "unobserved-request".to_owned(),
                lifecycle_state:
                    gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
            }),
        };
        caller_access.transact_write("test.inspection.index_only", |txn| {
            let scope = scope.clone();
            let observation = observation.clone();
            Box::pin(async move { txn.execute_with_variables(&format!("mutation($input: AgentSessionMutationInputArg!) {{update_AgentSession(filter: {{{scope}}}, input: $input) {{_docID}}}}"), &json!({"input": {"observation": observation}})).await })
        }).await.unwrap();
        args.view = ReadView::Status;
        let unavailable = agent_session_read(&node, &ReadWorkerGoal, &caller, &tools, &args)
            .await
            .unwrap();
        assert_eq!(unavailable["request"]["request_id"], "unobserved-request");
        assert_eq!(unavailable["request"]["available"], false);
        assert!(unavailable["observation_started_at"].is_string());
        args.session_id = caller.session_id.clone();
        assert!(
            agent_session_read(&node, &ReadWorkerGoal, &caller, &tools, &args)
                .await
                .is_err()
        );
        let foreign_identity = gents_store::identity::KeyIdentity::load_or_create(
            message.admission.path.join("other-principal.key"),
            None,
        )
        .unwrap();
        let foreign = gents_protocol::row::AgentRequest {
            node_did: foreign_identity.did().to_owned(),
            ..caller.clone()
        };
        args.session_id = "read-worker".to_owned();
        let error = agent_session_read(&node, &ReadWorkerGoal, &foreign, &tools, &args)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not messageable"), "{error:#}");
        drop(worker);
        node.shutdown().await.expect("shut down test node");
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[async_trait::async_trait]
    impl gents_session::session::message::SessionMessageGoals for ReadWorkerGoal {
        async fn session_goal_in_txn(
            &self,
            _txn: &ConfigApplyTxn<'_>,
            _node_did: &str,
            session_id: &str,
        ) -> Result<Option<gents_session::session::message::SessionGoal>> {
            Ok(
                (session_id == "read-worker").then(|| gents_session::session::message::SessionGoal {
                    goal_id: "read-worker-goal".to_owned(),
                    status: "active".to_owned(),
                    objective: "verify shard R3a".to_owned(),
                    tokens_used: Some(0),
                    token_budget: Some(1000),
                    active_time_seconds: Some(0),
                    assignment_root_request_doc_id: None,
                }),
            )
        }

        async fn stage_goal_backed_request_in_txn(
            &self,
            _txn: &ConfigApplyTxn<'_>,
            _node_did: &str,
            _session_id: &str,
            _objective: &str,
            _token_budget: Option<i64>,
            _request: &gents_protocol::request_admission::AgentRequestCreate,
        ) -> Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
            anyhow::bail!("session inspection stages no request")
        }

        async fn prepare_goal_input_in_txn(
            &self,
            _txn: &ConfigApplyTxn<'_>,
            _node_did: &str,
            _session_id: &str,
            _agent_did: &str,
            _requester_did: Option<&str>,
            _input: gents_protocol::request_input::RequestInput,
        ) -> Result<gents_protocol::request_input::RequestInput> {
            anyhow::bail!("session inspection prepares no request input")
        }

        async fn request_assignment(
            &self,
            _access: &gents_store::config_client::ConfigAccess,
            _request: &gents_protocol::row::AgentRequestRow,
        ) -> Result<gents_session::session::message::RequestGoalAssignment> {
            anyhow::bail!("session inspection observes no request assignment")
        }
    }
