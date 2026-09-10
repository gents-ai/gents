use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use crate::ensure_runtime_schemas;
use crate::workspace::{
    action_plan_canonical_json, emit_create_workspace_plan, execute_create_workspace_plan,
    parse_action_plan_json, ActionJournalEntry, ActionJournalState, CreateWorkspaceAction,
    CreationPolicy, HostAction, HostExecutorContext, MemoryWorkspaceDocuments,
    RepositoryPlacementRef, WorkspaceAdapterKind, WorkspaceDocuments, CAP_CLONE_ARTIFACTS,
    CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE,
};

use super::claim::invocation_is_claimable;
use super::documents::{
    strip_secret_fields, succeeded_missing_result, succeeded_repair_cutoff,
    validate_callback_binding, CallbackBindingDoc, CallbackInvocationDoc, CallbackModuleDoc,
    CallbackResultInvocationRow,
};
use super::run::{
    apply_planner_deny, can_emit_callback_result, can_start_executing, emit_plan_from_source,
    journal_has_started_host_execution, plan_from_callback, resolve_action_plan,
    resolve_action_plan_with_module,
};
use super::wasm::{
    compute_module_id, fixture_create_workspace_wasm, fixture_wasm_is_stub, invoke_wasm_planner,
    plan_from_wasm_module, validate_callback_module, CallbackModuleLimits, MAX_WASM_BYTES,
};
use super::{
    LIFECYCLE_DENIED, LIFECYCLE_FAILED, LIFECYCLE_PENDING, LIFECYCLE_RUNNING, LIFECYCLE_SUCCEEDED,
};

fn binding() -> CallbackBindingDoc {
    serde_json::from_value(json!({"binding_id":"bind-1","agent_did":"did:key:zWriter","event_source_id":"events","callback_id":"cb-1","input_fields":["work_unit_id","repository_id","base_sha","branch","owned_files"]})).unwrap()
}
fn callback() -> crate::document_config::Callback {
    serde_json::from_value(json!({"callback_id":"cb-1","agent_did":"did:key:zWriter","handler":{"kind":"built_in","emitter":"create_workspace"},"capabilities":["create_workspace","observe_dirty_base","clone_artifacts"]})).unwrap()
}

#[test]
fn executing_action_waits_for_prior_result_docs() {
    let illegal = vec![
        ActionJournalEntry::new(0, ActionJournalState::Validated),
        ActionJournalEntry::new(1, ActionJournalState::Executing),
    ];
    assert!(!can_start_executing(&illegal, 1));

    let legal = vec![
        ActionJournalEntry::new(0, ActionJournalState::ResultDocsWritten),
        ActionJournalEntry::new(1, ActionJournalState::Executing),
    ];
    assert!(can_start_executing(&legal, 1));
    assert!(can_start_executing(&[], 0));
}

#[test]
fn callback_result_requires_succeeded_complete_journal_and_docs() {
    let journal = vec![ActionJournalEntry::new(
        0,
        ActionJournalState::ResultDocsWritten,
    )];
    let workspace = crate::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::workspace::WorkspacePathCapability::exact_paths(Vec::new())
            .unwrap(),
        workspace_id: "ws-1".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: "abc".into(),
        branch: "topic".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_agent_did: "deploy-1".into(),
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
    };
    let placement = crate::workspace::WorkspacePlacementDoc {
        workspace_id: "ws-1".into(),
        owner_agent_did: "deploy-1".into(),
        host_path: "/tmp/ws".into(),
        repository_placement_id: "repo-1".into(),
        adapter: "git_worktree".into(),
        adapter_version: "gents-workspace-adapter/1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "{}".into(),
        observed_tree_hash: "tree".into(),
    };
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        Some(&workspace),
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &[],
        Some(&workspace),
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        None,
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&workspace),
        None
    ));
    assert!(can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&workspace),
        Some(&placement)
    ));
}

