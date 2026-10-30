use super::*;
use crate::store::identity::NodeIdentity;

#[cfg(unix)]
#[tokio::test]
async fn cancelling_while_approval_waits_never_requires_a_process_record() {
    use crate::plugin::approval::workspace;
    if !crate::toolset::workspace_write_sandbox_enforced() {
        return;
    }
    let expected = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.live_evidence == "never_spawned" && case.worker_released)
        .unwrap();
    let mut harness = admission_harness(|_, root| {
        BackgroundToolRegistry::from_tools(
            vec![crate::toolset::write_capable_bash_for_test(root)],
            &["bash_unrestricted".into()],
        )
    })
    .await;
    let executions = BackgroundExecutionRegistry::default();
    harness.hook = harness
        .hook
        .with_background_execution_registry(executions.clone());
    let root = harness.root.path().canonicalize().unwrap();
    let key = workspace::WorkspaceKey::canonical(
        &harness.hook.node_did,
        &harness.hook.node_did,
        "general",
        &root,
    )
    .unwrap();
    let selection = workspace::ReviewTaskSelection {
        task_id: "review-cancel".into(),
        policy_digest: "fixture-policy".into(),
    };
    workspace::save_mode(
        harness._dir.path(),
        &key,
        0,
        workspace::Mode::AutoReview,
        Some(&selection),
    )
    .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let context = workspace::for_request(
        Some(harness._dir.path()),
        &key.node_did,
        Some(&key.requester_did),
        &key.agent_id,
        Some(&root),
    )
    .unwrap()
    .unwrap()
    .with_reviewer(Arc::new(move |_| {
        let signal = signal.clone();
        Box::pin(async move {
            signal.notify_one();
            std::future::pending().await
        })
    }));
    let args = json!({"tool_name":"bash_unrestricted", "args":{"command":"/bin/sh", "args":["-c", "exit 0"]}}).to_string();
    accept_hook_tool_call(&harness.hook, "approval-stop", "spawn_process", &args, None).await;
    let action = workspace::scope(
        Some(context),
        harness
            .hook
            .on_tool_call("spawn_process", None, "approval-stop", &args),
    )
    .await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("{action:?}")
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    let child = receipt["tool_call_id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(60), entered.notified())
        .await
        .expect("approval was not reached");
    let session = harness.hook.session_id().await.unwrap();
    let row = fetch_tool_call_row(&harness.node, &session, child).await;
    let request = gents_protocol::process_control::OperatorProcessControlRequest {
        node_did: key.node_did.clone(),
        session_id: session.clone(),
        tool_call_id: child.to_owned(),
        tool_call_doc_id: row["_docID"].as_str().unwrap().to_owned(),
    };
    let outcome = crate::tool_control::operator_process_control(
        harness.node.clone(),
        &executions,
        Some(&key.node_did),
        &request,
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        crate::tool_control::CancelBackgroundToolCallOutcome::Cancelled {
            live_execution_cancelled: true
        } | crate::tool_control::CancelBackgroundToolCallOutcome::AlreadyTerminal { .. }
    ));
    let row = fetch_tool_call_row(&harness.node, &session, child).await;
    assert_eq!(row["lifecycle_state"], expected.durable_state);
    assert!(executions
        .process_record(child, &request.tool_call_doc_id)
        .is_none());
    crate::tool_call_lifecycle::ToolCallLifecycle::reconcile_orphaned_background_tools(
        &harness.node,
        &key.node_did,
        &executions,
    )
    .await
    .unwrap();
    assert_eq!(
        fetch_tool_call_row(&harness.node, &session, child).await["lifecycle_state"],
        expected.durable_state
    );
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(
            &harness.hook,
            &harness.request_id,
        ));
    harness.node.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn background_commands_keep_workspace_consent_after_dispatch() {
    use crate::plugin::approval::workspace;

    for (mode, reviewer_available, revoke) in [
        (workspace::Mode::Ask, false, false),
        (workspace::Mode::FullAccess, false, false),
        (workspace::Mode::AutoReview, false, false),
        (workspace::Mode::AutoReview, true, false),
        (workspace::Mode::AutoReview, true, true),
    ] {
        let model_mode = serde_json::to_value(mode).unwrap();
        let review = if reviewer_available {
            "allow"
        } else {
            "unavailable"
        };
        let case = crate::lean_vocab_test::lean_contract_snapshot()
            .workspace_permission_cases
            .iter()
            .find(|case| {
                let input = &case["input"];
                input["mode"] == model_mode
                    && input["ceilingAllows"] == true
                    && input["crossesBoundary"] == true
                    && input["grant"].is_null()
                    && input["configuredWorkspace"] == input["current"]["workspace"]
                    && (mode != workspace::Mode::AutoReview
                        || (case["review"] == review
                            && case["reviewedGrant"]["scope"]["workspace"]
                                == input["current"]["workspace"]
                            && (case["reviewedGrant"]["scope"]["revision"]
                                == input["current"]["revision"])
                                != revoke
                            && case["reviewedGrant"]["action"] == input["action"]))
            })
            .expect("missing executable permission case");
        let expected = if mode == workspace::Mode::AutoReview {
            &case["expectedReviewedRoute"]
        } else {
            &case["expectedRoute"]
        };
        let supported = crate::toolset::workspace_write_sandbox_enforced();
        let guard_case = crate::lean_vocab_test::lean_contract_snapshot()
            .operator_path_cases
            .iter()
            .find(|case| {
                case["kind"] == "commandGuard"
                    && case["command"]["required"] == true
                    && case["command"]["supported"] == supported
                    && case["command"]["inspected"] == true
                    && case["command"]["singleLinked"] == true
            })
            .expect("missing command isolation case");
        let allowed = expected == "allow" && guard_case["expected"] == true;
        let mut harness = admission_harness(|_, root| {
            BackgroundToolRegistry::from_tools(
                vec![crate::toolset::write_capable_bash_for_test(root)],
                &["bash_unrestricted".into()],
            )
        })
        .await;
        let executions = BackgroundExecutionRegistry::default();
        harness.hook = harness
            .hook
            .with_background_execution_registry(executions.clone());
        let root = harness.root.path().canonicalize().unwrap();
        let key = workspace::WorkspaceKey::canonical(
            &harness.hook.node_did,
            &harness.hook.node_did,
            "general",
            &root,
        )
        .unwrap();
        let selection = workspace::ReviewTaskSelection {
            task_id: "review-command".into(),
            policy_digest: "fixture-policy".into(),
        };
        workspace::save_mode(
            harness._dir.path(),
            &key,
            0,
            mode,
            (mode == workspace::Mode::AutoReview).then_some(&selection),
        )
        .unwrap();
        let mut context = workspace::for_request(
            Some(harness._dir.path()),
            &key.node_did,
            Some(&key.requester_did),
            &key.agent_id,
            Some(&root),
        )
        .unwrap()
        .unwrap();
        let reviews = Arc::new(AtomicU64::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        if reviewer_available {
            let reviews = reviews.clone();
            let entered = entered.clone();
            let release = release.clone();
            context = context.with_reviewer(Arc::new(move |invocation| {
                reviews.fetch_add(1, Ordering::SeqCst);
                let entered = entered.clone();
                let release = release.clone();
                Box::pin(async move {
                    if revoke {
                        entered.notify_one();
                        release.notified().await;
                    }
                    Ok(Some(
                        json!({
                            "actionId": invocation.action_id, "decision": "allow",
                            "reason": "The command writes only the requested fixture marker.",
                        })
                        .to_string(),
                    ))
                })
            }));
        }
        let marker = root.join("approved-marker");
        let args = json!({"tool_name": "bash_unrestricted", "args": {
            "command": "/bin/sh",
            "args": ["-c", "printf approved > \"$1\"", "fixture", marker],
        }})
        .to_string();
        accept_hook_tool_call(&harness.hook, "spawn-command", "spawn_process", &args, None).await;
        let action = workspace::scope(
            Some(context),
            harness
                .hook
                .on_tool_call("spawn_process", None, "spawn-command", &args),
        )
        .await;
        let ToolCallHookAction::Skip { reason } = action else {
            panic!("{action:?}")
        };
        let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
        let child = receipt["tool_call_id"].as_str().expect("background handle");
        if revoke && supported {
            tokio::time::timeout(std::time::Duration::from_secs(60), entered.notified())
                .await
                .expect("background review did not start");
            workspace::save_mode(harness._dir.path(), &key, 1, workspace::Mode::Ask, None).unwrap();
            release.notify_one();
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            executions.wait_for_completion(child),
        )
        .await
        .expect("background command did not settle");
        let session = harness.hook.session_id().await.unwrap();
        let row = fetch_tool_call_row(&harness.node, &session, child).await;
        assert_eq!(
            row["lifecycle_state"],
            if allowed { "completed" } else { "failed" },
            "{mode:?}: {row}"
        );
        assert_eq!(marker.exists(), allowed, "{mode:?}: {row}");
        if allowed {
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "approved");
        }
        assert_eq!(
            reviews.load(Ordering::SeqCst),
            u64::from(reviewer_available && supported)
        );
        assert!(crate::plugin::approval::pending(harness._dir.path())
            .unwrap()
            .is_empty());
        hook_execution_fixtures()
            .lock()
            .await
            .remove(&hook_execution_fixture_key(
                &harness.hook,
                &harness.request_id,
            ));
        harness.node.shutdown().await;
    }
}

