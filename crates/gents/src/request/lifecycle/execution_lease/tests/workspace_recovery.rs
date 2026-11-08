use super::*;
use crate::store::identity::NodeIdentity;
use crate::TriggerSource;

#[tokio::test]
async fn expired_execution_recovers_one_goal_successor_that_reopens_existing_workspace_changes() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(
        crate::store::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None)
            .unwrap(),
    );
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
    let binding = crate::store::identity::provision_agent_identity(identity.as_ref()).unwrap();
    let admin = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(did).unwrap());
    crate::test_support::bootstrap_signed_test_node(&admin, did).await;
    crate::test_support::install_test_agent_binding_with_access(
        did,
        "workspace recovery",
        &binding,
        &admin,
    )
    .await;
    let agent =
        crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
    let agent_did = agent.did();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent_did).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    access
        .transact_write("test.workspace_recovery_session", |txn| {
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    "workspace-recovery-session",
                    did,
                    agent_did,
                    Some(agent_did),
                    None,
                    None,
                    "2026-09-01T00:00:00Z",
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
    // The session's creator publishes its relationships, as client and goal
    // sessions do, before runtime successors may target it.
    let (session_doc, agent_doc) = access
        .transact_readonly("test.workspace_recovery_targets", |txn| {
            Box::pin(async move {
                let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    did,
                    "workspace-recovery-session",
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture session is missing"))?
                .doc_id;
                let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    did,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture Agent is missing"))?;
                Ok::<_, anyhow::Error>((session, agent_doc))
            })
        })
        .await
        .unwrap();
    crate::session::grant_created_session_relationships(
        &access,
        &session_doc,
        &agent_doc,
        agent_did,
    )
    .await
    .unwrap();
    let workspace_path = dir.path().join("existing-workspace");
    std::fs::create_dir(&workspace_path).unwrap();
    std::fs::write(
        workspace_path.join("unfinished.txt"),
        "existing durable work\n",
    )
    .unwrap();
    let workspace = crate::tools::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::tools::workspace::WorkspacePathCapability::exact_paths(vec![])
            .unwrap(),
        workspace_id: "recovery-workspace".into(),
        work_unit_id: "work-unit".into(),
        repository_id: "repository".into(),
        base_sha: "base".into(),
        branch: "existing-branch".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_node_did: did.into(),
        writer_principal: agent_did.into(),
        integrator_principal: agent_did.into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "invocation".into(),
        caused_by_correlation: "correlation".into(),
    };
    let placement = crate::tools::workspace::WorkspacePlacementDoc {
        workspace_id: workspace.workspace_id.clone(),
        owner_node_did: did.into(),
        host_path: workspace_path.to_string_lossy().into_owned(),
        repository_placement_id: "repository-placement".into(),
        adapter: "git_worktree".into(),
        adapter_version: "1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "ready".into(),
        observed_tree_hash: String::new(),
    };
    fn upserted_doc_id(response: &serde_json::Value, field: &str) -> anyhow::Result<String> {
        let rows = crate::store::graphql::mutation_documents(response, field)?
            .ok_or_else(|| anyhow::anyhow!("{field} omitted its document"))?;
        anyhow::ensure!(rows.len() == 1, "{field} affected {} documents", rows.len());
        Ok(rows[0]["_docID"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{field} omitted _docID"))?
            .to_owned())
    }
    // The selected Agent publishes and reads its workspace and local placement.
    access
        .transact_write("test.workspace_recovery_workspace", |txn| {
            let workspace = &workspace;
            let placement = &placement;
            Box::pin(async move {
                let response = txn
                    .execute(&crate::tools::workspace::isolated_workspace_upsert_mutation(
                        workspace,
                    ))
                    .await?;
                let workspace_doc = upserted_doc_id(&response, "upsert_IsolatedWorkspace")?;
                txn.add_actor_relationship(
                    "IsolatedWorkspace",
                    &workspace_doc,
                    "writer",
                    &workspace.writer_principal.parse()?,
                )
                .await?;
                let response = txn
                    .execute(&crate::tools::workspace::workspace_placement_upsert_mutation(
                        placement,
                        "2026-09-01T00:00:00Z",
                    ))
                    .await?;
                let placement_doc = upserted_doc_id(&response, "upsert_WorkspacePlacement")?;
                txn.add_resource_relationship(
                    "WorkspacePlacement",
                    &placement_doc,
                    "parent",
                    "workspaces",
                    &workspace_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "workspace-recovery-parent",
        did,
        agent_did,
        agent_did,
        "workspace-recovery-session",
        "resume durable work",
        "interactive",
        "2026-09-01T00:00:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent_did),
    );
    create.workspace_id = Some(workspace.workspace_id.clone());
    create.workspace_authority = Some("readOnly".into());
    create.workspace_owner_node_did = Some(did.into());
    create.request_hop = 2;
    create.caused_by_parent_request_id = Some("grandparent-request".into());
    create.caused_by_parent_request_doc_id = Some("grandparent-request-doc".into());
    create.caused_by_parent_tool_call_id = Some("grandparent-tool".into());
    create.caused_by_parent_tool_call_doc_id = Some("grandparent-tool-doc".into());
    crate::request::admission::sign_agent_request_create(agent.as_ref(), &mut create)
        .await
        .unwrap();
    access
        .write(
            "test.workspace_recovery_parent",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    async fn requests(
        access: &crate::ConfigAccess,
        owner: &str,
        requester: &str,
    ) -> Vec<AgentRequest> {
        let scope = gents_protocol::session::session_scope_filter(
            owner,
            "workspace-recovery-session",
            Some(requester),
        );
        let response = access
            .execute(&format!(
                "{{AgentRequest(filter: {{{scope}}}) {{{}}}}}",
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .expect("AgentRequest read as its Agent");
        serde_json::from_value::<Vec<AgentRequestRow>>(response["data"]["AgentRequest"].clone())
            .unwrap()
            .into_iter()
            .map(|row| AgentRequest::try_from(row).unwrap())
            .collect()
    }
    let parent = requests(&access, did, agent_did).await.remove(0);
    let mut owner = RequestLifecycle::new_with_node_did(node.clone(), agent_did, did, parent, 60);
    owner.bind_managed_agent(agent.as_ref()).unwrap();
    owner.claim().await.unwrap();
    let writer = crate::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        agent.as_ref(),
        Duration::ZERO,
    )
    .unwrap();
    owner.begin_owned_execution(&writer).await.unwrap();
    let response_id = owner.request().doc_id.clone();
    writer
        .start_provider_attempt(&response_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    writer
        .flush_native_partial(
            &owner,
            &gents_protocol::message::Message::assistant("partial durable response"),
        )
        .await
        .unwrap();
    let parent = owner.request().clone();
    let parent_overlay = crate::tools::workspace::resolve_request_workspace_overlay_with_identity(
        &node,
        agent.as_ref(),
        &parent,
        owner.execution_generation().unwrap(),
        false,
        Some(dir.path()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(parent_overlay.cwd.join("unfinished.txt")).unwrap(),
        "existing durable work\n"
    );
    crate::goal::set_goal_from_access(
        &access,
        did,
        &parent.session_id,
        Some("Finish existing work"),
        Some(crate::goal::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    expire_observed_execution(&access, &parent.doc_id).await;
    assert_eq!(
        RequestLifecycle::recover_all_with_identities(&node, did, std::slice::from_ref(&agent))
            .await
            .unwrap()
            .requests_recovered,
        1
    );
    assert_eq!(
        request_row(&access, &parent.doc_id).await.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let recovered_output = output_segment_snapshot(&access, &parent.doc_id).await;
    assert!(recovered_output
        .to_string()
        .contains("partial durable response"));
    let runtime_node = Arc::new(crate::store::identity::RuntimeNode {
        node_did: did.to_owned(),
        identity: identity.clone(),
        default_agent_did: agent_did.to_owned(),
        display_name: None,
        enabled: true,
    });
    let resolved_agent = Arc::new(crate::inference::resolved::ResolvedAgent {
        agent_did: agent_did.to_owned(),
        node: runtime_node.clone(),
        agent_identity: agent.clone(),
        configuration: crate::inference::resolved::ResolvedConfiguration::Static,
        backend_id: None,
        backend_provider_kind: crate::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: String::new(),
        backend_auth: crate::documents::BackendAuth::Unauthenticated,
        claude_platform: None,
        model_name: "fixture".into(),
        profile_id: None,
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: 8192,
        max_output_tokens: 1024,
        max_turns: 2,
        max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: crate::tools::surface::AgentToolSurfaceConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: 0,
        stream_liveness_timeout: Duration::from_secs(60),
        provider_idle_timeout: Duration::from_secs(60),
        deadline_duration: Duration::from_secs(60),
        completion_retry: Default::default(),
        sampling: Default::default(),
        skills: Vec::new(),
        oauth_pool: Vec::new(),
    });
    let (dispatcher, _executor) = tokio::sync::mpsc::channel(1);
    let snapshot = Arc::new(crate::ActiveRuntimeSnapshot {
        generation: 1,
        node: Some(runtime_node),
        local_did: did.into(),
        default_agent_did: agent_did.into(),
        agents: std::iter::once((agent_did.to_owned(), resolved_agent)).collect(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_agents: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: std::iter::once((agent_did.to_owned(), dispatcher)).collect(),
        agent_executor_capacities: Default::default(),
        agent_executor_queue_capacities: Default::default(),
    });
    let readiness = gents_protocol::row::project_node_readiness_source(
        gents_protocol::row::NodeReadinessProcessState::Ready,
        1,
        1,
        agent_did,
        [gents_protocol::row::AgentReadinessSourceEntry {
            agent_did: agent_did.into(),
            dispatcher_present: true,
            unavailable_reason: None,
            startup_demoted: false,
        }],
    )
    .unwrap();
    crate::runtime::node_readiness_publisher::upsert_node_readiness(
        &node,
        did,
        &readiness,
        &chrono::Utc::now().to_rfc3339(),
    )
    .await
    .unwrap();
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
    let children: Vec<_> = requests(&access, did, agent_did)
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
        child.workspace_owner_node_did,
        parent.workspace_owner_node_did
    );
    assert_eq!(child.request_hop, parent.request_hop);
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent.request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(parent.doc_id.as_str())
    );
    let overlay = crate::tools::workspace::resolve_request_workspace_overlay_with_identity(
        &node,
        agent.as_ref(),
        child,
        "",
        false,
        Some(dir.path()),
    )
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
            RequestLifecycle::recover_all_with_identities(&node, did, std::slice::from_ref(&agent))
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
        let remaining: Vec<_> = requests(&access, did, agent_did)
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
