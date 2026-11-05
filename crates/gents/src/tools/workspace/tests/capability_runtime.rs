use super::*;
use crate::request::lifecycle::{RequestLifecycle, RequestTerminalOutcome, WorkspaceLineage};
use crate::store::identity::NodeIdentity;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn workspace_completion_requires_selected_managed_agent() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .build()
            .await
            .unwrap(),
    );
    for case in &crate::lean_vocab_test::lean_contract_snapshot().workspace_completion_actor_cases {
        let actor = case["actor"].as_str().unwrap();
        let selected = if actor == "empty" { "" } else { agent.did() };
        let held: &dyn NodeIdentity = if actor == "agent" {
            agent.as_ref()
        } else {
            &runtime
        };
        assert_eq!(
            super::super::runtime::completion_access(&node, held, selected).is_ok(),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_completion_reads_keep_selected_agent_visibility() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let selected_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let other_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let selected = crate::store::identity::resolve_agent_identity(&runtime, &selected_binding)?;
    let other = crate::store::identity::resolve_agent_identity(&runtime, &other_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .build()
            .await?,
    );
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    let repository_policy = node.add_dac_policy_as(&context, "name: WorkspaceCompletionReads\nresources:\n  - name: facts\n    permissions:\n      - name: read\n      - name: update\n      - name: delete\n").await?;
    for (name, schema) in [
        (
            "IsolatedWorkspace",
            gents_protocol::schemas::ISOLATED_WORKSPACE,
        ),
        (
            "WorkspacePlacement",
            gents_protocol::schemas::WORKSPACE_PLACEMENT,
        ),
        (
            "WorkspaceBinding",
            gents_protocol::schemas::WORKSPACE_BINDING,
        ),
        (
            "WorkspaceReceipt",
            gents_protocol::schemas::WORKSPACE_RECEIPT,
        ),
        (
            "RepositoryPlacement",
            gents_protocol::schemas::REPOSITORY_PLACEMENT,
        ),
    ] {
        let (id, resource) = if name == "RepositoryPlacement" {
            (&repository_policy, "facts")
        } else {
            (&policy, "agent_children")
        };
        let schema = schema.replacen(
            &format!("type {name}"),
            &format!("type {name} @policy(id: \"{id}\", resource: \"{resource}\")"),
            1,
        );
        node.add_collection_as(&context, &schema).await?;
    }
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for actor in [selected.did(), other.did()] {
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let access = |identity: &dyn NodeIdentity| {
        crate::ConfigAccess::local_managed_agent(
            node.clone(),
            identity.did().parse().unwrap(),
            identity,
        )
        .unwrap()
    };
    let selected_access = access(selected.as_ref());
    let other_access = access(other.as_ref());
    let doc = IsolatedWorkspaceDoc {
        path_capability: WorkspacePathCapability::exact_paths(Vec::new())?,
        workspace_id: "workspace".into(),
        work_unit_id: "unit".into(),
        repository_id: "repository".into(),
        base_sha: "base".into(),
        branch: "branch".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_node_did: runtime.did().into(),
        writer_principal: runtime.did().into(),
        integrator_principal: runtime.did().into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "invocation".into(),
        caused_by_correlation: "correlation".into(),
    };
    selected_access
        .write(
            "test.workspace.private",
            &isolated_workspace_upsert_mutation(&doc),
        )
        .await?;
    let repository = RepositoryPlacementRef {
        repository_id: "repository".into(),
        owner_node_did: runtime.did().into(),
        host_path: directory.path().to_owned(),
        enabled: true,
    };
    selected_access
        .write(
            "test.workspace.repository",
            &repository_placement_upsert_mutation(&repository, "2026-11-05T00:00:00Z")?,
        )
        .await?;
    let selected_docs =
        super::super::runtime::load_docs_with_access(&selected_access, "workspace", runtime.did())
            .await?;
    let other_docs =
        super::super::runtime::load_docs_with_access(&other_access, "workspace", runtime.did())
            .await?;
    for case in &crate::lean_vocab_test::lean_contract_snapshot().workspace_completion_access_cases
    {
        let docs = match case["actor"].as_str().unwrap() {
            "agent" => &selected_docs,
            "other" => &other_docs,
            actor => panic!("unknown modeled actor {actor}"),
        };
        assert_eq!(
            docs.load_isolated_workspace("workspace")?.is_some(),
            case["read"].as_bool().unwrap(),
            "{case}"
        );
    }
    assert!(super::super::runtime::load_repository_with_access(
        &selected_access,
        "repository",
        runtime.did()
    )
    .await
    .is_ok());
    assert!(super::super::runtime::load_repository_with_access(
        &other_access,
        "repository",
        runtime.did()
    )
    .await
    .is_err());
    node.shutdown().await?;
    Ok(())
}

async fn query(node: &defra_node::Node, document: &str) -> serde_json::Value {
    let response = node.execute(document).await;
    assert!(!response.has_errors(), "{document}: {:?}", response.errors);
    response.data.expect("database result")
}

async fn runtime_seal_case(unowned: bool) {
    let fx = Fixture::new();
    let db_dir = tempfile::tempdir().unwrap();
    let identity =
        crate::store::identity::KeyIdentity::load_or_create(db_dir.path().join("agent.key"), None)
            .unwrap();
    let did = identity.did();
    let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .data_path(db_dir.path().join("db"))
            .with_node_identity_did(did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let admin = crate::ConfigAccess::local_as(node.clone(), did.parse().unwrap());
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::DacRelationAdd,
    ] {
        admin
            .grant_node_permission(&agent.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    node.add_collection("type CapabilityReviewVerdict { workspace_id: String accepted: Boolean }")
        .await
        .unwrap();
    let mut memory = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("runtime-capability", "runtime-unit", "runtime-capability");
    action.path_capability = WorkspacePathCapability::exact_paths(vec!["patch.rs".into()]).unwrap();
    let created = {
        let mut ctx = fx.ctx(&mut memory, git_worktree_caps());
        ctx.writer_principal = agent.did().to_owned();
        ctx.integrator_principal = agent.did().to_owned();
        execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut ctx,
        )
        .unwrap()
    };
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let repository = RepositoryPlacementRef {
        repository_id: "repo-1".into(),
        owner_node_did: created.workspace.owner_node_did.clone(),
        host_path: fx.repo.clone(),
        enabled: true,
    };
    for mutation in [
        isolated_workspace_upsert_mutation(&created.workspace),
        workspace_placement_upsert_mutation(&created.placement, &timestamp),
        repository_placement_upsert_mutation(&repository, &timestamp).unwrap(),
    ] {
        query(&node, &mutation).await;
    }
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "runtime-capability-writer",
        did,
        did,
        agent.did(),
        "runtime-capability-session",
        "Apply the owned patch",
        "interactive",
        &timestamp,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    create.workspace_id = Some(created.workspace.workspace_id.clone());
    create.workspace_authority = Some("readWrite".into());
    create.workspace_owner_node_did = Some(created.workspace.owner_node_did.clone());
    crate::request::admission::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    query(&node, &create.graphql_mutation().unwrap()).await;
    let response = node
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::request::admission::SIGNED_REQUEST_FIELDS
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::store::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let request = crate::request::watcher::AgentRequest::try_from(row).unwrap();
    admin
        .add_actor_relationship(
            "AgentRequest",
            &request.doc_id,
            "working_agent",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let lineage = WorkspaceLineage {
        workspace_id: request.workspace_id.clone(),
        workspace_authority: request.workspace_authority.clone(),
        workspace_owner_node_did: request.workspace_owner_node_did.clone(),
        workspace_seal_hash: None,
    };
    materialize_workspace_binding(
        &node,
        &request.request_id,
        &request.doc_id,
        agent.as_ref(),
        &lineage,
    )
    .await
    .unwrap();
    let mut owner =
        RequestLifecycle::new_with_node_did(node.clone(), agent.did(), did, request, 120);
    owner.bind_managed_agent(agent.as_ref()).unwrap();
    owner.claim().await.unwrap();
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &owner,
        Duration::ZERO,
    )
    .unwrap();
    owner.begin_owned_execution(&writer).await.unwrap();
    let request = owner.request().clone();
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "pub fn owned_patch() {}\n").unwrap();
    if unowned {
        fs::create_dir(dest.join(".tmp-build")).unwrap();
        fs::write(
            dest.join(".tmp-build/test-build.log"),
            "compiler output must not become source\n",
        )
        .unwrap();
    }
    query(&node, r#"mutation { create_CapabilityReviewVerdict(input: { workspace_id: "runtime-capability", accepted: true }) { _docID } }"#).await;
    let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let before_readme = fs::read(fx.repo.join("README.md")).unwrap();
    let result = seal_on_writer_success(&node, agent.as_ref(), &request, Some(&fx.repo)).await;
    if unowned {
        let error = result
            .expect_err("accepted verdict cannot authorize an unowned log")
            .to_string();
        assert!(error.contains(".tmp-build/test-build.log"), "{error}");
        owner
            .terminalize_owned(
                RequestTerminalOutcome::Failed,
                gents_protocol::output::TerminalOutput::NoMessage,
                Some(&error),
            )
            .await
            .unwrap();
        let mut integrator = request.clone();
        integrator.workspace_authority = Some("integrate".into());
        let denial =
            integrate_on_integrator_success(&node, agent.as_ref(), &integrator, Some(&fx.repo))
                .await
                .expect_err("unsealed rejected writer cannot integrate")
                .to_string();
        assert!(
            denial.contains("Sealed") || denial.contains("sealed"),
            "{denial}"
        );
    } else {
        result.expect("owned untracked file seals through the runtime");
        let receipts = query(
            &node,
            "{ WorkspaceReceipt { _docID _version { cid height fieldName } } }",
        )
        .await;
        let document = &receipts["WorkspaceReceipt"][0];
        let version =
            crate::store::graphql::document_composite_version(document, "workspace receipt")
                .unwrap()
                .unwrap();
        assert_eq!(
            node.verified_block_signer_did(&version.cid).await.unwrap(),
            agent.did()
        );
        owner
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
    }
    let durable = query(
        &node,
        r#"{
        CapabilityReviewVerdict { accepted }
        IsolatedWorkspace { lifecycle_state seal_hash path_capability }
        WorkspaceReceipt { kind path_capability_digest changed_files }
        AgentRequest { request_id lifecycle_state failure_reason terminal_output }
        AgentMessage { _docID }
        AgentOutputSegment { _docID }
    }"#,
    )
    .await;
    assert_eq!(durable["CapabilityReviewVerdict"][0]["accepted"], true);
    assert_eq!(
        durable["IsolatedWorkspace"][0]["lifecycle_state"],
        if unowned { "ready" } else { "sealed" }
    );
    assert_eq!(
        durable["AgentRequest"][0]["lifecycle_state"],
        if unowned { "failed" } else { "completed" }
    );
    let requests = durable["AgentRequest"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    let terminal: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(requests[0].clone()).unwrap();
    assert_eq!(
        terminal.terminal_output,
        Some(gents_protocol::output::TerminalOutput::NoMessage)
    );
    assert!(durable["AgentMessage"].as_array().unwrap().is_empty());
    assert!(durable["AgentOutputSegment"].as_array().unwrap().is_empty());
    let receipts = durable["WorkspaceReceipt"].as_array().unwrap();
    if unowned {
        assert!(requests[0]["failure_reason"]
            .as_str()
            .unwrap()
            .contains(".tmp-build/test-build.log"));
        assert!(
            receipts.is_empty(),
            "rejection must not publish a writer receipt"
        );
        assert!(durable["IsolatedWorkspace"][0]["seal_hash"].is_null());
    } else {
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["kind"], "writer");
        assert_eq!(
            receipts[0]["path_capability_digest"],
            created.workspace.path_capability.digest()
        );
        assert!(receipts[0]["changed_files"]
            .as_str()
            .unwrap()
            .contains("patch.rs"));
    }
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(fs::read(fx.repo.join("README.md")).unwrap(), before_readme);
    assert!(!fx.repo.join("patch.rs").exists());
    assert!(!fx.repo.join(".tmp-build/test-build.log").exists());
    drop(owner);
    node.shutdown().await.expect("shut down test node");
    // Keep the DB directory alive for Defra background tasks.
    std::mem::forget(db_dir);
}

#[tokio::test]
async fn accepted_domain_verdict_cannot_override_unowned_log_and_failure_is_durable() {
    runtime_seal_case(true).await;
}

#[tokio::test]
async fn owned_untracked_patch_produces_durable_runtime_writer_receipt() {
    runtime_seal_case(false).await;
}

#[tokio::test]
async fn invisible_workspace_request_cannot_release_exclusive_binding() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let first_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let second_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let first = crate::store::identity::resolve_agent_identity(&runtime, &first_binding)?;
    let second = crate::store::identity::resolve_agent_identity(&runtime, &second_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .build()
            .await?,
    );
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection_as(&context, gents_protocol::schemas::AGENT_REQUEST)
        .await?;
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for actor in [first.did(), second.did()] {
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let second_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        second.did().parse()?,
        second.as_ref(),
    )?;
    let first_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        first.did().parse()?,
        first.as_ref(),
    )?;
    for case in &crate::lean_vocab_test::lean_contract_snapshot().workspace_liveness_cases {
        let visible = case["visible"].as_bool().unwrap();
        let terminal = case["terminal"].as_bool().unwrap();
        let state = if terminal { "completed" } else { "pending" };
        let request_id = format!("old-{visible}-{terminal}");
        let response = second_access.write("test.workspace.foreign_request", &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{request_id}", purpose: "normal", node_did: "{}", requester_did: "{}", agent_did: "{}", session_id: "session", content: "work", created_at: "2026-11-05T00:00:00Z", workspace_id: "workspace", workspace_owner_node_did: "{}", lifecycle_state: "{state}"}}) {{_docID}} }}"#, runtime.did(), runtime.did(), second.did(), runtime.did())).await?;
        let doc = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
        let binding = WorkspaceBindingDoc {
            binding_id: "binding".into(),
            workspace_id: "workspace".into(),
            request_id,
            request_doc_id: doc,
            authority: "readWrite".into(),
            owner_node_did: runtime.did().into(),
            seal_hash: None,
            lifecycle_state: "active".into(),
        };
        let observer = if visible {
            &second_access
        } else {
            &first_access
        };
        let result = super::super::overlay::previous_exclusive_is_stale_with_access(
            observer,
            "workspace",
            &[binding],
            "new-request",
            false,
        )
        .await;
        if visible {
            assert_eq!(result?, case["may_release"].as_bool().unwrap(), "{case}");
        } else {
            assert!(result.is_err(), "{case}");
            assert_eq!(case["may_release"], false);
        }
    }
    node.shutdown().await?;
    Ok(())
}