#[test]
fn non_owner_does_not_claim_invocation_or_workspace_request() {
    let invocation = CallbackInvocationDoc {
        invocation_id: "inv-1".into(),
        owner_agent_did: "deploy-owner".into(),
        callback_id: "cb-1".into(),
        input: json!({}),
        origin: crate::document_config::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    assert!(!invocation_is_claimable("deploy-replica", &invocation));
    assert!(invocation_is_claimable("deploy-owner", &invocation));
}

#[test]
fn apply_rejects_secret_bearing_source_fields() {
    let mut secret = binding();
    secret.input_fields = vec!["work_unit_id".into(), "api_token".into()];
    assert!(validate_callback_binding(&secret).is_err());
    assert!(validate_callback_binding(&binding()).is_ok());
    let stripped = strip_secret_fields(json!({"branch": "topic", "api_token": "secret"}));
    assert_eq!(stripped["branch"], "topic");
    assert!(stripped.get("api_token").is_none());
}

#[test]
fn apply_rejects_secret_bearing_filter_fields() {
    let error = super::documents::reject_secret_bearing_callback_fields(
        "bind-1",
        Some(r#"{ api_token: { _eq: "x" } }"#),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("secret-bearing"), "{error}");
    assert!(super::documents::reject_secret_bearing_callback_fields(
        "bind-1",
        Some(r#"{ work_unit_id: { _eq: "TOKEN" } }"#),
        None
    )
    .is_ok());
}

#[test]
fn succeeded_without_result_repair_is_windowed_and_batched() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(succeeded_repair_cutoff(now), "2026-08-20T12:00:00Z");

    let row = |id: &str| CallbackInvocationDoc {
        invocation_id: id.into(),
        owner_agent_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        input: json!({}),
        origin: crate::document_config::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: id.into(),
            source_version: Some("created".into()),
        },
        idempotency_key: format!("bind-1:{id}:created"),
        lifecycle_state: LIFECYCLE_SUCCEEDED.into(),
        attempts: Some(1),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let missing = succeeded_missing_result(
        vec![row("inv-ok"), row("inv-gap")],
        &["inv-ok".to_string()].into_iter().collect(),
    );
    assert_eq!(
        missing
            .iter()
            .map(|row| row.invocation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["inv-gap"]
    );

    let projected: Vec<CallbackResultInvocationRow> = serde_json::from_value(json!([
        { "invocation_id": "inv-ok" },
        { "invocation_id": "inv-gap" }
    ]))
    .expect("narrow CallbackResult projection should decode without provenance fields");
    assert_eq!(
        projected
            .iter()
            .map(|row| row.invocation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["inv-ok", "inv-gap"]
    );
}

#[test]
fn callback_handler_accepts_module_and_rejects_mixed_handlers() {
    let mut value = serde_json::to_value(callback()).unwrap();
    value["handler"] = json!({"kind":"module","module_id":"module-1"});
    assert!(serde_json::from_value::<crate::document_config::Callback>(value.clone()).is_ok());
    value["handler"]["emitter"] = json!("create_workspace");
    assert!(serde_json::from_value::<crate::document_config::Callback>(value).is_err());
}

#[test]
fn recovery_reuses_stored_action_plan() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-stored"
    });
    let plan = emit_plan_from_source(&callback(), &source).unwrap();
    let mut invocation = CallbackInvocationDoc {
        invocation_id: "inv-1".into(),
        owner_agent_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        input: json!({}),
        origin: crate::document_config::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_RUNNING.into(),
        attempts: Some(1),
        action_plan: Some(serde_json::to_string(&plan).unwrap()),
        action_journal: Some(
            serde_json::to_string(&[ActionJournalEntry::new(0, ActionJournalState::Executing)])
                .unwrap(),
        ),
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let mutated = json!({
        "owned_files": ["other.md"],
        "work_unit_id": "unit-OTHER",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic-OTHER",
        "workspace_id": "ws-mutated"
    });
    let resolved = resolve_action_plan(&invocation, &callback(), &mutated).unwrap();
    match &resolved.actions[0] {
        crate::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-stored");
            assert_eq!(
                action.path_capability,
                crate::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
                "recovery cannot expand the stored manifest from changed source content"
            );
            assert_eq!(action.branch, "topic");
        }
        crate::workspace::HostAction::SealWorkspace(_)
        | crate::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::workspace::HostAction::IntegrateWorkspace(_)
        | crate::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
    invocation.action_plan = None;
    let missing = resolve_action_plan(&invocation, &callback(), &mutated).unwrap_err();
    assert!(missing.contains("missing stored ActionPlan"), "{missing}");
}

#[test]
fn wasm_recovery_reuses_stored_plan_without_reloading_module() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-stored"
    });
    let plan = emit_plan_from_source(&callback(), &source).unwrap();
    let journal = vec![ActionJournalEntry::new(0, ActionJournalState::Executing)];
    let invocation = CallbackInvocationDoc {
        invocation_id: "inv-wasm-recover".into(),
        owner_agent_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        input: json!({}),
        origin: crate::document_config::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_RUNNING.into(),
        attempts: Some(1),
        action_plan: Some(action_plan_canonical_json(&plan).unwrap()),
        action_journal: Some(serde_json::to_string(&journal).unwrap()),
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let mut wasm_binding = callback();
    wasm_binding.handler = crate::document_config::CallbackHandler::Module {
        module_id: "mod-gone".into(),
    };
    let mutated = json!({
        "owned_files": ["other.md"],
        "work_unit_id": "unit-OTHER",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic-OTHER",
        "workspace_id": "ws-mutated"
    });
    let resolved =
        resolve_action_plan_with_module(&invocation, &wasm_binding, &mutated, None).unwrap();
    match &resolved.actions[0] {
        crate::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-stored");
            assert_eq!(
                action.path_capability,
                crate::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
                "recovery cannot expand the stored manifest from changed source content"
            );
        }
        crate::workspace::HostAction::SealWorkspace(_)
        | crate::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::workspace::HostAction::IntegrateWorkspace(_)
        | crate::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
    assert!(journal_has_started_host_execution(&journal));

    let mut denied = invocation.clone();
    apply_planner_deny(&mut denied, "CallbackModule mod-gone not found");
    assert_eq!(denied.lifecycle_state, LIFECYCLE_FAILED);
    assert_eq!(denied.action_journal, invocation.action_journal);
    assert_eq!(denied.action_plan, invocation.action_plan);
    assert!(denied.error.as_deref().unwrap().contains("mod-gone"));

    let mut pre_exec = invocation.clone();
    pre_exec.action_journal = Some("[]".into());
    pre_exec.action_plan = None;
    apply_planner_deny(&mut pre_exec, "CallbackModule missing");
    assert_eq!(pre_exec.lifecycle_state, LIFECYCLE_DENIED);
    assert_eq!(pre_exec.action_journal.as_deref(), Some("[]"));
}

#[test]
fn builtin_emitter_accepts_assignment_and_base_revision_aliases() {
    let source = json!({
        "owned_files": [],
        "assignment_id": "cluster:patch",
        "repository_id": "defending_code",
        "base_revision": "abc123"
    });
    let plan = emit_plan_from_source(&callback(), &source).expect("plan");
    match &plan.actions[0] {
        crate::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.work_unit_id, "cluster:patch");
            assert_eq!(action.base_sha, "abc123");
            assert_eq!(action.branch, "gents/cluster-patch");
            assert_eq!(action.workspace_id, "cluster:patch");
        }
        _ => panic!("expected create_workspace"),
    }
}

