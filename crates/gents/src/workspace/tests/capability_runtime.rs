use super::*;
use crate::identity::AgentIdentity;
use crate::lifecycle::{RequestLifecycle, RequestTerminalOutcome, WorkspaceLineage};
use std::sync::Arc;
use std::time::Duration;

async fn query(node: &defra_node::EmbeddedNode, document: &str) -> serde_json::Value {
    let response = node.execute(document).await;
    assert!(!response.has_errors(), "{document}: {:?}", response.errors);
    response.data.expect("database result")
}

async fn runtime_seal_case(unowned: bool) {
    let fx = Fixture::new();
    let db_dir = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(db_dir.path().join("agent.key"), None)
            .unwrap();
    let did = identity.did();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(db_dir.path().join("db"))
            .with_node_identity_did(did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node.add_schema("type CapabilityReviewVerdict { workspace_id: String accepted: Boolean }")
        .await
        .unwrap();
    let mut memory = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("runtime-capability", "runtime-unit", "runtime-capability");
    action.path_capability = WorkspacePathCapability::exact_paths(vec!["patch.rs".into()]).unwrap();
    let created = {
        let mut ctx = fx.ctx(&mut memory, git_worktree_caps());
        ctx.writer_principal = did.to_owned();
        ctx.integrator_principal = did.to_owned();
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
        deployment_id: "deploy-1".into(),
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
        "runtime-capability-writer",
        did,
        did,
        "general",
        "runtime-capability-session",
        "Apply the owned patch",
        "interactive",
        &timestamp,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    create.workspace_id = Some(created.workspace.workspace_id.clone());
    create.workspace_authority = Some("readWrite".into());
    create.workspace_owner_deployment_id = Some("deploy-1".into());
    crate::request_admission::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    query(&node, &create.graphql_mutation().unwrap()).await;
    let response = node
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let request = crate::watcher::AgentRequest::try_from(row).unwrap();
    let lineage = WorkspaceLineage {
        workspace_id: request.workspace_id.clone(),
        workspace_authority: request.workspace_authority.clone(),
        workspace_owner_deployment_id: request.workspace_owner_deployment_id.clone(),
        workspace_seal_hash: None,
    };
    materialize_workspace_binding(
        &node,
        &request.request_id,
        &request.doc_id,
        did,
        &lineage,
        Some("deploy-1"),
    )
    .await
    .unwrap();
    let mut owner =
        RequestLifecycle::new_with_agent_did(node.clone(), "general", did, request, 120);
    owner.claim().await.unwrap();
    let writer = crate::streaming::DefraStreamWriter::new(node.clone(), did, Duration::ZERO);
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
    // A persisted domain-level acceptance is input/evidence, never path authority.
    query(&node, r#"mutation { create_CapabilityReviewVerdict(input: { workspace_id: "runtime-capability", accepted: true }) { _docID } }"#).await;
    let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let before_readme = fs::read(fx.repo.join("README.md")).unwrap();
    let result = seal_on_writer_success(&node, &request, Some(&fx.repo)).await;
    if unowned {
        let error = result
            .expect_err("accepted verdict cannot authorize an unowned log")
            .to_string();
        assert!(error.contains(".tmp-build/test-build.log"), "{error}");
        owner
            .terminalize_owned(&writer, RequestTerminalOutcome::Failed, Some(&error))
            .await
            .unwrap();
        let mut integrator = request.clone();
        integrator.workspace_authority = Some("integrate".into());
        let denial = integrate_on_integrator_success(&node, &integrator, Some(&fx.repo))
            .await
            .expect_err("unsealed rejected writer cannot integrate")
            .to_string();
        assert!(
            denial.contains("Sealed") || denial.contains("sealed"),
            "{denial}"
        );
    } else {
        result.expect("owned untracked file seals through the runtime");
        owner
            .terminalize_owned(&writer, RequestTerminalOutcome::Completed, None)
            .await
            .unwrap();
    }
    let durable = query(
        &node,
        r#"{
        CapabilityReviewVerdict { accepted }
        IsolatedWorkspace { lifecycle_state seal_hash path_capability }
        WorkspaceReceipt { kind path_capability_digest changed_files }
        AgentRequest { lifecycle_state }
        AgentResponse { status content error_message }
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
    let responses = durable["AgentResponse"].as_array().unwrap();
    assert_eq!(responses.len(), 1);
    let receipts = durable["WorkspaceReceipt"].as_array().unwrap();
    if unowned {
        assert_eq!(responses[0]["status"], "error");
        assert!(responses[0]["error_message"]
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
    node.shutdown().await;
    // Preserve the DB directory for Defra background tasks, as migration tests do.
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
