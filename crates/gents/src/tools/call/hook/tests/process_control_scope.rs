use super::*;

#[cfg(unix)]
#[tokio::test]
async fn unowned_live_process_group_keeps_the_retained_owner_record() {
    use crate::lean_vocab_test::lean_process_stop_cases;
    use crate::tools::exec::ownership::{ProcessIdentity, ProcessObservation, ProcessRecord};

    // The current test process proves the group is live; the impossible pid
    // models a recorded leader that exited or was reused. No signal is sent.
    let pgid = unsafe { libc::getpgid(0) };
    assert!(pgid > 0);
    let identity = ProcessIdentity {
        pid: i32::MAX,
        pgid,
        start: "gone-leader".to_owned(),
    };
    let observed = identity.observe();
    let case = lean_process_stop_cases()
        .iter()
        .find(|case| case.after == observed.as_str())
        .expect("Lean has a process observation case");
    assert_eq!(observed, ProcessObservation::Unowned);
    assert!(
        case.retain_record,
        "Lean requires unverified ownership to persist"
    );
    assert_eq!(observed.retain_process_record(), case.retain_record);

    let registry = BackgroundExecutionRegistry::default();
    registry
        .process_records
        .write(&ProcessRecord {
            tool_call_id: "stale-call".to_owned(),
            tool_call_doc_id: "stale-doc".to_owned(),
            identity,
        })
        .unwrap();
    registry
        .release_process_record("stale-call", "stale-doc")
        .await;
    assert!(registry.process_record("stale-call", "stale-doc").is_some());
    registry.forget_process_record("stale-call", "stale-doc");
    assert!(registry.process_record("stale-call", "stale-doc").is_some());
    registry.process_records.remove("stale-doc");
}
use crate::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::store::identity::NodeIdentity;

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".into(),
                description: "Held work for principal-scoped process controls".into(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn invoke(hook: &DefraSessionHook, id: &str, name: &str, args: &str) -> serde_json::Value {
    accept_hook_tool_call(hook, id, name, args, None).await;
    let ToolCallHookAction::Skip { reason } = hook.on_tool_call(name, None, id, args).await else {
        panic!("accepted {name} must return a tool result")
    };
    serde_json::from_str(&reason).expect("canonical process-control result JSON")
}

async fn finish_owned_request(hook: &DefraSessionHook, request_id: &str) {
    publish_claimed_provider_turn(
        hook,
        request_id,
        Message::Assistant {
            id: Some(format!("final-{request_id}")),
            content: vec![AssistantContent::Text(Text {
                text: "process controls complete".into(),
            })],
        },
    )
    .await;
    let request_doc_id = {
        let fixtures = hook_execution_fixtures().lock().await;
        fixtures
            .get(&hook_execution_fixture_key(hook, request_id))
            .expect("owned process-control request fixture")
            .lifecycle
            .request()
            .doc_id
            .clone()
    };
    let session_id = hook.session_id().await.expect("owned session");
    let node_did = hook.node_did.clone();
    let requester_did = hook.active_requester_did().await;
    let headers = crate::store::config_client::ConfigAccess::transact_local_write(
        &hook.node,
        None,
        "test.process_control_scope.headers",
        |txn| {
            let request_doc_id = request_doc_id.clone();
            let session_id = session_id.clone();
            let node_did = node_did.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                crate::session::load_request_headers_in_txn(
                    txn,
                    &session_id,
                    &node_did,
                    requester_did.as_deref(),
                    &request_doc_id,
                )
                .await
            })
        },
    )
    .await
    .expect("load exact canonical request headers");
    let selected = headers
        .iter()
        .filter(|header| {
            matches!(
                &header.message.role,
                gents_protocol::output::MessageRole::Assistant
            ) && matches!(
                &header.message.publication,
                gents_protocol::output::MessagePublication::RequestExecution { .. }
            )
        })
        .max_by_key(|header| header.message.sequence)
        .expect("owned request has an accepted assistant header")
        .doc_id
        .clone();
    let mut fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get_mut(&hook_execution_fixture_key(hook, request_id))
        .expect("owned process-control request fixture");
    fixture
        .lifecycle
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: selected,
            },
            None,
        )
        .await
        .expect("finish owned request through terminal owner");
}