#[tokio::test]
async fn spawn_process_rejects_target_policy_before_spawned_admission() {
    let cases = crate::lean_vocab_test::lean_canonical_spawned_target_rejection_cases();
    assert!(!cases.is_empty());
    for case in cases {
        for target in DeniedTarget::ALL {
            spawned_target_rejection_case(case, &target).await;
        }
    }
}

enum DeniedTarget {
    ReadOnlyBash,
    CliArgvPrefix,
    McpAllowlist,
}

impl DeniedTarget {
    const ALL: [DeniedTarget; 3] = [
        DeniedTarget::ReadOnlyBash,
        DeniedTarget::CliArgvPrefix,
        DeniedTarget::McpAllowlist,
    ];

    fn target(&self) -> &'static str {
        match self {
            Self::ReadOnlyBash => "read_only_bash",
            Self::CliArgvPrefix => "cli_argv_prefix",
            Self::McpAllowlist => "mcp_allowlist",
        }
    }

    fn spawn_arguments(&self) -> &'static str {
        match self {
            Self::ReadOnlyBash => {
                r#"{"tool_name":"bash","args":{"command":"rm","args":["-rf","."]}}"#
            }
            Self::CliArgvPrefix => r#"{"tool_name":"git","args":{"argv":["push","--force"]}}"#,
            Self::McpAllowlist => {
                r#"{"tool_name":"call_tool","args":{"service_id":"selected-service","tool_name":"unselected-tool","arguments":{}}}"#
            }
        }
    }

    fn registry(&self, node: &Arc<EmbeddedNode>, root: &std::path::Path) -> BackgroundToolRegistry {
        match self {
            Self::ReadOnlyBash => BackgroundToolRegistry::from_tools(
                vec![crate::toolset::read_only_bash_for_test(
                    root,
                    vec!["ls".into()],
                )],
                &["bash".into()],
            ),
            Self::CliArgvPrefix => BackgroundToolRegistry::from_tools(
                vec![crate::toolset::cli_tool_for_test(
                    crate::toolset::CliToolConfig {
                        name: "git".into(),
                        binary_path: "/bin/echo".into(),
                        description: String::new(),
                        allowed_argv_prefixes: vec![vec!["status".into()]],
                        env_vars: RapidHashMap::new(),
                        working_dir: Some(root.to_path_buf()),
                        timeout_secs: 5,
                        max_output_chars: 4096,
                    },
                )],
                &["git".into()],
            ),
            Self::McpAllowlist => BackgroundToolRegistry::from_tools(
                vec![Box::new(crate::meta_tools::CallToolTool::new(
                    crate::meta_tools::MetaToolContext {
                        node: node.clone(),
                        mcp_pool: crate::mcp_pool::McpPool::new().for_agent("did:test:test"),
                        health: crate::health_checker::ServiceHealthMap::new(),
                        local_hostname: "local".into(),
                        local_subnet: None,
                        node_did: "did:test:test".into(),
                        allowed_mcp_service_ids: vec!["selected-service".into()],
                        remote_tools: crate::documents::RemoteTools {
                            services: vec![crate::documents::RemoteServiceTools {
                                mcp_service_id: "selected-service".into(),
                                tool_names: vec!["selected-tool".into()],
                                ..Default::default()
                            }],
                        },
                    },
                ))],
                &["call_tool".into()],
            ),
        }
    }
}