#[test]
fn builtin_emitter_builds_create_workspace_plan() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1"
    });
    let plan = emit_plan_from_source(&callback(), &source).expect("plan");
    let encoded = serde_json::to_string(&plan).unwrap();
    assert!(!encoded.contains("host_path"));
    assert_eq!(plan.abi, 1);
    match &plan.actions[0] {
        crate::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-1");
            assert_eq!(action.work_unit_id, "unit-1");
        }
        crate::workspace::HostAction::SealWorkspace(_)
        | crate::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::workspace::HostAction::IntegrateWorkspace(_)
        | crate::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct GitFixture {
    _root: TempDir,
    repo: PathBuf,
    base_sha: String,
}

impl GitFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "cb@example.com"]);
        git(&repo, &["config", "user.name", "Callback Test"]);
        fs::write(repo.join("README.md"), "hello\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "init"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);
        Self {
            _root: root,
            repo,
            base_sha,
        }
    }
}

#[test]
fn callback_result_only_after_workspace_docs_are_durable() {
    let fx = GitFixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = CreateWorkspaceAction {
        path_capability: crate::workspace::WorkspacePathCapability::exact_paths(Vec::new())
            .unwrap(),
        workspace_id: "ws-result".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: fx.base_sha.clone(),
        branch: "topic-result".into(),
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
    };
    let plan = emit_create_workspace_plan(action);
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut journal = Vec::new();
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        docs.load_isolated_workspace("ws-result").unwrap().as_ref(),
        docs.load_placement("ws-result").unwrap().as_ref(),
    ));

    let mut ctx = HostExecutorContext {
        owner_agent_did: "deploy-1".into(),
        repository: RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            owner_agent_did: "deploy-1".into(),
            host_path: fx.repo.clone(),
            enabled: true,
        },
        ceiling: fx.repo.parent(),
        capabilities: caps,
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
        documents: &mut docs,
    };
    let outcome = execute_create_workspace_plan(&plan, &mut journal, &mut ctx).expect("provision");
    assert_eq!(
        journal.last().map(|entry| entry.state),
        Some(ActionJournalState::ResultDocsWritten)
    );
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        Some(&outcome.workspace),
        Some(&outcome.placement)
    ));
    assert!(can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&outcome.workspace),
        Some(&outcome.placement)
    ));
}

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node
}