#[tokio::test]
async fn generated_wait_observer_interrupt_preserves_background_process() {
    let case = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "caller_interrupt_observer_completes_wait_call_preserves_background_process",
    );
    assert!(case.legal);
    let dir = tempfile::tempdir().unwrap();
    let identity =
        crate::store::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None)
            .unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_agent(&node, identity.did(), "general").await;
    let executions = BackgroundExecutionRegistry::default();
    let hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_background_tool_registry(BackgroundToolRegistry::from_tools(
                vec![Box::new(PendingTool)],
                &["slow_tool".into()],
            ))
            .with_background_execution_registry(executions.clone());
    hook.on_completion_call(&user_text_message("observe background work"), &[])
        .await;
    let session_id = hook.session_id().await.unwrap();
    let request_id = "wait-observer-interrupt";
    let deadline = Utc::now() + chrono::Duration::minutes(5);
    bind_interruptible_request(&node, &hook, request_id, &session_id, deadline).await;
    let receipt = invoke(
        &hook,
        "observer-spawn",
        "spawn_process",
        r#"{"tool_name":"slow_tool","args":{}}"#,
    )
    .await;
    let tool_call_id = receipt["tool_call_id"]
        .as_str()
        .expect("spawned process handle");
    let args = json!({"tool_call_id": tool_call_id}).to_string();
    let wait_id = "observer-wait";
    accept_hook_tool_call(&hook, wait_id, "wait_process", &args, None).await;
    let waiting_hook = hook.clone();
    let wait = tokio::spawn(async move {
        waiting_hook
            .on_tool_call("wait_process", None, wait_id, &args)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !hook.in_flight_lifecycles.lock().await.contains_key(wait_id) {
            assert!(!wait.is_finished(), "wait must register before returning");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("wait owner registered");
    crate::request::interrupt::interrupt_request(&node, request_id)
        .await
        .unwrap();
    let action = tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .expect("observer returns on interrupt")
        .unwrap();
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("wait observer must publish its structured result: {action:?}");
    };
    let result: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(result["status"].as_str(), case.result.as_deref());
    assert_eq!(result["error"]["reason"].as_str(), case.reason.as_deref());
    assert_eq!(result["tool_call_id"], tool_call_id);
    let row = fetch_tool_call_row(&node, &session_id, tool_call_id).await;
    assert_eq!(row["lifecycle_state"].as_str(), case.result.as_deref());
    assert!(row["cancel_cause"].is_null());
    let wait_row = fetch_tool_call_row(&node, &session_id, wait_id).await;
    assert_eq!(wait_row["lifecycle_state"], case.terminal_state);
    let persisted = crate::tools::call::lifecycle::query::load_tool_call_result(
        &crate::store::config_client::ConfigAccess::local(node.clone()),
        wait_row["_docID"].as_str().expect("physical wait call"),
        &hook.node_did,
        &session_id,
        hook.active_requester_did().await.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        crate::tools::call::lifecycle::query::render_tool_result(&persisted).unwrap(),
        reason
    );
    crate::tools::call::control::cancel_session_background_process(
        node.clone(),
        &executions,
        identity.did(),
        hook.active_requester_did().await.as_deref(),
        &session_id,
        tool_call_id,
    )
    .await
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        executions.wait_for_completion(tool_call_id),
    )
    .await
    .expect("test background task cleaned up");
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&hook, request_id));
    node.shutdown().await;
}

#[tokio::test]
async fn accepted_cross_agent_process_controls_preserve_the_owners_running_job() {
    run_process_control_scope(None).await;
    run_process_control_scope(Some("did:key:shared-process-requester")).await;
}