struct AdmissionHarness {
    _dir: tempfile::TempDir,
    root: tempfile::TempDir,
    node: Arc<EmbeddedNode>,
    hook: DefraSessionHook,
    request_id: String,
}

async fn admission_harness(
    registry: impl FnOnce(&Arc<EmbeddedNode>, &std::path::Path) -> BackgroundToolRegistry,
) -> AdmissionHarness {
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
    let root = tempfile::tempdir().unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_background_tool_registry(registry(&node, root.path()));
    hook.on_completion_call(&user_text_message("run in the background"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    crate::session::create_session_with_agent_id(&node, &session, identity.did(), "general")
        .await
        .unwrap();
    let request_id = format!("admission-{}", uuid::Uuid::new_v4());
    bind_interruptible_request(
        &node,
        &hook,
        &request_id,
        &session,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    AdmissionHarness {
        _dir: dir,
        root,
        node,
        hook,
        request_id,
    }
}

async fn spawned_target_rejection_case(
    case: &crate::lean_vocab_test::LeanSpawnedTargetRejectionCase,
    target: &DeniedTarget,
) {
    let expected = &case.expected;
    let AdmissionHarness {
        _dir,
        root,
        node,
        hook,
        request_id,
    } = admission_harness(|node, root| target.registry(node, root)).await;
    let args = target.spawn_arguments();
    accept_hook_tool_call(&hook, "spawn-denied", &case.parent_tool, args, None).await;
    let action = hook
        .on_tool_call(&case.parent_tool, None, "spawn-denied", args)
        .await;
    assert!(
        matches!(action, ToolCallHookAction::Skip { .. }),
        "{}/{}: {action:?}",
        case.name,
        target.target()
    );

    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}} }}) {{ tool_name lifecycle_state started_at tool_failure_class spawned_by_tool_call_doc_id }} }}"#,
            crate::graphql::escape_graphql_string(&request_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let rows = data["AgentToolCall"].as_array().unwrap();
    let (parents, spawned): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|row| row["spawned_by_tool_call_doc_id"].is_null());
    assert_eq!(
        !spawned.is_empty(),
        expected.spawned_admitted,
        "{}: {rows:?}",
        target.target()
    );
    assert_eq!(parents.len(), 1, "{rows:?}");
    let parent = parents[0];
    assert_eq!(parent["tool_name"], case.parent_tool.as_str());
    assert_eq!(
        (
            parent["lifecycle_state"] == "failed",
            parent["started_at"].is_string(),
            parent["tool_failure_class"].as_str(),
        ),
        (
            expected.failed,
            expected.started,
            expected.failure_class.as_deref(),
        ),
        "{}/{}",
        case.name,
        target.target()
    );
    assert!(root.path().exists());

    let mut fixture = hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&hook, &request_id))
        .unwrap();
    let completion_outcome = match case.completion_probe_outcome.as_str() {
        "completed" => crate::lifecycle::RequestTerminalOutcome::Completed,
        other => panic!("unsupported modeled completion probe {other}"),
    };
    let selection = fixture
        .writer
        .terminal_output(&fixture.lifecycle.request().doc_id)
        .await;
    let completion = fixture
        .lifecycle
        .terminalize_owned(completion_outcome, selection, None)
        .await;
    assert_eq!(
        completion.is_ok(),
        expected.completion_accepted,
        "{}/{}: {completion:?}",
        case.name,
        target.target()
    );
    node.shutdown().await;
}

