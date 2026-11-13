use anyhow::Context;
use gents_session::request::lifecycle::{
    RequestLifecycle, RequestTerminalOutcome, WorkspaceLineage,
};
use gents_session::workspace::{
    bind_integrate, git, git_worktree_caps, integrate_writer, seal_writer, Fixture,
};
use gents_session::workspace::{
    isolated_workspace_upsert_mutation, materialize_workspace_binding,
    workspace_binding_upsert_mutation, workspace_host_path, CreateWorkspaceAction, CreationPolicy,
    IsolatedWorkspaceDoc, MemoryWorkspaceDocuments, RepositoryPlacementRef, WorkspaceAdapterKind,
    WorkspaceBindingDoc, WorkspaceDocuments, WorkspacePathCapability, WorkspacePlacementDoc,
    WorkspaceReaderBinding,
};
use gents_store::identity::NodeIdentity;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn native_workspace_publication_and_exclusive_admission() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user.key"), None)?;
    let creator_key = gents_store::identity::provision_agent_identity(&user)?;
    let writer_key = gents_store::identity::provision_agent_identity(&user)?;
    let integrator_key = gents_store::identity::provision_agent_identity(&user)?;
    let reader_key = gents_store::identity::provision_agent_identity(&user)?;
    let other_key = gents_store::identity::provision_agent_identity(&user)?;
    let creator = gents_store::identity::resolve_agent_identity(&user, &creator_key)?;
    let writer = gents_store::identity::resolve_agent_identity(&user, &writer_key)?;
    let integrator = gents_store::identity::resolve_agent_identity(&user, &integrator_key)?;
    let reader = gents_store::identity::resolve_agent_identity(&user, &reader_key)?;
    let other = gents_store::identity::resolve_agent_identity(&user, &other_key)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(user.did())
            .with_node_acp_owner(user.did().to_owned())
            .build()
            .await?,
    );
    let ctx = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&ctx, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY, 0)
        .await?;
    for (name, schema, resource) in [
        ("Agent", gents_protocol::schemas::AGENT, "agents"),
        (
            "AgentRequest",
            gents_protocol::schemas::AGENT_REQUEST,
            "delivered_requests",
        ),
        (
            "IsolatedWorkspace",
            gents_protocol::schemas::ISOLATED_WORKSPACE,
            "workspaces",
        ),
        (
            "WorkspacePlacement",
            gents_protocol::schemas::WORKSPACE_PLACEMENT,
            "workspace_children",
        ),
        (
            "WorkspaceBinding",
            gents_protocol::schemas::WORKSPACE_BINDING,
            "workspace_children",
        ),
    ] {
        let schema = if schema.contains("@policy(") {
            schema.replace(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID, &policy)
        } else {
            schema.replacen(
                &format!("type {name}"),
                &format!("type {name} @policy(id: \"{policy}\", resource: \"{resource}\")"),
                1,
            )
        };
        node.add_collection_as(&ctx, &schema).await?;
    }
    let admin =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), user.did().parse()?);
    for actor in [
        creator.did(),
        writer.did(),
        integrator.did(),
        reader.did(),
        other.did(),
    ] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::DocumentDelete,
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::DacRelationAdd,
            gents_store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let mut agent_docs = Vec::new();
    for binding in [&creator_key, &writer_key, &integrator_key, &reader_key] {
        let response = admin.write("test.workspace.agent", &format!(r#"mutation {{create_Agent(input: {{node_did: "{}", agent_did: "{}", creation_nonce: "{}", enabled: true}}) {{_docID}}}}"#, user.did(), binding.agent_did, binding.creation_nonce)).await?;
        let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
        admin
            .add_actor_relationship("Agent", &doc, "reader", &creator.did().parse()?)
            .await?;
        agent_docs.push(doc);
    }
    let creator_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        creator.did().parse()?,
        creator.as_ref(),
    )?;
    let writer_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        writer.did().parse()?,
        writer.as_ref(),
    )?;
    let action = CreateWorkspaceAction {
        workspace_id: "workspace".into(),
        work_unit_id: "unit".into(),
        repository_id: "repository".into(),
        base_sha: "base".into(),
        branch: "branch".into(),
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
        path_capability: WorkspacePathCapability::exact_paths(Vec::new())?,
        writer_agent_did: Some(writer.did().into()),
        writer_agent_doc_id: Some(agent_docs[1].clone()),
        integrator_agent_did: Some(integrator.did().into()),
        integrator_agent_doc_id: Some(agent_docs[2].clone()),
        readers: vec![WorkspaceReaderBinding {
            agent_did: reader.did().into(),
            agent_doc_id: Some(agent_docs[3].clone()),
        }],
    };
    let fx = tokio::task::spawn_blocking(Fixture::new).await?;
    let mut invalid_action = action.clone();
    invalid_action.workspace_id = "denied-reservation".into();
    invalid_action.work_unit_id = "denied-unit".into();
    invalid_action.base_sha = fx.base_sha.clone();
    invalid_action.writer_agent_doc_id = Some("unavailable-physical-agent".into());
    let repo = RepositoryPlacementRef {
        repository_id: invalid_action.repository_id.clone(),
        owner_node_did: user.did().into(),
        host_path: fx.repo.clone(),
        enabled: true,
    };
    let reservation = {
        let action = invalid_action.clone();
        let repository = repo.clone();
        let ceiling = fx.parent().to_path_buf();
        let owner = user.did().to_owned();
        let writer = writer.did().to_owned();
        let integrator = integrator.did().to_owned();
        tokio::task::spawn_blocking(move || {
            gents_session::workspace::executor::create::prepare_workspace_reservation(
                &action,
                &repository,
                Some(&ceiling),
                &owner,
                &writer,
                &integrator,
                "denied-invocation",
                "denied-correlation",
                &Default::default(),
            )
        })
        .await??
    };
    let target = workspace_host_path(
        &fx.repo,
        &invalid_action.workspace_id,
        &invalid_action.branch,
        Some(fx.parent()),
    )?;
    let mut denied_docs = MemoryWorkspaceDocuments::default();
    denied_docs.write_isolated_workspace(reservation)?;
    assert!(gents_session::workspace::flush_workspace_docs(
        &creator_access,
        &denied_docs,
        creator.did(),
        &invalid_action
    )
    .await
    .is_err());
    assert_eq!(
        target.exists(),
        gents_lean_vocab::lean_contract_snapshot()
            .workspace_publication_cases
            .effect_after_failed_reservation
    );
    let workspace = IsolatedWorkspaceDoc {
        path_capability: action.path_capability.clone(),
        workspace_id: "workspace".into(),
        work_unit_id: "unit".into(),
        repository_id: "repository".into(),
        base_sha: "base".into(),
        branch: "branch".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_node_did: user.did().into(),
        writer_principal: writer.did().into(),
        integrator_principal: integrator.did().into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "invocation".into(),
        caused_by_correlation: "correlation".into(),
        route: Default::default(),
    };
    let placement = WorkspacePlacementDoc {
        workspace_id: "workspace".into(),
        owner_node_did: user.did().into(),
        host_path: "/tmp/workspace".into(),
        repository_placement_id: "repository".into(),
        adapter: "git_worktree".into(),
        adapter_version: "1".into(),
        dirty_base: false,
        dirty_base_summary: "".into(),
        provisioning_state: "ready".into(),
        observed_tree_hash: "tree".into(),
    };
    let mut docs = MemoryWorkspaceDocuments::default();
    docs.write_isolated_workspace(workspace)?;
    docs.write_placement(placement)?;
    let mut aborted = docs.load_isolated_workspace("workspace")?.unwrap();
    aborted.workspace_id = "aborted-workspace".into();
    let abort: anyhow::Result<()> = creator_access
        .transact_write("test.workspace.abort_publication", |txn| {
            let aborted = &aborted;
            let parent = &agent_docs[0];
            let writer_did = writer.did();
            Box::pin(async move {
                let response = txn
                    .execute(&isolated_workspace_upsert_mutation(aborted))
                    .await?;
                let rows = gents_store::graphql::mutation_documents(
                    &response,
                    "upsert_IsolatedWorkspace",
                )?
                .unwrap();
                let id = rows[0]["_docID"].as_str().unwrap();
                txn.add_resource_relationship("IsolatedWorkspace", id, "parent", "agents", parent)
                    .await?;
                txn.add_actor_relationship("IsolatedWorkspace", id, "writer", &writer_did.parse()?)
                    .await?;
                anyhow::bail!("forced rollback after staged relationships")
            })
        })
        .await;
    assert!(abort.is_err());
    let rows = creator_access
        .execute(
            r#"{ IsolatedWorkspace(filter: {workspace_id: {_eq: "aborted-workspace"}}) {_docID} }"#,
        )
        .await?;
    let visible = !rows["data"]["IsolatedWorkspace"]
        .as_array()
        .unwrap()
        .is_empty();
    assert_eq!(
        visible,
        gents_lean_vocab::lean_contract_snapshot()
            .workspace_publication_cases
            .aborted_document_visible
    );
    gents_session::workspace::flush_workspace_docs(&creator_access, &docs, creator.did(), &action)
        .await?;
    let workspace_rows = writer_access
        .execute("{ IsolatedWorkspace { _docID } }")
        .await?;
    let workspace_doc = workspace_rows["data"]["IsolatedWorkspace"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    creator_access
        .delete_actor_relationship(
            "IsolatedWorkspace",
            &workspace_doc,
            "writer",
            &writer.did().parse()?,
        )
        .await?;
    gents_session::workspace::flush_workspace_docs(&creator_access, &docs, creator.did(), &action)
        .await?;
    let visible_after_retry = !writer_access
        .execute("{ IsolatedWorkspace { _docID } }")
        .await?["data"]["IsolatedWorkspace"]
        .as_array()
        .unwrap()
        .is_empty();
    assert_eq!(
        visible_after_retry,
        gents_lean_vocab::lean_contract_snapshot()
            .workspace_publication_cases
            .revoked_writer_restored,
        "duplicate workspace publication changed a revoked role"
    );
    creator_access
        .add_actor_relationship(
            "IsolatedWorkspace",
            &workspace_doc,
            "writer",
            &writer.did().parse()?,
        )
        .await?;
    creator_access
        .delete_actor_relationship(
            "IsolatedWorkspace",
            &workspace_doc,
            "reader",
            &reader.did().parse()?,
        )
        .await?;
    gents_session::workspace::flush_workspace_docs(&creator_access, &docs, creator.did(), &action)
        .await?;
    let reader_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        reader.did().parse()?,
        reader.as_ref(),
    )?;
    let visible = !reader_access
        .execute("{ IsolatedWorkspace {_docID} }")
        .await?["data"]["IsolatedWorkspace"]
        .as_array()
        .unwrap()
        .is_empty();
    assert_eq!(
        visible,
        gents_lean_vocab::lean_contract_snapshot()
            .workspace_publication_cases
            .revoked_reader_restored
    );
    creator_access
        .add_actor_relationship(
            "IsolatedWorkspace",
            &workspace_doc,
            "reader",
            &reader.did().parse()?,
        )
        .await?;
    overlapping_native_workspace_fences_conflict(
        &node,
        writer.as_ref(),
        &writer_access,
        &workspace_doc,
        user.did(),
    )
    .await?;
    let mut requests = Vec::new();
    for name in ["one", "two"] {
        let response = writer_access.write("test.workspace.request", &format!(r#"mutation {{create_AgentRequest(input: {{request_id: "{name}", purpose: "normal", node_did: "{}", requester_did: "{}", agent_did: "{}", session_id: "session", content: "work", created_at: "2026-11-05T00:00:00Z", workspace_id: "workspace", workspace_owner_node_did: "{}", workspace_authority: "readWrite", lifecycle_state: "pending"}}) {{_docID}}}}"#, user.did(), user.did(), writer.did(), user.did())).await?;
        requests.push(gents_store::graphql::created_doc_id(
            &response,
            "AgentRequest",
        )?);
    }
    let lineage = WorkspaceLineage {
        workspace_id: Some("workspace".into()),
        workspace_owner_node_did: Some(user.did().into()),
        workspace_authority: Some("readWrite".into()),
        workspace_seal_hash: None,
    };
    let (first, second) = tokio::join!(
        materialize_workspace_binding(&node, "one", &requests[0], writer.as_ref(), &lineage),
        materialize_workspace_binding(&node, "two", &requests[1], writer.as_ref(), &lineage)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "only one concurrent exclusive admission may commit"
    );
    let result = writer_access
        .execute(
            &gents_session::workspace::overlay::workspace_bindings_for_parent_query(&workspace_doc),
        )
        .await?;
    let rows = result["data"]["WorkspaceBinding"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let result = writer_access
        .execute("{ WorkspaceBinding { _docID } }")
        .await?;
    let binding_doc = result["data"]["WorkspaceBinding"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    for case in &gents_lean_vocab::lean_contract_snapshot().workspace_child_cases {
        let actor = match case.actor.as_str() {
            "user" => user.did(),
            "creator" => creator.did(),
            "writer" => writer.did(),
            "integrator" => integrator.did(),
            "reader" => reader.did(),
            "other" => other.did(),
            name => panic!("unknown actor {name}"),
        };
        let permission = case.permission.as_str();
        let node_permission = match permission {
            "read" => acp::nac::NodePermission::DocumentRead,
            "update" => acp::nac::NodePermission::DocumentUpdate,
            "delete" => acp::nac::NodePermission::DocumentDelete,
            other => panic!("unknown permission {other}"),
        };
        let subject = actor.parse()?;
        let decisions = admin
            .transact_readonly("test.workspace.access", |txn| {
                let subject = &subject;
                let doc = binding_doc.clone();
                Box::pin(async move {
                    txn.observe_document_access(
                        subject,
                        "WorkspaceBinding",
                        &[doc],
                        defra_node::DocumentAccessMode::Named {
                            permission: permission.into(),
                            node_permission,
                        },
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(
            decisions == [defra_node::DocumentAccessDecision::Allowed],
            case.expected,
            "{case:?}"
        );
    }
    let cases = &gents_lean_vocab::lean_contract_snapshot().workspace_admission_cases;
    assert_eq!(
        cases
            .iter()
            .find(|case| case.complete && !case.occupied)
            .unwrap()
            .admitted,
        true
    );
    assert_eq!(
        cases
            .iter()
            .find(|case| case.complete && case.occupied)
            .unwrap()
            .admitted,
        false
    );
    writer_access.write("test.workspace.seal", &format!(r#"mutation {{update_IsolatedWorkspace(docID: "{}", input: {{lifecycle_state: "sealed"}}) {{_docID}}}}"#, workspace_doc)).await?;
    let other_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        other.did().parse()?,
        other.as_ref(),
    )?;
    for case in &gents_lean_vocab::lean_contract_snapshot().workspace_cleanup_cases {
        let name = case.name.as_str();
        if name == "unauthorized" {
            writer_access.write("test.workspace.release", &format!(r#"mutation {{update_WorkspaceBinding(docID: "{}", input: {{lifecycle_state: "released"}}) {{_docID}}}}"#, binding_doc)).await?;
        }
        let access = if name == "unauthorized" {
            &other_access
        } else {
            &writer_access
        };
        let result = gents_session::workspace::runtime::claim_workspace_cleanup(
            access,
            "workspace",
            user.did(),
        )
        .await;
        assert_eq!(result.is_ok(), case.begins, "{case:?}: {result:?}");
    }
    let state = writer_access
        .execute("{ IsolatedWorkspace { lifecycle_state } }")
        .await?;
    assert_eq!(
        state["data"]["IsolatedWorkspace"][0]["lifecycle_state"],
        "cleaning"
    );
    let stale = gents_session::workspace::flush_workspace_docs(
        &creator_access,
        &docs,
        creator.did(),
        &action,
    )
    .await;
    assert_eq!(
        stale.is_ok(),
        gents_lean_vocab::lean_contract_snapshot()
            .workspace_publication_cases
            .stale_publication_allowed,
        "stale callback publication changed a cleaning workspace"
    );
    let detached = WorkspaceBindingDoc {
        workspace_doc_id: Some(workspace_doc.clone()),
        binding_id: "detached-binding".into(),
        workspace_id: "workspace".into(),
        request_id: "detached-request".into(),
        request_doc_id: "detached-request-doc".into(),
        authority: "readOnly".into(),
        owner_node_did: user.did().into(),
        seal_hash: None,
        lifecycle_state: "active".into(),
    };
    let declaration: gents_documents::WriteToolDecl = serde_json::from_value(serde_json::json!({
        "tool_name":"create_hidden_binding", "collection":"WorkspaceBinding",
        "fields":[{"name":"binding_id"},{"name":"workspace_id"},{"name":"workspace_doc_id"}]
    }))?;
    let tool = gents_tools::builtin::defra_write::BoundedWriteTool::new(
        writer_access.clone(),
        declaration,
    );
    let args = serde_json::from_value(serde_json::json!({
        "binding_id":"tool-hidden", "workspace_id":"workspace", "workspace_doc_id":workspace_doc
    }))?;
    let allowed = gents_lean_vocab::lean_contract_snapshot()
        .workspace_datastore_write_cases
        .iter()
        .find(|case| case.collection == "WorkspaceBinding")
        .map(|case| case.allowed)
        .context("missing modeled workspace write case")?;
    assert_eq!(
        gents_loop::tool::Tool::call(&tool, args).await.is_ok(),
        allowed
    );
    let response = writer_access
        .write(
            "test.workspace.detached_child",
            &workspace_binding_upsert_mutation(&detached),
        )
        .await?;
    let rows =
        gents_store::graphql::mutation_documents(&response, "upsert_WorkspaceBinding")?.unwrap();
    let detached_doc = rows[0]["_docID"].as_str().unwrap().to_owned();
    let integrator_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        integrator.did().parse()?,
        integrator.as_ref(),
    )?;
    let query =
        gents_session::workspace::overlay::workspace_bindings_for_parent_query(&workspace_doc);
    let writer_rows = writer_access.execute(&query).await?;
    let integrator_rows = integrator_access.execute(&query).await?;
    assert!(writer_rows["data"]["WorkspaceBinding"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["binding_id"] == "detached-binding"));
    assert!(!integrator_rows["data"]["WorkspaceBinding"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["binding_id"] == "detached-binding"));
    writer_access
        .add_resource_relationship(
            "WorkspaceBinding",
            &detached_doc,
            "parent",
            "workspaces",
            &workspace_doc,
        )
        .await?;
    writer_access
        .delete_resource_relationship(
            "WorkspaceBinding",
            &detached_doc,
            "parent",
            "workspaces",
            &workspace_doc,
        )
        .await?;
    for case in &gents_lean_vocab::lean_contract_snapshot().workspace_child_revocation_cases {
        let actor = match case.actor.as_str() {
            "user" => user.did(),
            "creator" => creator.did(),
            "writer" => writer.did(),
            "integrator" => integrator.did(),
            "reader" => reader.did(),
            "other" => other.did(),
            name => panic!("unknown actor {name}"),
        };
        let subject = actor.parse()?;
        let decisions = admin
            .transact_readonly("test.workspace.detached_access", |txn| {
                let subject = &subject;
                let doc = detached_doc.clone();
                Box::pin(async move {
                    txn.observe_document_access(
                        subject,
                        "WorkspaceBinding",
                        &[doc],
                        defra_node::DocumentAccessMode::Named {
                            permission: "read".into(),
                            node_permission: acp::nac::NodePermission::DocumentRead,
                        },
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(
            decisions == [defra_node::DocumentAccessDecision::Allowed],
            case.read,
            "{case:?}"
        );
    }
    node.shutdown().await?;
    Ok(())
}

/// This uses two native handles so the local mutation gate cannot serialize the
/// snapshots before either transaction writes the shared workspace revision.
async fn overlapping_native_workspace_fences_conflict(
    node: &defra_node::Node,
    writer: &dyn NodeIdentity,
    writer_access: &gents_store::config_client::ConfigAccess,
    workspace_doc: &str,
    owner: &str,
) -> anyhow::Result<()> {
    let signer = writer
        .managed_agent_signer()
        .expect("selected writer signer");
    let first = node.begin_transaction(false).await?;
    let second = node.begin_transaction(false).await?;
    let context =
        defra_core::RequestContext::default().with_identity(Some(writer.did().to_owned()));
    let query =
        gents_session::workspace::overlay::workspace_bindings_for_parent_query(workspace_doc);
    for handle in [&first, &second] {
        let response = node
            .execute_request_in_txn(
                signer.native().attach(
                    defra_node::QueryRequest::new(&query)
                        .with_identity(Some(writer.did().parse()?)),
                ),
                handle,
            )
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "native binding inventory failed: {:?}",
            response.errors
        );
        let data = response
            .data
            .context("native binding inventory omitted data")?;
        anyhow::ensure!(
            data["WorkspaceBinding"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "conflict fixture must start with an empty binding inventory"
        );
    }
    for (handle, name) in [
        (&first, "native-overlap-one"),
        (&second, "native-overlap-two"),
    ] {
        let fence = format!(
            r#"mutation {{ update_IsolatedWorkspace(docID: "{}", input: {{binding_revision: "{}"}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(workspace_doc),
            name
        );
        let response = node
            .execute_request_in_txn(
                signer.native().attach(
                    defra_node::QueryRequest::new(&fence)
                        .with_identity(Some(writer.did().parse()?)),
                ),
                handle,
            )
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "native workspace fence failed: {:?}",
            response.errors
        );
        let document = WorkspaceBindingDoc {
            workspace_doc_id: Some(workspace_doc.to_owned()),
            binding_id: name.into(),
            workspace_id: "workspace".into(),
            request_id: name.into(),
            request_doc_id: name.into(),
            authority: "readWrite".into(),
            owner_node_did: owner.into(),
            seal_hash: None,
            lifecycle_state: "active".into(),
        };
        let mutation = workspace_binding_upsert_mutation(&document);
        let response = node
            .execute_request_in_txn(
                signer.native().attach(
                    defra_node::QueryRequest::new(&mutation)
                        .with_identity(Some(writer.did().parse()?)),
                ),
                handle,
            )
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "native binding write failed: {:?}",
            response.errors
        );
        let envelope = serde_json::to_value(response)?;
        let rows = gents_store::graphql::mutation_documents(&envelope, "upsert_WorkspaceBinding")?
            .context("native binding write omitted confirmation")?;
        let [row] = rows else {
            anyhow::bail!("native binding write did not confirm one document")
        };
        let doc = row["_docID"]
            .as_str()
            .context("native binding physical ID missing")?;
        let parent = format!("workspaces:\"{}\"", workspace_doc);
        // DefraDB reports whether the relationship already existed; a fresh
        // binding must receive a new parent edge.
        let existed = node
            .add_dac_actor_relationship_for_in_txn(
                handle,
                &context,
                "WorkspaceBinding",
                doc,
                "parent",
                &parent,
            )
            .await?;
        anyhow::ensure!(!existed, "native binding parent edge already existed");
    }
    node.commit_transaction(&first).await?;
    let conflict = node.commit_transaction(&second).await;
    anyhow::ensure!(
        matches!(&conflict, Err(query::TransactionError::Conflict(_))),
        "overlapping workspace revision did not produce a native transaction conflict: {conflict:?}"
    );
    let _ = node.discard_transaction(&second).await;
    let response = writer_access.execute(&query).await?;
    let rows = response["data"]["WorkspaceBinding"]
        .as_array()
        .context("binding inventory omitted rows")?;
    let cases = &gents_lean_vocab::lean_contract_snapshot().workspace_admission_cases;
    let expected = cases
        .iter()
        .find(|case| case.complete && !case.occupied)
        .map(|case| case.admitted)
        .context("missing modeled empty admission")?;
    assert_eq!(rows.len(), usize::from(expected));
    let [winner] = rows.as_slice() else {
        anyhow::bail!("only one overlapping binding should commit")
    };
    assert_eq!(winner["binding_id"], "native-overlap-one");
    let delete = r#"mutation { delete_WorkspaceBinding(filter: {binding_id: {_eq: "native-overlap-one"}}) {_docID} }"#.to_string();
    writer_access
        .write("test.workspace.conflict.cleanup", &delete)
        .await?;
    Ok(())
}
