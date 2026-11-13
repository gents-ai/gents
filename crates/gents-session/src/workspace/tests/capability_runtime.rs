use super::*;
use crate::request::lifecycle::{RequestLifecycle, RequestTerminalOutcome, WorkspaceLineage};
use crate::workspace::{git, git_worktree_caps, Fixture};
use anyhow::Context;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn workspace_completion_requires_selected_managed_agent() {
    let db = gents_inference::test_support::test_db(
        "workspace-completion-selected-agent",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let runtime = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let cases = gents_lean_vocab::families::workspace_and_file_requests_completion_actor::cases();
    for case in cases {
        let selected = if case.input.selected.is_empty() {
            ""
        } else {
            agent.did()
        };
        let held: &dyn NodeIdentity = if case.input.held == "agent" {
            agent.as_ref()
        } else {
            &runtime
        };
        case.check(super::super::runtime::completion_access(&node, held, selected).is_ok());
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_completion_reads_keep_selected_agent_visibility() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("runtime.key"),
        None,
    )?;
    let selected_binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let other_binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let selected = gents_store::identity::resolve_agent_identity(&runtime, &selected_binding)?;
    let other = gents_store::identity::resolve_agent_identity(&runtime, &other_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(runtime.did().to_owned())
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
    for schema in [
        gents_protocol::schemas::ISOLATED_WORKSPACE,
        gents_protocol::schemas::WORKSPACE_PLACEMENT,
        gents_protocol::schemas::WORKSPACE_BINDING,
        gents_protocol::schemas::WORKSPACE_RECEIPT,
        gents_protocol::schemas::REPOSITORY_PLACEMENT,
    ] {
        node.add_collection_as(&context, schema).await?;
    }
    let admin =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for actor in [selected.did(), other.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let access = |identity: &dyn NodeIdentity| {
        gents_store::config_client::ConfigAccess::local_managed_agent(
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
        route: Default::default(),
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
    let cases = gents_lean_vocab::families::workspace_completion_access::cases();
    for case in &cases {
        let access = match case.input.actor.as_str() {
            "agent" => &selected_access,
            "other" => &other_access,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let docs = super::super::runtime::load_docs_with_access(access, "workspace", runtime.did())
            .await?;
        case.check(
            gents_lean_vocab::families::workspace_completion_access::CompletionAccessOutput {
                read: docs.load_isolated_workspace("workspace")?.is_some(),
            },
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

async fn query(
    admin: &gents_store::config_client::ConfigAccess,
    document: &str,
) -> serde_json::Value {
    let response = admin.execute(document).await.unwrap();
    assert!(response.get("errors").is_none(), "{document}: {response}");
    response.get("data").cloned().expect("database result")
}

async fn mutate(
    admin: &gents_store::config_client::ConfigAccess,
    operation: &'static str,
    document: &str,
) -> serde_json::Value {
    let response = admin.write(operation, document).await.unwrap();
    assert!(response.get("errors").is_none(), "{document}: {response}");
    response.get("data").cloned().expect("database result")
}

async fn runtime_seal_case(unowned: bool) {
    let fx = Fixture::new();
    let db = gents_inference::test_support::test_db(
        "runtime-seal",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let binding =
        gents_store::identity::provision_agent_identity(db.node_identity.as_ref()).unwrap();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let admin =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), did.parse().unwrap());
    for permission in [
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
        gents_store::config_client::NodePermission::DacRelationAdd,
        gents_store::config_client::NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&agent.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    node.add_collection("type CapabilityReviewVerdict { workspace_id: String accepted: Boolean }")
        .await
        .unwrap();
    let node_data = mutate(
        &admin,
        "test.capability.node",
        &format!(
            r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
            did
        ),
    )
    .await;
    let node_doc =
        gents_store::graphql::created_doc_id(&serde_json::json!({"data":node_data}), "Node")
            .unwrap();
    let agent_data = mutate(&admin, "test.capability.agent", &format!(r#"mutation {{ create_Agent(input: {{node_did: "{}", agent_did: "{}", creation_nonce: "{}", enabled: true}}) {{_docID}} }}"#, did, agent.did(), binding.creation_nonce)).await;
    let agent_doc =
        gents_store::graphql::created_doc_id(&serde_json::json!({"data":agent_data}), "Agent")
            .unwrap();
    admin
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    admin
        .add_actor_relationship("Agent", &agent_doc, "reader", &agent.did().parse().unwrap())
        .await
        .unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let mut memory = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("runtime-capability", "runtime-unit", "runtime-capability");
    action.writer_agent_did = Some(agent.did().to_owned());
    action.writer_agent_doc_id = Some(agent_doc.clone());
    action.integrator_agent_did = Some(agent.did().to_owned());
    action.integrator_agent_doc_id = Some(agent_doc);
    action.path_capability = WorkspacePathCapability::exact_paths(vec!["patch.rs".into()]).unwrap();
    let created = {
        let mut ctx = fx.ctx(&mut memory, git_worktree_caps());
        ctx.owner_node_did = did.to_owned();
        ctx.repository.owner_node_did = did.to_owned();
        ctx.writer_principal = agent.did().to_owned();
        ctx.integrator_principal = agent.did().to_owned();
        execute_create_workspace_plan(
            &emit_create_workspace_plan(action.clone()),
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
    crate::workspace::flush_workspace_docs(&agent_access, &memory, agent.did(), &action)
        .await
        .unwrap();
    let repo_mutation = repository_placement_upsert_mutation(&repository, &timestamp).unwrap();
    let agent_did = agent.did().to_owned();
    admin
        .transact_write("test.workspace.repository", |txn| {
            let repo_mutation = &repo_mutation;
            let node_doc = &node_doc;
            let agent_did = &agent_did;
            Box::pin(async move {
                let response = txn.execute(repo_mutation).await?;
                let rows = gents_store::graphql::mutation_documents(
                    &response,
                    "upsert_RepositoryPlacement",
                )?
                .context("repository mutation omitted confirmation")?;
                let [row] = rows else {
                    anyhow::bail!("repository mutation did not affect one document")
                };
                let repo_doc = row["_docID"]
                    .as_str()
                    .context("repository physical ID missing")?;
                txn.add_resource_relationship(
                    "RepositoryPlacement",
                    repo_doc,
                    "parent",
                    "nodes",
                    node_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "RepositoryPlacement",
                    repo_doc,
                    "reader",
                    &agent_did.parse()?,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
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
    crate::request::admission::sign_agent_request_create(db.node_identity.as_ref(), &mut create)
        .await
        .unwrap();
    mutate(
        &admin,
        "test.capability.request",
        &create.graphql_mutation().unwrap(),
    )
    .await;
    let response = query(
        &admin,
        &format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ),
    )
    .await;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["AgentRequest"][0].clone()).unwrap();
    let request = gents_protocol::row::AgentRequest::try_from(row).unwrap();
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
    let mut owner = RequestLifecycle::new_with_node_did(node.clone(), request, 120);
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
    mutate(&admin, "test.capability.verdict", r#"mutation { create_CapabilityReviewVerdict(input: { workspace_id: "runtime-capability", accepted: true }) { _docID } }"#).await;
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
            &admin,
            "{ WorkspaceReceipt { _docID _version { cid height fieldName } } }",
        )
        .await;
        let document = &receipts["WorkspaceReceipt"][0];
        let version =
            gents_store::graphql::document_composite_version(document, "workspace receipt")
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
        &admin,
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
    let runtime = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("runtime.key"),
        None,
    )?;
    let first_binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let second_binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let first = gents_store::identity::resolve_agent_identity(&runtime, &first_binding)?;
    let second = gents_store::identity::resolve_agent_identity(&runtime, &second_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(runtime.did().to_owned())
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
    let admin =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for actor in [first.did(), second.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let second_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        second.did().parse()?,
        second.as_ref(),
    )?;
    let first_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        first.did().parse()?,
        first.as_ref(),
    )?;
    for case in gents_lean_vocab::families::workspace_and_file_requests_liveness::cases() {
        let visible = case.input.visible;
        let terminal = case.input.terminal;
        let state = if terminal { "completed" } else { "pending" };
        let request_id = format!("old-{visible}-{terminal}");
        let response = second_access.write("test.workspace.foreign_request", &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{request_id}", purpose: "normal", node_did: "{}", requester_did: "{}", agent_did: "{}", session_id: "session", content: "work", created_at: "2026-11-05T00:00:00Z", workspace_id: "workspace", workspace_owner_node_did: "{}", lifecycle_state: "{state}"}}) {{_docID}} }}"#, runtime.did(), runtime.did(), second.did(), runtime.did())).await?;
        let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
        let binding = WorkspaceBindingDoc {
            workspace_doc_id: None,
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
        assert_eq!(result.is_err(), !visible, "{result:?}");
        case.check(
            gents_lean_vocab::families::workspace_and_file_requests_liveness::LivenessOutput {
                may_release: result.unwrap_or(false),
            },
        );
    }
    node.shutdown().await?;
    Ok(())
}