#[tokio::test]
async fn spawn_process_rejects_unadvertised_arguments_before_spawned_admission() {
    for args in [
        r#"{"args":[],"command":"ls","timeout_secs":"25","tool_name":"bash"}"#,
        r#"{"tool_name":"bash","args":{},"command":"ls"}"#,
        r#"{"tool_name":"bash"}"#,
        r#"{"tool_name":"bash","args":"{\"command\":\"ls\"}"}"#,
    ] {
        let AdmissionHarness {
            _dir,
            root: _root,
            node,
            hook,
            request_id,
        } = admission_harness(|_, root| {
            BackgroundToolRegistry::from_tools(
                vec![crate::toolset::read_only_bash_for_test(
                    root,
                    vec!["ls".into()],
                )],
                &["bash".into()],
            )
        })
        .await;
        accept_hook_tool_call(
            &hook,
            "spawn-malformed",
            crate::toolset::SPAWN_PROCESS_TOOL_NAME,
            args,
            None,
        )
        .await;
        let action = hook
            .on_tool_call(
                crate::toolset::SPAWN_PROCESS_TOOL_NAME,
                None,
                "spawn-malformed",
                args,
            )
            .await;
        let ToolCallHookAction::Skip { reason } = &action else {
            panic!("{args}: {action:?}");
        };
        assert!(
            reason.contains("invalid spawn_process arguments"),
            "{args}: {reason}"
        );

        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}} }}) {{ tool_name lifecycle_state tool_failure_class spawned_by_tool_call_doc_id }} }}"#,
                crate::graphql::escape_graphql_string(&request_id),
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{args}: {rows:?}");
        assert_eq!(
            rows[0]["tool_name"],
            crate::toolset::SPAWN_PROCESS_TOOL_NAME
        );
        assert!(rows[0]["spawned_by_tool_call_doc_id"].is_null(), "{rows:?}");
        assert_eq!(rows[0]["lifecycle_state"], "failed", "{args}: {rows:?}");
        assert_eq!(
            rows[0]["tool_failure_class"], "argumentInvalid",
            "{args}: {rows:?}"
        );
        hook_execution_fixtures()
            .lock()
            .await
            .remove(&hook_execution_fixture_key(&hook, &request_id));
        node.shutdown().await;
    }
}