async fn run_process_control_scope(requester_did: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let owner_identity =
        crate::store::identity::KeyIdentity::load_or_create(dir.path().join("owner.key"), None)
            .unwrap();
    let foreign_identity =
        crate::store::identity::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None)
            .unwrap();
    let owner_did = owner_identity.did();
    let foreign_did = foreign_identity.did();
    assert_ne!(owner_did, foreign_did);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(owner_did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for did in [owner_did, foreign_did] {
        crate::test_support::install_test_agent(&node, did, "general").await;
    }

    let executions = BackgroundExecutionRegistry::default();
    let owner = DefraSessionHook::with_identity(node.clone(), owner_did, FailurePolicy::default())
        .with_background_tool_registry(BackgroundToolRegistry::from_tools(
            vec![Box::new(PendingTool)],
            &["slow_tool".into()],
        ))
        .with_background_execution_registry(executions.clone());
    owner
        .on_completion_call(&user_text_message("run scoped background work"), &[])
        .await;
    let session_id = owner.session_id().await.unwrap();
    for did in [owner_did, foreign_did] {
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &node,
            &session_id,
            did,
            "general",
            requester_did,
        )
        .await
        .unwrap();
    }
    let foreign = DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &session_id,
        foreign_did,
        requester_did,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    let suffix = uuid::Uuid::new_v4();
    let owner_request = format!("process-control-owner-{suffix}");
    let foreign_request = format!("process-control-foreign-{suffix}");
    let owner_cancel_request = format!("process-control-owner-cancel-{suffix}");
    let deadline = Utc::now() + chrono::Duration::minutes(5);
    bind_interruptible_request_with_requester(
        &node,
        &owner,
        &owner_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    assert_eq!(
        owner.session_id().await.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(
        foreign.session_id().await.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(owner.active_requester_did().await.as_deref(), requester_did);

    let receipt = invoke(
        &owner,
        "cross-agent-spawn",
        "spawn_process",
        r#"{"tool_name":"slow_tool","args":{}}"#,
    )
    .await;
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"]
        .as_str()
        .expect("accepted spawned process ID")
        .to_owned();
    let args = json!({"tool_call_id":tool_call_id}).to_string();
    let sibling = invoke(
        &owner,
        "cross-agent-sibling-spawn",
        "spawn_process",
        r#"{"tool_name":"slow_tool","args":{}}"#,
    )
    .await;
    let sibling_id = sibling["tool_call_id"]
        .as_str()
        .expect("accepted sibling process ID")
        .to_owned();
    let row = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(row["request_id"], owner_request);
    assert_eq!(row["node_did"], owner_did);
    assert_eq!(row["requester_did"].as_str(), requester_did);
    assert_eq!(row["lifecycle_state"], "running");
    assert_eq!(row["await_mode"], "background");

    let owner_list = invoke(&owner, "owner-list", "list_processes", "{}").await;
    assert!(owner_list["entries"]
        .as_array()
        .expect("owner process entries")
        .iter()
        .any(|entry| entry["tool_call_id"] == tool_call_id));
    let owner_read = invoke(&owner, "owner-read", "read_process", &args).await;
    assert_eq!(owner_read["status"], "running");

    finish_owned_request(&owner, &owner_request).await;
    let after_owner_completion = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(after_owner_completion["lifecycle_state"], "running");
    bind_interruptible_request_with_requester(
        &node,
        &foreign,
        &foreign_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    assert_eq!(
        foreign.active_requester_did().await.as_deref(),
        requester_did
    );

    let foreign_list = invoke(&foreign, "foreign-list", "list_processes", "{}").await;
    assert!(!foreign_list["entries"]
        .as_array()
        .expect("foreign process entries")
        .iter()
        .any(|entry| entry["tool_call_id"] == tool_call_id));
    let foreign_read = invoke(&foreign, "foreign-read", "read_process", &args).await;
    assert_eq!(foreign_read["ok"], false);
    assert_eq!(foreign_read["failure_class"], "tool_not_allowed");
    let foreign_read_row = fetch_tool_call_row(&node, &session_id, "foreign-read").await;
    assert_eq!(foreign_read_row["request_id"], foreign_request);
    assert_eq!(foreign_read_row["node_did"], foreign_did);
    assert_eq!(foreign_read_row["requester_did"].as_str(), requester_did);
    for name in ["wait_process", "cancel_process"] {
        let denial = invoke(&foreign, &format!("foreign-{name}"), name, &args).await;
        assert_eq!(denial["ok"], false, "{name}: {denial}");
        assert!(
            denial["message"]
                .as_str()
                .is_some_and(|message| message.contains("not manageable by this session")),
            "{name}: {denial}"
        );
    }
    let denied = crate::tools::call::control::cancel_session_background_process(
        node.clone(),
        &BackgroundExecutionRegistry::default(),
        foreign_did,
        requester_did,
        &session_id,
        &tool_call_id,
    )
    .await
    .unwrap();
    assert!(matches!(
        denied,
        crate::tools::call::control::CancelBackgroundToolCallOutcome::NotFound
    ));
    let still_running = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(still_running["lifecycle_state"], "running");
    assert!(still_running["cancel_cause"].is_null());

    finish_owned_request(&foreign, &foreign_request).await;
    bind_interruptible_request_with_requester(
        &node,
        &owner,
        &owner_cancel_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    let cancelled = invoke(&owner, "owner-cancel", "cancel_process", &args).await;
    assert_eq!(cancelled["status"], "cancelled");
    executions.wait_for_completion(&tool_call_id).await;
    let final_row = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(final_row["lifecycle_state"], "cancelled");
    let sibling_row = fetch_tool_call_row(&node, &session_id, &sibling_id).await;
    assert_eq!(
        sibling_row["lifecycle_state"], "running",
        "cancelling one process must not stop its sibling"
    );
    let sibling_args = json!({"tool_call_id":sibling_id}).to_string();
    let sibling_cancelled = invoke(
        &owner,
        "owner-cancel-sibling",
        "cancel_process",
        &sibling_args,
    )
    .await;
    assert_eq!(sibling_cancelled["status"], "cancelled");
    executions.wait_for_completion(&sibling_id).await;
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&owner, &owner_request));
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&foreign, &foreign_request));
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&owner, &owner_cancel_request));
    node.shutdown().await;
}

