//! Run the built e2e_live executable directly, with no parent Cargo process.
use crate::steward_loop_live::wait_for_request_terminal;
use crate::support::{
    interrupt::{wait_for_runtime_ready, BootedAgent},
    test_db,
};
use gents::{
    graphql::escape_graphql_string, workspace::*, AgentIdentity, DocumentRuntimeOptions, Gents,
    ToolCeiling,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path, process::Command, sync::Arc, time::Duration};

const ENDPOINT: &str = "http://workstation-2:8000/v1";
const MODEL: &str = "GLM-5.3-Flash-NVFP4";

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
async fn execute(node: &gents::defra_node::EmbeddedNode, query: &str) -> Value {
    let out = node.execute(query).await;
    assert!(!out.has_errors(), "{query}: {:?}", out.errors);
    out.data.unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live compiler QA; run built test executable directly without parent Cargo; GENTS_ARTIFACT_LIVE=1"]
async fn real_glm_daemon_compiler_uses_sealed_artifact_authority() {
    assert_eq!(std::env::var("GENTS_ARTIFACT_LIVE").as_deref(), Ok("1"));
    let db = test_db("artifact-compiler-live").await;
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let did = identity.did().to_owned();
    let bootstrap = gents::ensure_agent_principal(&db.node, &did).await.unwrap();
    let mut behavior = bootstrap.default_behavior;
    let behavior_id = behavior.behavior_id.clone();
    execute(&db.node, &format!(r#"mutation {{ create_InferenceBackend(input: {{
        backend_id: "artifact-live-backend", name: "Artifact live GLM", provider_kind: "OpenAiCompatible",
        endpoint: "{ENDPOINT}", api_key: "", api_key_env_var: "", enabled: true,
        max_concurrent: 1, max_queue_depth: 4, models: ["{MODEL}"], probe_status: "healthy"
    }}) {{ _docID }} }}"#)).await;
    execute(
        &db.node,
        r#"mutation { create_InferenceProfile(input: {
        profile_id: "artifact-live-profile", display_name: "Bounded artifact live QA",
        max_turns: 8, max_output_tokens: 4096, temperature: 0.7,
        deadline_duration_secs: 600, stream_liveness_timeout_secs: 180
    }) { _docID } }"#,
    )
    .await;
    gents::upsert_tool_selection(
        &db.node,
        &gents::ToolSelectionDocument {
            selection_id: "artifact-live-tools".into(),
            agent_did: did.clone(),
            tool_policy_version: Some("tool-policy/v1".into()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".into()),
            enable_bash: Some(true),
            bash_mode: Some("Unrestricted".into()),
            command_execution_policy: Some("artifact_write".into()),
            command_network_mode: Some("disabled".into()),
            enable_lsp: Some(false),
            enable_meta_tools: Some(false),
            enable_goal_tools: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    behavior.backend_id = Some("artifact-live-backend".into());
    behavior.model_name = Some(MODEL.into());
    behavior.inference_profile_id = Some("artifact-live-profile".into());
    behavior.tool_selection_id = Some("artifact-live-tools".into());
    behavior.enabled = true;
    gents::upsert_agent_behavior(&db.node, &behavior)
        .await
        .unwrap();

    let fixture = tempfile::tempdir().unwrap();
    let parent = std::fs::canonicalize(fixture.path()).unwrap();
    let repo = parent.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &["config", "user.email", "artifact-live@example.com"],
    );
    git(&repo, &["config", "user.name", "Artifact Live QA"]);
    let files = [("Cargo.toml", "[package]\nname = \"artifact-live-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("Cargo.lock", "version = 3\n\n[[package]]\nname = \"artifact-live-probe\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub fn answer() -> u32 { 42 }\n#[test] fn live_artifact_answer() { assert_eq!(answer(), 42); }\n")];
    for (path, bytes) in files {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "sealed compiler fixture"]);
    let deployment = "artifact-live-deployment";
    execute(&db.node, &format!(r#"mutation {{ create_HostDeployment(input: {{ deployment_id: "{deployment}", display_name: "Artifact QA" }}) {{ _docID }} }}"#)).await;
    let mut documents = MemoryWorkspaceDocuments::default();
    let mut host = HostExecutorContext {
        deployment_id: deployment.into(),
        repository: RepositoryPlacementRef {
            repository_id: "artifact-live-repo".into(),
            deployment_id: deployment.into(),
            host_path: repo.clone(),
            enabled: true,
        },
        ceiling: Some(&repo),
        capabilities: BTreeSet::from([
            CAP_CREATE_WORKSPACE.into(),
            CAP_OBSERVE_DIRTY_BASE.into(),
            CAP_SEAL_WORKSPACE.into(),
        ]),
        writer_principal: did.clone(),
        integrator_principal: did.clone(),
        caused_by_invocation_id: "artifact-live-setup".into(),
        caused_by_correlation: "artifact-live-setup".into(),
        documents: &mut documents,
    };
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(CreateWorkspaceAction {
            workspace_id: "artifact-live-workspace".into(),
            work_unit_id: "artifact-live-unit".into(),
            repository_id: "artifact-live-repo".into(),
            base_sha: git(&repo, &["rev-parse", "HEAD"]),
            branch: "artifact-live-review".into(),
            creation_policy: CreationPolicy::GitWorktreeDiff,
            adapter: WorkspaceAdapterKind::GitWorktree,
            clone_artifacts: None,
            path_capability: WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
        }),
        &mut Vec::new(),
        &mut host,
    )
    .unwrap();
    let sealed = execute_seal_workspace_plan(
        &emit_seal_workspace_plan(SealWorkspaceAction {
            workspace_id: "artifact-live-workspace".into(),
            produced_by_request_id: "artifact-live-writer".into(),
            produced_by_request_doc_id: "artifact-live-writer-doc".into(),
        }),
        &mut Vec::new(),
        &mut host,
    )
    .unwrap();
    let mut placement = created.placement;
    placement.observed_tree_hash = sealed.workspace.seal_hash.clone().unwrap();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    execute(
        &db.node,
        &isolated_workspace_upsert_mutation(&sealed.workspace),
    )
    .await;
    execute(
        &db.node,
        &workspace_placement_upsert_mutation(&placement, &now),
    )
    .await;
    let source = std::path::PathBuf::from(&placement.host_path);
    let before_tree = git(&source, &["rev-parse", "HEAD^{tree}"]);
    let gitdir = std::path::PathBuf::from(git(&source, &["rev-parse", "--absolute-git-dir"]));
    let before_metadata: Vec<_> = ["HEAD", "index", "commondir", "gitdir"]
        .into_iter()
        .map(|file| (file, std::fs::read(gitdir.join(file)).unwrap()))
        .collect();

    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(&repo),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(rx));
    let booted = BootedAgent::new(tx, handle, did.clone());
    wait_for_runtime_ready(&db.node, &did).await;
    let did_q = escape_graphql_string(&did);
    let behavior_q = escape_graphql_string(&behavior_id);
    execute(&db.node, &format!(r#"mutation {{ create_AgentConversation(input: {{ session_id: "artifact-live-session",
        agent_did: "{did_q}", agent_name: "{behavior_q}", behavior_id: "{behavior_q}", title: "Artifact live QA", title_source: "generated",
        status: "active", created_at: "{now}", updated_at: "{now}" }}) {{ _docID }} }}"#)).await;
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        "artifact-live-request", &did, &did, &behavior_id, "artifact-live-session",
        "Inspect Cargo.toml and src/lib.rs, then call bash exactly once with command cargo, args [\"test\",\"--locked\",\"--offline\"], raw_json true. This is a sealed read-only source with runtime-managed compiler artifacts. Do not edit source or set output directories. You must execute the test, not merely suggest it. After observing its actual result, report the test count and answer value. If the compiler fails, report the exact failure without claiming success.",
        "interactive", &now, gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did));
    request.workspace_id = Some(sealed.workspace.workspace_id.clone());
    request.workspace_authority = Some("readOnly".into());
    request.workspace_owner_deployment_id = Some(deployment.into());
    request.workspace_seal_hash = sealed.workspace.seal_hash.clone();
    gents::sign_agent_request_create_as_registered_target(&mut request)
        .await
        .unwrap();
    execute(&db.node, &request.graphql_mutation().unwrap()).await;
    let terminal =
        wait_for_request_terminal(&db.node, "artifact-live-request", Duration::from_secs(600))
            .await;
    let evidence = execute(&db.node, r#"{
        AgentRequest(filter: { request_id: { _eq: "artifact-live-request" } }) { _docID request_id lifecycle_state execution_generation workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash }
        AgentToolCall(filter: { request_id: { _eq: "artifact-live-request" } }) { tool_call_id tool_name args result lifecycle_state request_doc_id }
        InferenceCall(filter: { request_id: { _eq: "artifact-live-request" } }) { call_id backend_id request_doc_id call_state prompt_tokens completion_tokens }
        WorkspaceBinding(filter: { request_id: { _eq: "artifact-live-request" } }) { request_doc_id workspace_id authority deployment_id seal_hash lifecycle_state }
    }"#).await;
    booted.shutdown().await;
    if let Some(path) = std::env::var_os("GENTS_ARTIFACT_LIVE_EVIDENCE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"endpoint":ENDPOINT,"model":MODEL,"terminal":terminal,"evidence":evidence}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(terminal, "completed", "{evidence}");
    let row = &evidence["AgentRequest"][0];
    assert_eq!(evidence["AgentRequest"].as_array().unwrap().len(), 1);
    let calls = evidence["AgentToolCall"].as_array().unwrap();
    assert!(
        calls.iter().any(|call| {
            let args = call["args"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or(Value::Null);
            let result = call["result"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or(Value::Null);
            // UnrestrictedBashTool::NAME identifies shell-tool access; the
            // separate execution_mode below must still be ArtifactWrite.
            call["tool_name"] == "bash_unrestricted"
                && call["request_doc_id"] == row["_docID"]
                && call["lifecycle_state"] == "completed"
                && args["command"] == "cargo"
                && args["args"] == json!(["test", "--locked", "--offline"])
                && result["execution_mode"] == "artifact_write"
                && result["sandbox"] == "macos_seatbelt"
                && result["network_mode"] == "disabled"
                && result["exit_code"] == 0
                && result["stdout"]
                    .as_str()
                    .is_some_and(|text| text.contains("1 passed"))
        }),
        "actual successful artifact compiler ToolCall required: {evidence}"
    );
    let inference = evidence["InferenceCall"].as_array().unwrap();
    assert!(!inference.is_empty());
    assert!(
        inference
            .iter()
            .all(|call| call["request_doc_id"] == row["_docID"]
                && call["backend_id"] == "artifact-live-backend"
                && call["call_state"] == "completed"
                && call["call_id"].as_str().is_some_and(|id| !id.is_empty())),
        "every inference must belong to this physical request and configured backend: {evidence}"
    );
    assert!(inference.iter().any(|call| call["completion_tokens"]
        .as_u64()
        .is_some_and(|tokens| tokens > 0)));
    assert_eq!(row["workspace_id"], "artifact-live-workspace");
    assert_eq!(row["workspace_owner_deployment_id"], deployment);
    assert_eq!(row["workspace_authority"], "readOnly");
    assert_eq!(
        row["workspace_seal_hash"],
        sealed.workspace.seal_hash.as_deref().unwrap()
    );
    assert!(row["execution_generation"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(evidence["WorkspaceBinding"]
        .as_array()
        .unwrap()
        .iter()
        .any(|binding| binding["request_doc_id"] == row["_docID"]
            && binding["authority"] == "readOnly"
            && binding["seal_hash"] == row["workspace_seal_hash"]));
    for (path, bytes) in files {
        assert_eq!(std::fs::read(source.join(path)).unwrap(), bytes.as_bytes());
    }
    for (name, bytes) in before_metadata {
        assert_eq!(std::fs::read(gitdir.join(name)).unwrap(), bytes);
    }
    assert_eq!(git(&source, &["rev-parse", "HEAD^{tree}"]), before_tree);
    assert!(git(&source, &["status", "--porcelain=v1"]).is_empty());
    assert!(!source.join("target").exists());
}