#[tokio::test]
async fn first_seen_source_create_materializes_owner_invocation() {
    let node = test_node().await;
    let owner_agent_did = "did:key:zWriter".to_owned();
    node.add_schema(
        r#"
        type WorkUnit {
            work_unit_id: String
            owned_files: String
            repository_id: String
            base_sha: String
            branch: String
        }
        "#,
    )
    .await
    .expect("WorkUnit schema");

    let binding_mutation = r#"mutation {
        create_Callback(input: {callback_id:"cb-scan",agent_did:"did:key:zWriter",handler:{kind:"built_in",emitter:"create_workspace"},capabilities:["create_workspace","observe_dirty_base"],enabled:true}) {_docID}
        create_EventSource(input: {event_source_id:"events-scan",agent_did:"did:key:zWriter",source_collection:"WorkUnit",event_kind:"created"}) {_docID}
        create_CallbackBinding(input: {binding_id:"bind-scan",agent_did:"did:key:zWriter",event_source_id:"events-scan",callback_id:"cb-scan",input_fields:["work_unit_id","repository_id","base_sha","branch","owned_files"],enabled:true}) {_docID}
    }"#;
    let response = node.execute(&binding_mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let cancel = tokio_util::sync::CancellationToken::new();
    let mut engine =
        super::CallbackEngine::new(node.clone(), owner_agent_did.clone(), None, cancel);
    engine.reconcile_bindings().await;

    let create = r#"mutation {
        create_WorkUnit(input: {
            work_unit_id: "unit-scan",
            owned_files: "[]",
            repository_id: "repo-1",
            base_sha: "abc",
            branch: "topic"
        }) { _docID }
    }"#;
    let created = node.execute(create).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let doc_id = crate::graphql::single_mutation_document(&created, "create_WorkUnit")
        .unwrap()
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("WorkUnit doc id")
        .to_string();

    let mut projection = binding();
    projection.input_fields.clear();
    let mut cache = super::scan::SourceSchemaCache::default();
    let empty = super::scan::fetch_source_doc(
        node.as_ref(),
        &mut cache,
        "WorkUnit",
        &doc_id,
        None,
        &projection,
    )
    .await
    .unwrap();
    assert_eq!(empty, json!({}), "empty input_fields grants no source data");
    projection.input_fields = vec![
        "work_unit_id".into(),
        "repository_id".into(),
        "base_sha".into(),
        "branch".into(),
    ];
    let narrow = super::scan::fetch_source_doc(
        node.as_ref(),
        &mut cache,
        "WorkUnit",
        &doc_id,
        None,
        &projection,
    )
    .await
    .unwrap();
    assert!(narrow.get("owned_files").is_none());
    assert!(
        emit_plan_from_source(&callback(), &narrow).is_err(),
        "omitted manifest cannot authorize workspace paths"
    );
    projection.input_fields.push("owned_files".into());
    let admitted = super::scan::fetch_source_doc(
        node.as_ref(),
        &mut cache,
        "WorkUnit",
        &doc_id,
        None,
        &projection,
    )
    .await
    .unwrap();
    assert!(emit_plan_from_source(&callback(), &admitted).is_ok());

    engine.handle_created_doc("WorkUnit", &doc_id).await;

    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 10
            ) {{
                invocation_id
                owner_agent_did
                lifecycle_state
                idempotency_key
                input
                origin
            }}
        }}"#,
        owner = crate::graphql::escape_graphql_string(&owner_agent_did),
    );
    let response = node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("CallbackInvocation"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["owner_agent_did"], owner_agent_did);
    assert_eq!(rows[0]["origin"]["source_doc_id"], doc_id);
    assert_eq!(rows[0]["input"]["work_unit_id"], "unit-scan");
    assert!(rows[0]["input"].get("_docID").is_none());
    engine.handle_created_doc("WorkUnit", &doc_id).await;
    let repeated = node.execute(&query).await;
    assert_eq!(
        crate::graphql::rows::<serde_json::Value>(&repeated, "CallbackInvocation")
            .unwrap()
            .len(),
        1
    );
}

