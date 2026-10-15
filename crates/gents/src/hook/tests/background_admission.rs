use super::*;
use crate::identity::NodeIdentity;

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
                        env_vars: HashMap::new(),
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
                        remote_tools: crate::document_config::RemoteTools {
                            services: vec![crate::document_config::RemoteServiceTools {
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
        crate::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
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

/// Input outside the advertised `spawn_process` schema fails the call that
/// carries it, so the model sees the rejection in the same turn instead of a
/// running receipt for a process whose target arguments cannot decode. The
/// first input is the live shape GLM emitted for the desktop operations smoke.
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
