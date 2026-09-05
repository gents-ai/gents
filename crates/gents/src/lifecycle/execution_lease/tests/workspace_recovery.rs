use super::*;
use crate::identity::AgentIdentity;
use crate::TriggerSource;

#[tokio::test]
async fn expired_execution_recovers_one_goal_successor_that_reopens_existing_workspace_changes() {
    let dir = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
    let did = identity.did();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path().join("db"))
            .with_node_identity_did(did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let workspace_path = dir.path().join("existing-workspace");
    std::fs::create_dir(&workspace_path).unwrap();
    std::fs::write(
        workspace_path.join("unfinished.txt"),
        "existing durable work\n",
    )
    .unwrap();
    let workspace = crate::workspace::IsolatedWorkspaceDoc {
        workspace_id: "recovery-workspace".into(),
        work_unit_id: "work-unit".into(),
        repository_id: "repository".into(),
        base_sha: "base".into(),
        branch: "existing-branch".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_deployment_id: "recovery-host".into(),
        writer_principal: did.into(),
        integrator_principal: did.into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "invocation".into(),
        caused_by_correlation: "correlation".into(),
    };
    let placement = crate::workspace::WorkspacePlacementDoc {
        workspace_id: workspace.workspace_id.clone(),
        deployment_id: "recovery-host".into(),
        host_path: workspace_path.to_string_lossy().into_owned(),
        repository_placement_id: "repository-placement".into(),
        adapter: "git_worktree".into(),
        adapter_version: "1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "ready".into(),
        observed_tree_hash: String::new(),
    };
    for mutation in [crate::workspace::isolated_workspace_upsert_mutation(&workspace),
        crate::workspace::workspace_placement_upsert_mutation(&placement, "2026-09-01T00:00:00Z"),
        r#"mutation { create_HostDeployment(input: { deployment_id: "recovery-host", display_name: "local", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z" }) { _docID } }"#.into()] {
        let result = node.execute(&mutation).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
    }
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        "workspace-recovery-parent",
        did,
        did,
        "general",
        "workspace-recovery-session",
        "resume durable work",
        "interactive",
        "2026-09-01T00:00:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    create.workspace_id = Some(workspace.workspace_id.clone());
    create.workspace_authority = Some("readOnly".into());
    create.workspace_owner_deployment_id = Some("recovery-host".into());
    create.subagent_depth = 2;
    create.caused_by_parent_request_id = Some("grandparent-request".into());
    create.caused_by_parent_request_doc_id = Some("grandparent-request-doc".into());
    create.caused_by_parent_tool_call_id = Some("grandparent-tool".into());
    create.caused_by_parent_tool_call_doc_id = Some("grandparent-tool-doc".into());
    crate::request_admission::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    let result = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    async fn requests(node: &EmbeddedNode) -> Vec<AgentRequest> {
        let result = node.execute(r#"{ AgentRequest { _docID request_id agent_did requester_did behavior_id session_id content metadata created_at lifecycle_state execution_origin subagent_depth caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id caused_by_trigger_kind workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash execution_generation execution_lease_expires_at execution_progress_seq } }"#).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        crate::graphql::rows::<AgentRequestRow>(&result, "AgentRequest")
            .unwrap()
            .into_iter()
            .map(|row| AgentRequest::try_from(row).unwrap())
            .collect()
    }
    let parent = requests(&node).await.remove(0);
    let mut owner = RequestLifecycle::new_with_agent_did(node.clone(), "general", did, parent, 60);
    owner.claim().await.unwrap();
    let writer = crate::streaming::DefraStreamWriter::new(node.clone(), did, Duration::ZERO);
    let response_id = owner.begin_owned_execution(&writer).await.unwrap();
    writer
        .write_tokens(&response_id, "partial durable response")
        .await
        .unwrap();
    let parent = owner.request().clone();
    let parent_overlay =
        crate::workspace::resolve_request_workspace_overlay(&node, &parent, Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        std::fs::read_to_string(parent_overlay.cwd.join("unfinished.txt")).unwrap(),
        "existing durable work\n"
    );
    crate::goal::set_goal(
        &node,
        did,
        &parent.session_id,
        Some("Finish existing work"),
        Some(crate::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    expire_observed_execution(&node, &parent.doc_id).await;
    assert_eq!(
        RequestLifecycle::recover_all(&node, did)
            .await
            .unwrap()
            .requests_recovered,
        1
    );
    assert_eq!(
        request_row(&node, &parent.doc_id).await.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let recovered_response = terminal_response_snapshot(&node, &parent.doc_id).await;
    assert!(recovered_response["content"]
        .as_str()
        .unwrap()
        .starts_with("partial durable response"));
    let snapshot = Arc::new(crate::ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: did.into(),
        default_behavior_id: "general".into(),
        behaviors: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_behaviors: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        behavior_executor_capacities: Default::default(),
        behavior_executor_queue_capacities: Default::default(),
    });
    let (_tx, rx) = tokio::sync::watch::channel(snapshot);
    let mut source = crate::GoalSource::new(
        rx.clone(),
        node.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_rescan_interval(Duration::from_millis(20));
    tokio::time::timeout(Duration::from_secs(3), source.next_fire())
        .await
        .unwrap()
        .unwrap();
    let children: Vec<_> = requests(&node)
        .await
        .into_iter()
        .filter(|request| request.request_id != parent.request_id)
        .collect();
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.session_id, parent.session_id);
    assert!(child.content.contains("Finish existing work"));
    assert_eq!(child.workspace_id, parent.workspace_id);
    assert_eq!(child.workspace_authority, parent.workspace_authority);
    assert_eq!(
        child.workspace_owner_deployment_id,
        parent.workspace_owner_deployment_id
    );
    assert_eq!(child.subagent_depth, parent.subagent_depth);
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent.request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(parent.doc_id.as_str())
    );
    let overlay =
        crate::workspace::resolve_request_workspace_overlay(&node, child, Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(overlay.root, parent_overlay.root);
    assert_eq!(
        std::fs::read_to_string(overlay.cwd.join("unfinished.txt")).unwrap(),
        "existing durable work\n"
    );
    drop(source);
    for _ in 0..2 {
        assert_eq!(
            RequestLifecycle::recover_all(&node, did)
                .await
                .unwrap()
                .requests_recovered,
            0
        );
        let mut restarted = crate::GoalSource::new(
            rx.clone(),
            node.clone(),
            tokio_util::sync::CancellationToken::new(),
        )
        .with_rescan_interval(Duration::from_millis(20));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
                .await
                .is_err()
        );
        let remaining: Vec<_> = requests(&node)
            .await
            .into_iter()
            .filter(|request| request.request_id != parent.request_id)
            .collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].doc_id, child.doc_id);
        assert_eq!(
            std::fs::read_to_string(overlay.cwd.join("unfinished.txt")).unwrap(),
            "existing durable work\n"
        );
    }
}