fn wasm_bytes_for_id() -> Vec<u8> {
    b"\0asm\x01\x00\x00\x00hello-planner".to_vec()
}

fn module_doc(wasm: &[u8], args: &serde_json::Value, signer: &str) -> CallbackModuleDoc {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let module_id = compute_module_id(wasm, args, 1).unwrap();
    CallbackModuleDoc {
        module_id,
        agent_did: "did:key:zWriter".into(),
        tags: vec![],
        abi_version: Some(1),
        wasm_bytes: Some(STANDARD.encode(wasm)),
        canonical_args: Some(serde_json::to_string(args).unwrap()),
        signer_did: Some(signer.into()),
        provenance: Some("fixture_create_workspace".into()),
        enabled: true,
        fuel_limit: Some(50_000_000),
        memory_pages: Some(256),
        max_input_bytes: Some(1_000_000),
        max_output_bytes: Some(1_000_000),
    }
}

fn trusted(signer: &str) -> BTreeSet<String> {
    [signer.to_string()].into_iter().collect()
}

fn compile_wat(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap_or_else(|error| panic!("wat parse: {error}"))
}

fn wat_returns_json(json: &str) -> Vec<u8> {
    let escaped = json.replace('\\', "\\\\").replace('"', "\\\"");
    compile_wat(&format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}")
  (func (export "alloc") (param i32) (result i32) (i32.const 2048))
  (func (export "output_ptr") (result i32) (i32.const 0))
  (func (export "plan") (param i32 i32) (result i32)
    (i32.const {len}))
)"#,
        len = json.len()
    ))
}

fn tight_limits(fuel: u64, pages: u32, max_in: usize, max_out: usize) -> CallbackModuleLimits {
    CallbackModuleLimits {
        fuel_limit: fuel,
        memory_pages: pages,
        max_input_bytes: max_in,
        max_output_bytes: max_out,
    }
}

#[test]
fn module_id_is_stable_across_host_paths_and_arg_key_order() {
    let wasm = wasm_bytes_for_id();
    let root = TempDir::new().unwrap();
    let path_a = root.path().join("one").join("module.wasm");
    let path_b = root.path().join("other-host").join("copy.wasm");
    fs::create_dir_all(path_a.parent().unwrap()).unwrap();
    fs::create_dir_all(path_b.parent().unwrap()).unwrap();
    fs::write(&path_a, &wasm).unwrap();
    fs::write(&path_b, &wasm).unwrap();

    let id_a = compute_module_id(&fs::read(&path_a).unwrap(), &json!({"b": 1, "a": 2}), 1).unwrap();
    let id_b = compute_module_id(&fs::read(&path_b).unwrap(), &json!({"a": 2, "b": 1}), 1).unwrap();
    assert_eq!(id_a, id_b);
    assert!(id_a.starts_with("sha256:"));
    assert_ne!(
        compute_module_id(path_a.to_string_lossy().as_bytes(), &json!({}), 1).unwrap(),
        id_a,
        "module_id must hash decoded bytes, not the host path"
    );
}

#[test]
fn signer_policy_fail_closes_when_missing_or_untrusted() {
    let wasm = wasm_bytes_for_id();
    let mut module = module_doc(&wasm, &json!({}), "did:key:zTrusted");
    let empty = BTreeSet::new();
    let error = validate_callback_module(&module, &empty).unwrap_err();
    assert!(error.contains("no trusted principals"), "{error}");

    module.signer_did = None;
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("signer_did is missing"), "{error}");

    module.signer_did = Some("did:key:zTrusted".into());
    module.provenance = None;
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("provenance is missing"), "{error}");

    module.provenance = Some("fixture".into());
    module.signer_did = Some("did:key:zStranger".into());
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("not a trusted installer"), "{error}");

    module.signer_did = Some("did:key:zTrusted".into());
    validate_callback_module(&module, &trusted("did:key:zTrusted")).expect("trusted signer");
}

