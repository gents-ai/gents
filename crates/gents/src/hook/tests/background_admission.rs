use super::*;
use crate::identity::AgentIdentity;

/// The generated spawned-target case fixes what the parent row, the spawned
/// rows and request completion must show once the real read-only command owner
/// denies the target.
#[tokio::test]
async fn spawn_process_rejects_target_policy_before_spawned_admission() {
    let cases = crate::lean_vocab_test::lean_canonical_spawned_target_rejection_cases();
    assert!(!cases.is_empty());
    for case in cases {
        spawned_target_rejection_case(case).await;
    }
}

async fn spawned_target_rejection_case(
    case: &crate::lean_vocab_test::LeanSpawnedTargetRejectionCase,
) {
    let expected = &case.expected;
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
    crate::test_support::install_test_behavior(&node, identity.did(), "general").await;
    let root = tempfile::tempdir().unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        identity.did(),
        FailurePolicy::default(),
    )
    .with_background_tool_registry(BackgroundToolRegistry::from_tools(
        vec![crate::toolset::read_only_bash_for_test(
            root.path(),
            vec!["ls".into()],
        )],
        &["bash".into()],
    ));
    hook.on_completion_call(&user_text_message("remove in the background"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    crate::session::create_session_with_behavior_id(
        &node,
        &session,
        "general",
        identity.did(),
        "general",
    )
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
    let args = r#"{"tool_name":"bash","args":{"command":"rm","args":["-rf","."]}}"#;
    accept_hook_tool_call(&hook, "spawn-denied", &case.parent_tool, args, None).await;
    let action = hook
        .on_tool_call(&case.parent_tool, None, "spawn-denied", args)
        .await;
    assert!(
        matches!(action, ToolCallHookAction::Skip { .. }),
        "{}: {action:?}",
        case.name
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
    assert_eq!(!spawned.is_empty(), expected.spawned_admitted, "{rows:?}");
    assert_eq!(parents.len(), 1, "{rows:?}");
    let parent = parents[0];
    assert_eq!(parent["tool_name"], case.parent_tool.as_str());
    assert_eq!(parent["lifecycle_state"] == "failed", expected.failed);
    assert_eq!(parent["started_at"].is_string(), expected.started);
    assert_eq!(
        parent["tool_failure_class"].as_str(),
        expected.failure_class.as_deref()
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
        "{}: {completion:?}",
        case.name
    );
    node.shutdown().await;
}