mod job_control {
    use super::*;
    use crate::store::config_client::ConfigAccess;
    use crate::tools::background::job::admit::Refusal;
    use crate::tools::background::job::control::control;
    use crate::tools::background::job::driver::{settle_plugin_job, Settlement};
    use crate::tools::background::job::keys::job_spawn_key;
    use crate::tools::background::job::records::{
        write_pin_in_txn, write_progress_in_txn, write_step_state_in_txn, HostProgressFacts,
    };
    use crate::tools::background::status::ProcessControlScope;
    use crate::tools::call::control::{
        cancel_background_tool_call, cancel_session_background_process,
        CancelBackgroundToolCallOutcome,
    };
    use crate::tools::call::lifecycle::admission_fixture::{
        fixture_job_pin, publish_accepted_on_claimed_request,
    };
    use crate::tools::call::lifecycle::{AwaitMode, ToolCallLifecycle};
    use gents_protocol::job::{
        JobBudget, JobControlKind, JobControlOutcome, JobPin, JobProgress, JobReadError, JobState,
    };
    use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};

    struct JobHook {
        _dir: tempfile::TempDir,
        node: Arc<EmbeddedNode>,
        executions: BackgroundExecutionRegistry,
        owner: DefraSessionHook,
        session_id: String,
        request_id: String,
    }

    impl JobHook {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let identity = crate::store::identity::KeyIdentity::load_or_create(
                dir.path().join("owner.key"),
                None,
            )
            .unwrap();
            let node = Arc::new(
                EmbeddedNode::builder()
                    .data_path(dir.path())
                    .with_node_identity_did(identity.did())
                    .build()
                    .await
                    .unwrap(),
            );
            crate::ensure_runtime_schemas(&node).await.unwrap();
            crate::test_support::install_test_agent(&node, identity.did(), "general").await;
            let executions = BackgroundExecutionRegistry::default();
            let owner = DefraSessionHook::with_identity(
                node.clone(),
                identity.did(),
                FailurePolicy::default(),
            )
            .with_background_tool_registry(BackgroundToolRegistry::from_tools(
                vec![Box::new(PendingTool)],
                &["slow_tool".into()],
            ))
            .with_background_execution_registry(executions.clone());
            let session_id = owner.session_id().await.unwrap();
            let request_id = format!("job-control-{}", uuid::Uuid::new_v4());
            bind_signed_hook_request(
                &owner,
                &request_id,
                Utc::now() + chrono::Duration::minutes(5),
            )
            .await;
            owner
                .on_completion_call(&user_text_message("run job control work"), &[])
                .await;
            Self {
                _dir: dir,
                node,
                executions,
                owner,
                session_id,
                request_id,
            }
        }

        fn scope(&self, requester_did: Option<&str>) -> ProcessControlScope {
            ProcessControlScope {
                request_id: self.request_id.clone(),
                session_id: self.session_id.clone(),
                node_did: self.owner.node_did.clone(),
                requester_did: Some(requester_did.unwrap_or(&self.owner.node_did).to_owned()),
            }
        }

        fn access(&self) -> ConfigAccess {
            ConfigAccess::local(self.node.clone())
        }

        async fn load(&self, tool_call_id: &str) -> ToolCallLifecycle {
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, tool_call_id)
                .await
                .unwrap()
                .unwrap()
        }

        async fn stored(&self, tool_call_id: &str) -> serde_json::Value {
            let doc_id = self.load(tool_call_id).await.doc_id().unwrap().to_owned();
            self.access()
                .execute(&format!(
                    r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                    crate::store::graphql::escape_graphql_string(&doc_id),
                ))
                .await
                .unwrap()["data"]["AgentToolCall"][0]
                .clone()
        }

        async fn receipt(&self, tool_call_id: &str) -> PluginExecutionReceipt {
            serde_json::from_value(
                self.stored(tool_call_id).await["plugin_execution_receipt"].clone(),
            )
            .expect("a settled job row holds its receipt")
        }

        /// A running background job row published on the owner's bound request;
        /// it is its own chain root and holds one running progress record.
        async fn job_row(&self, tool_call_id: &str) -> String {
            let hook = &self.owner;
            let lifecycle = {
                let mut fixtures = hook_execution_fixtures().lock().await;
                let fixture = fixtures
                    .get_mut(&hook_execution_fixture_key(hook, &self.request_id))
                    .expect("owned job request fixture");
                let turn = fixture.turn;
                fixture.turn += 1;
                publish_accepted_on_claimed_request(
                    hook.node.clone(),
                    &mut fixture.lifecycle,
                    &hook.node_did,
                    turn,
                    "workflow",
                    tool_call_id,
                    json!({"argv": ["run"]}),
                    AwaitMode::Background,
                    true,
                )
                .await
                .unwrap()
            };
            let row = lifecycle.doc_id().unwrap().to_owned();
            ConfigAccess::transact_local_write(
                &hook.node,
                None,
                "test.process_control_scope.job_row",
                |txn| {
                    let row = &row;
                    Box::pin(async move {
                        let pin = JobPin {
                            chain_root_doc_id: row.clone(),
                            ..fixture_job_pin()
                        };
                        write_pin_in_txn(txn, row, &pin).await?;
                        let guest = JobProgress {
                            title: "audit".into(),
                            state: JobState::Running,
                            pause_reason: None,
                            phases: Vec::new(),
                            active: Vec::new(),
                            spawned: 0,
                            budget: JobBudget {
                                total: 0,
                                spent: 0,
                                remaining: 0,
                            },
                            last_event: "scanning".into(),
                            last_event_at: None,
                            question: None,
                            observed_terminal_children: 0,
                            revision: 0,
                        };
                        let host = HostProgressFacts {
                            spawned: 1,
                            budget: JobBudget {
                                total: 4,
                                spent: 1,
                                remaining: 3,
                            },
                            observed_terminal_children: 0,
                            state: JobState::Running,
                            pause_reason: None,
                        };
                        write_progress_in_txn(txn, row, 0, guest, host).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                },
            )
            .await
            .unwrap();
            // The published lifecycle predates the pin, so it is not a job row; the
            // reloaded one is.
            let mut job = self.load(tool_call_id).await;
            assert!(job.publish_background_receipt("started").await.unwrap());
            row
        }

        /// The evidence of the job step that ran, persisted on `row`.
        async fn persist_evidence(&self, row: &str) -> PluginExecutionReceipt {
            let evidence = evidence();
            ConfigAccess::transact_local_write(
                &self.node,
                None,
                "test.process_control_scope.step_state",
                |txn| {
                    let evidence = &evidence;
                    Box::pin(async move {
                        write_step_state_in_txn(txn, row, evidence, None, None).await
                    })
                },
            )
            .await
            .unwrap();
            evidence
        }

        /// A live local child the job spawned: a processing request of this node.
        async fn live_child(&self, row: &str) -> String {
            crate::tools::background::job::input::tests::insert(
                &self.node,
                "AgentRequest",
                json!({
                    "request_id": "child-slot-a",
                    "purpose": "normal",
                    "node_did": self.owner.node_did,
                    "requester_did": self.owner.node_did,
                    "session_id": "child-session-slot-a",
                    "agent_id": "general",
                    "lifecycle_state": "processing",
                    "retry_key": job_spawn_key(row, "slot-a", 0),
                    "created_at": "2026-01-01T00:00:00Z",
                    "caused_by_parent_tool_call_doc_id": row,
                    "caused_by_parent_tool_call_id": "call",
                    "caused_by_parent_request_id": "parent",
                    "caused_by_parent_request_doc_id": "parent-doc",
                }),
            )
            .await
        }

        async fn interrupted(&self, child: &str) -> bool {
            crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(&self.node, child)
                .await
                .unwrap()
                .is_some()
        }

        async fn settle(&self, tool_call_id: &str, settlement: Settlement) {
            let mut lifecycle = self.load(tool_call_id).await;
            settle_plugin_job(&mut lifecycle, settlement).await.unwrap();
        }

        async fn finish(self) {
            hook_execution_fixtures()
                .lock()
                .await
                .remove(&hook_execution_fixture_key(&self.owner, &self.request_id));
            self.node.shutdown().await;
        }
    }

    fn evidence() -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "fixture/engine".into(),
            artifact_digest: "sha256:engine".into(),
            input_digest: "step".into(),
            output_digest: Some("sha256:output".into()),
            authority: None,
            limits: None,
            verdict: PluginExecutionVerdict::Success,
        }
    }

    /// What a cancelled row publishes of its step evidence (Lean `PluginReceipt.published`).
    fn cancelled(evidence: PluginExecutionReceipt) -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            output_digest: None,
            verdict: PluginExecutionVerdict::ExecutionError,
            ..evidence
        }
    }

    #[tokio::test]
    async fn model_cancel_keeps_running_when_native_stop_is_unverified() {
        let expected = crate::lean_vocab_test::lean_process_stop_cases()
            .iter()
            .find(|case| case.live_evidence == "spawned_unverified" && case.worker_released)
            .unwrap();
        let hook = JobHook::new().await;
        let lifecycle = {
            let mut fixtures = hook_execution_fixtures().lock().await;
            let fixture = fixtures
                .get_mut(&hook_execution_fixture_key(&hook.owner, &hook.request_id))
                .unwrap();
            let turn = fixture.turn;
            fixture.turn += 1;
            publish_accepted_on_claimed_request(
                hook.node.clone(),
                &mut fixture.lifecycle,
                &hook.owner.node_did,
                turn,
                "bash",
                "unverified-native",
                json!({}),
                AwaitMode::Background,
                true,
            )
            .await
            .unwrap()
        };
        let doc = lifecycle.doc_id().unwrap().to_owned();
        let token = tokio_util::sync::CancellationToken::new();
        let reservation = hook
            .executions
            .reserve("unverified-native".into(), doc, token.clone())
            .unwrap();
        let owner = hook.owner.clone();
        let mut control = tokio::spawn(async move {
            invoke(
                &owner,
                "model-unverified",
                "cancel_process",
                r#"{"tool_call_id":"unverified-native"}"#,
            )
            .await
        });
        tokio::select! {
            result = &mut control => panic!("model control ended before cancellation: {result:?}"),
            _ = token.cancelled() => {}
        }
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(10)).await;
        tokio::time::resume();
        let result = control.await.unwrap();
        assert_eq!(result["status"], "unverified");
        assert_eq!(
            hook.stored("unverified-native").await["lifecycle_state"],
            expected.durable_state
        );
        drop(reservation);
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_stop_is_applied_with_killed_children() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-stop").await;
        let evidence = hook.persist_evidence(&row).await;
        let child = hook.live_child(&row).await;
        let outcome = control(
            &hook.access(),
            &hook.scope(None),
            "job-stop",
            JobControlKind::Stop,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            JobControlOutcome::Applied {
                state: JobState::Cancelled,
                killed_children: 1
            }
        );
        assert!(hook.interrupted(&child).await);
        let stored = hook.stored("job-stop").await;
        assert_eq!(stored["lifecycle_state"], "cancelled");
        assert_eq!(hook.receipt("job-stop").await, cancelled(evidence));
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_pause_is_applied_paused() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-pause").await;
        hook.persist_evidence(&row).await;
        let outcome = control(
            &hook.access(),
            &hook.scope(None),
            "job-pause",
            JobControlKind::Pause,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            JobControlOutcome::Applied {
                state: JobState::Paused,
                killed_children: 0
            }
        );
        assert_eq!(
            hook.stored("job-pause").await["lifecycle_state"],
            "completed"
        );
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_on_a_terminal_row_is_already_terminal() {
        let hook = JobHook::new().await;
        hook.job_row("job-done").await;
        hook.settle("job-done", Settlement::Result(json!({"findings": 3})))
            .await;
        let outcome = control(
            &hook.access(),
            &hook.scope(None),
            "job-done",
            JobControlKind::Stop,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            JobControlOutcome::AlreadyTerminal {
                state: JobState::Completed
            }
        );
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_on_a_non_job_row_is_not_a_job() {
        let hook = JobHook::new().await;
        let receipt = invoke(
            &hook.owner,
            "plain-spawn",
            "spawn_process",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await;
        let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_owned();
        let outcome = control(
            &hook.access(),
            &hook.scope(None),
            &tool_call_id,
            JobControlKind::Stop,
        )
        .await;
        assert_eq!(outcome, Err(JobReadError::NotAJob));
        let args = json!({"tool_call_id": tool_call_id}).to_string();
        invoke(&hook.owner, "plain-cancel", "cancel_process", &args).await;
        hook.executions.wait_for_completion(&tool_call_id).await;
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_on_a_missing_row_is_not_yet_observable() {
        let hook = JobHook::new().await;
        let outcome = control(
            &hook.access(),
            &hook.scope(None),
            "no-such-call",
            JobControlKind::Stop,
        )
        .await;
        assert_eq!(outcome, Err(JobReadError::NotYetObservable));
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_outside_scope_is_not_authorized() {
        let hook = JobHook::new().await;
        hook.job_row("job-scoped").await;
        let outcome = control(
            &hook.access(),
            &hook.scope(Some("did:key:another-requester")),
            "job-scoped",
            JobControlKind::Stop,
        )
        .await;
        assert_eq!(outcome, Err(JobReadError::NotAuthorized));
        assert_eq!(
            hook.stored("job-scoped").await["lifecycle_state"],
            "running"
        );
        hook.finish().await;
    }

    #[tokio::test]
    async fn job_control_over_graphql_access_is_unavailable_naming_the_in_session_routes() {
        let hook = JobHook::new().await;
        let outcome = control(
            &ConfigAccess::graphql("http://127.0.0.1:1"),
            &hook.scope(None),
            "job-remote",
            JobControlKind::Stop,
        )
        .await;
        let Err(JobReadError::Unavailable { message }) = outcome else {
            panic!("graphql access must be unavailable: {outcome:?}")
        };
        assert!(message.contains("workflow stop"), "{message}");
        hook.finish().await;
    }

    #[tokio::test]
    async fn operator_cancel_on_a_job_row_ends_it_cancelled() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-operator").await;
        let evidence = hook.persist_evidence(&row).await;
        let child = hook.live_child(&row).await;
        let outcome = cancel_background_tool_call(
            hook.node.clone(),
            &hook.executions,
            &hook.session_id,
            "job-operator",
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: false
            }
        );
        assert_eq!(
            hook.stored("job-operator").await["lifecycle_state"],
            "cancelled"
        );
        assert_eq!(hook.receipt("job-operator").await, cancelled(evidence));
        assert!(hook.interrupted(&child).await);
        hook.finish().await;
    }

    #[tokio::test]
    async fn client_cancel_on_a_job_row_ends_it_cancelled() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-client").await;
        let evidence = hook.persist_evidence(&row).await;
        let child = hook.live_child(&row).await;
        let denied = cancel_session_background_process(
            hook.node.clone(),
            &hook.executions,
            &hook.owner.node_did,
            Some("did:key:another-requester"),
            &hook.session_id,
            "job-client",
        )
        .await
        .unwrap();
        assert_eq!(denied, CancelBackgroundToolCallOutcome::NotFound);
        assert_eq!(
            hook.stored("job-client").await["lifecycle_state"],
            "running"
        );
        let outcome = cancel_session_background_process(
            hook.node.clone(),
            &hook.executions,
            &hook.owner.node_did,
            Some(&hook.owner.node_did),
            &hook.session_id,
            "job-client",
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: false
            }
        );
        assert_eq!(
            hook.stored("job-client").await["lifecycle_state"],
            "cancelled"
        );
        assert_eq!(hook.receipt("job-client").await, cancelled(evidence));
        assert!(hook.interrupted(&child).await);
        hook.finish().await;
    }

    #[tokio::test]
    async fn cancel_on_a_settled_job_row_reports_the_lifecycle_spelling() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-late").await;
        hook.persist_evidence(&row).await;
        hook.settle("job-late", Settlement::Refused(Refusal::TimedOut))
            .await;
        let outcome = cancel_background_tool_call(
            hook.node.clone(),
            &hook.executions,
            &hook.session_id,
            "job-late",
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::AlreadyTerminal {
                state: "timedOut".into()
            }
        );
        hook.finish().await;
    }

    #[tokio::test]
    async fn model_cancel_process_on_a_job_row_replies_cancelled() {
        let hook = JobHook::new().await;
        let row = hook.job_row("job-model").await;
        let evidence = hook.persist_evidence(&row).await;
        let args = json!({"tool_call_id": "job-model"}).to_string();
        let reply = invoke(&hook.owner, "model-cancel", "cancel_process", &args).await;
        assert_eq!(reply["status"], "cancelled", "{reply}");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            hook.stored("job-model").await["lifecycle_state"],
            "cancelled"
        );
        assert_eq!(hook.receipt("job-model").await, cancelled(evidence));
        hook.finish().await;
    }
}