#[test]
fn fuel_exhaustion_denies_without_a_plan() {
    let wat = r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "output_ptr") (result i32) (i32.const 0))
  (func (export "plan") (param i32 i32) (result i32)
    (loop $spin (br $spin))
    unreachable)
)"#;
    let wasm = compile_wat(wat);
    // Empty input skips `alloc` so the first metered call is the spinning `plan`.
    let error = invoke_wasm_planner(&wasm, &tight_limits(1, 1, 64, 64), b"").unwrap_err();
    assert!(
        error.to_lowercase().contains("fuel"),
        "expected wasmtime fuel exhaustion, got {error}"
    );
}

#[test]
fn memory_limit_denies_at_instantiate() {
    let wat = r#"(module
  (memory (export "memory") 8)
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "output_ptr") (result i32) (i32.const 0))
  (func (export "plan") (param i32 i32) (result i32) (i32.const 0))
)"#;
    let wasm = compile_wat(wat);
    let error = invoke_wasm_planner(&wasm, &tight_limits(10_000, 1, 64, 64), b"{}").unwrap_err();
    assert!(error.to_lowercase().contains("denied"), "{error}");
}

#[test]
fn input_and_output_byte_limits_deny() {
    let wasm = wat_returns_json(&"x".repeat(32));
    let error = invoke_wasm_planner(&wasm, &tight_limits(10_000_000, 1, 4, 1024), b"0123456789")
        .unwrap_err();
    assert!(error.contains("max_input_bytes"), "{error}");

    let error =
        invoke_wasm_planner(&wasm, &tight_limits(10_000_000, 1, 1024, 8), b"{}").unwrap_err();
    assert!(error.contains("max_output_bytes"), "{error}");
}

#[test]
fn unknown_action_type_denies_the_entire_plan() {
    let wasm = wat_returns_json(r#"{"abi":1,"actions":[{"type":"not_a_real_action"}]}"#);
    let module = module_doc(&wasm, &json!({}), "did:key:zTrusted");
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let error = plan_from_wasm_module(&module, &json!({}), &caps).unwrap_err();
    assert!(error.contains("unknown ActionPlan action type"), "{error}");
}

#[test]
fn wasi_import_is_denied() {
    let wasm = compile_wat(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "output_ptr") (result i32) (i32.const 0))
  (func (export "plan") (param i32 i32) (result i32) (i32.const 0))
)"#,
    );
    let error = invoke_wasm_planner(&wasm, &tight_limits(10_000, 1, 64, 64), b"{}").unwrap_err();
    assert!(
        error.contains("may not import") && error.contains("wasi_snapshot_preview1"),
        "{error}"
    );
}

#[test]
fn host_path_in_plan_is_denied() {
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"/tmp/evil"}]}"#,
    )
    .unwrap_err();
    assert!(error.contains("host path"), "{error}");
    for branch in [
        r"C:\Users\evil",
        r"\\?\C:\foo",
        "file:///tmp/foo",
        "~/secret",
        "topic/../escape",
        r"topic\..\escape",
        "C:foo",
    ] {
        let raw = serde_json::to_string(&json!({
            "abi": 1,
            "actions": [{
                "type": "create_workspace",
                "path_capability": {"mode": "exactPaths", "paths": []},
                "workspace_id": "ws",
                "work_unit_id": "u",
                "repository_id": "r",
                "base_sha": "s",
                "branch": branch,
            }]
        }))
        .unwrap();
        let error = parse_action_plan_json(&raw).unwrap_err();
        assert!(error.contains("host path"), "{branch}: {error}");
    }
}

#[test]
fn extra_action_fields_deny_the_plan() {
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"topic","path":"relative"}]}"#,
    )
    .unwrap_err();
    assert!(
        error.contains("schema rejected")
            || error.contains("unknown field")
            || error.contains("host_path"),
        "{error}"
    );
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"topic","host_path":"/tmp/ws"}]}"#,
    )
    .unwrap_err();
    assert!(error.contains("host_path"), "{error}");
}

#[test]
fn wat_text_and_over_ceiling_limits_are_denied() {
    let error = invoke_wasm_planner(b"(module)", &tight_limits(1, 1, 64, 64), b"{}").unwrap_err();
    assert!(error.contains("binary module"), "{error}");

    let wasm = wasm_bytes_for_id();
    let mut module = module_doc(&wasm, &json!({}), "did:key:zTrusted");
    module.fuel_limit = Some(i64::MAX);
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("host maximum"), "{error}");

    let mut wat_module = module_doc(b"(module)", &json!({}), "did:key:zTrusted");
    let error = validate_callback_module(&wat_module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("binary module"), "{error}");

    let mut oversized = vec![0u8; MAX_WASM_BYTES + 1];
    oversized[..4].copy_from_slice(b"\0asm");
    wat_module = module_doc(&oversized, &json!({}), "did:key:zTrusted");
    let error = validate_callback_module(&wat_module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("max_wasm_bytes"), "{error}");
}

#[test]
fn capability_miss_denies_create_workspace_plan() {
    let wasm = fixture_create_workspace_wasm();
    if fixture_wasm_is_stub(wasm) {
        return;
    }
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1"
    });
    let module = module_doc(wasm, &json!({}), "did:key:zTrusted");
    validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap();
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let plan = plan_from_wasm_module(&module, &source, &caps).expect("plan");
    let missing = BTreeSet::from([CAP_OBSERVE_DIRTY_BASE.to_string()]);
    let error = plan.validate_against(&missing).unwrap_err().to_string();
    assert!(
        error.contains("missing capability create_workspace"),
        "{error}"
    );
}

#[test]
fn fixture_wasm_emits_valid_create_workspace_plan() {
    let wasm = fixture_create_workspace_wasm();
    if fixture_wasm_is_stub(wasm) {
        return;
    }
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1",
        "api_token": "should-be-stripped"
    });
    let module = module_doc(
        wasm,
        &json!({"adapter": "git_worktree"}),
        "did:key:zTrusted",
    );
    validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap();
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let stripped = strip_secret_fields(source);
    let plan = plan_from_wasm_module(&module, &stripped, &caps).expect("fixture plan");
    let canonical = action_plan_canonical_json(&plan).expect("canonical");
    assert!(!canonical.contains("host_path"), "{canonical}");
    assert!(!canonical.contains("/tmp"), "{canonical}");
    assert!(!canonical.contains("api_token"), "{canonical}");
    assert_eq!(canonical, action_plan_canonical_json(&plan).unwrap());
    assert!(canonical.contains("\"abi\":1"));
    assert!(canonical.contains("\"adapter\":\"git_worktree\""));
    match &plan.actions[0] {
        HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-1");
            assert_eq!(action.work_unit_id, "unit-1");
            assert_eq!(action.adapter.as_str(), "git_worktree");
        }
        HostAction::FreezeWorkspaceBase(_)
        | HostAction::SealWorkspace(_)
        | HostAction::IntegrateWorkspace(_)
        | HostAction::CleanupWorkspace(_) => panic!("expected create_workspace"),
    }
    plan.validate_against(&caps).expect("capabilities");

    let mut wasm_binding = callback();
    wasm_binding.handler = crate::document_config::CallbackHandler::Module {
        module_id: module.module_id.clone(),
    };
    wasm_binding.capabilities = vec![CAP_CREATE_WORKSPACE.into(), CAP_OBSERVE_DIRTY_BASE.into()];
    let via_binding = plan_from_callback(&wasm_binding, &stripped, Some(&module)).expect("wired");
    assert_eq!(via_binding, plan);

    let clone_caps: BTreeSet<String> = [
        CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
        CAP_CLONE_ARTIFACTS,
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let builtin_binding = callback();
    let builtin = emit_plan_from_source(&builtin_binding, &stripped).expect("builtin");
    builtin.validate_against(&clone_caps).expect("builtin caps");
}

#[test]
fn canonical_action_plan_sorts_object_keys() {
    let plan = emit_plan_from_source(
        &callback(),
        &json!({
            "owned_files": [],
            "work_unit_id": "unit-1",
            "repository_id": "repo-1",
            "base_sha": "abc",
            "branch": "topic",
            "workspace_id": "ws-1"
        }),
    )
    .unwrap();
    let canonical = action_plan_canonical_json(&plan).unwrap();
    let abi = canonical.find("\"abi\"").unwrap();
    let actions = canonical.find("\"actions\"").unwrap();
    assert!(abi < actions, "{canonical}");
    let parsed = parse_action_plan_json(&canonical).unwrap();
    assert_eq!(parsed, plan);
}
