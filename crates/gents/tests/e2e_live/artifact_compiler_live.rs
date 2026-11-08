use crate::support::live_inference::wait_for_request_terminal;
use crate::support::snapshots::fetch_tool_call_payloads_for_request_as;
use crate::support::{
    interrupt::{wait_for_runtime_ready, BootedAgent},
    test_db,
};
use gents::documents::{BashTools, FileTools, HostTools, Tools};
use gents::{
    tools::workspace::*, BashMode, CommandExecutionMode, CommandNetworkMode,
    DocumentRuntimeOptions, FileToolMode, Gents, NodeIdentity, ToolCeiling,
};
use serde_json::{json, Value};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

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

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live compiler QA; run built test executable directly without parent Cargo; GENTS_ARTIFACT_LIVE=1"]
async fn real_glm_daemon_compiler_uses_sealed_artifact_authority() {
    assert_eq!(std::env::var("GENTS_ARTIFACT_LIVE").as_deref(), Ok("1"));
    let db = test_db("artifact-compiler-live").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let did = identity.did().to_owned();
    let target = crate::support::live_inference::live_target();
    let actors = crate::support::live_inference::bind_target(&db, &target).await;
    let agent_did = actors.agent_did().to_owned();
    let backend_id = target.backend_id().to_owned();
    let access = actors.fixture.access.clone();
    // The node's client key bounds the live profile with an execution.
    access
        .transact_write("test.artifact_live.profile", |txn| {
            use gents::documents::apply_desired_state_plan;
            use gents::store::config_client::{
                read_desired_state_record_in_txn as read, DesiredStateApplyDocument,
                DesiredStateApplyPlan,
            };
            let did = &did;
            let agent_did = &agent_did;
            let backend_id = &backend_id;
            let model = target.model().to_owned();
            Box::pin(async move {
                let (_, agent) = read(txn, gents::Collection::Agent, did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the live Agent is missing"))?;
                let mut agent: gents::documents::Agent = serde_json::from_value(agent)?;
                agent.inference_profile_id = "artifact-live-profile".into();
                agent.enabled = true;
                let documents = [
                    (
                        gents::Collection::InferenceExecution,
                        json!({
                            "execution_id": "artifact-live-execution", "node_did": did,
                            "max_turns": 8, "deadline_duration_secs": 600,
                            "stream_liveness_timeout_secs": 180
                        }),
                    ),
                    (
                        gents::Collection::InferenceProfile,
                        json!({
                            "profile_id": "artifact-live-profile", "node_did": did,
                            "display_name": "Bounded artifact live QA", "backend_id": backend_id,
                            "model_name": model, "max_output_tokens": 4096,
                            "execution_id": "artifact-live-execution"
                        }),
                    ),
                    (gents::Collection::Agent, serde_json::to_value(agent)?),
                ];
                let plan = DesiredStateApplyPlan::new(
                    documents
                        .into_iter()
                        .map(|(collection, value)| DesiredStateApplyDocument {
                            collection,
                            add: value.clone(),
                            update: value,
                        })
                        .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("bound the artifact live profile as the node's client key");

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

    crate::support::fixtures::configure_agent_tools_as(
        &access,
        &did,
        &agent_did,
        None,
        Tools {
            tools_id: "artifact-live-tools".into(),
            node_did: did.clone(),
            host: Some(HostTools {
                root: Some(repo.display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                bash: Some(BashTools {
                    mode: BashMode::Unrestricted,
                    execution_mode: Some(CommandExecutionMode::ArtifactWrite),
                    network_mode: Some(CommandNetworkMode::Disabled),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    // The User publishes the repository and the managed Agent creates and
    // seals the read-only workspace through the production provisioning owner,
    // which publishes the workspace documents as that Agent.
    let workspace_id = uuid::Uuid::new_v4().to_string();
    let sealed = gents::tools::workspace::provision_read_only_workspace(
        &access,
        &actors.agent_access,
        &repo,
        &git(&repo, &["rev-parse", "HEAD"]),
        &WorkspaceProvisioningSelection {
            workspace_id: workspace_id.clone(),
            node_did: did.clone(),
            creator_agent_did: agent_did.clone(),
            writer_agent_did: agent_did.clone(),
            integrator_agent_did: agent_did.clone(),
            reader_agent_dids: Vec::new(),
        },
    )
    .await
    .expect("provision the sealed read-only workspace");
    let placement = sealed.placement.clone();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let source = std::path::PathBuf::from(&placement.host_path);
    let before_tree = git(&source, &["rev-parse", "HEAD^{tree}"]);
    let gitdir = std::path::PathBuf::from(git(&source, &["rev-parse", "--absolute-git-dir"]));
    let before_metadata: Vec<_> = ["HEAD", "index", "commondir", "gitdir"]
        .into_iter()
        .map(|file| (file, std::fs::read(gitdir.join(file)).unwrap()))
        .collect();

    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readwrite(&repo),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // The Agent authors its own request against the sealed workspace.
    let mut seed = crate::support::agent_requests::RequestSeed::new(
        "artifact-live-request",
        "artifact-live-session",
        &now,
    );
    seed.content = Some(
        "Inspect Cargo.toml and src/lib.rs, then call bash exactly once with command cargo, args [\"test\",\"--locked\",\"--offline\"], raw_json true. This is a sealed read-only source with runtime-managed compiler artifacts. Do not edit source or set output directories. You must execute the test, not merely suggest it. After observing its actual result, report the test count and answer value. If the compiler fails, report the exact failure without claiming success.",
    );
    let mut request = actors.request_create(&seed);
    request.initial_lifecycle_state =
        gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending;
    request.workspace_id = Some(sealed.workspace.workspace_id.clone());
    request.workspace_authority = Some("readOnly".into());
    request.workspace_owner_node_did = Some(did.clone());
    request.workspace_seal_hash = sealed.workspace.seal_hash.clone();
    actors.create_signed(request).await;
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(rx));
    let booted = BootedAgent::new(tx, handle, did.clone());
    wait_for_runtime_ready(&db.node, &did).await;
    let terminal =
        wait_for_request_terminal(&access, "artifact-live-request", Duration::from_secs(600)).await;
    let evidence = access.execute(r#"{
        AgentRequest(filter: { request_id: { _eq: "artifact-live-request" } }) { _docID request_id lifecycle_state failure_reason terminal_output execution_generation workspace_id workspace_authority workspace_owner_node_did workspace_seal_hash }
        AgentToolCall(filter: { request_id: { _eq: "artifact-live-request" } }) { tool_call_id tool_name lifecycle_state request_doc_id }
        InferenceCall(filter: { request_id: { _eq: "artifact-live-request" } }) { call_id backend_id request_doc_id call_state prompt_tokens completion_tokens }
        WorkspaceBinding(filter: { request_id: { _eq: "artifact-live-request" } }) { request_doc_id workspace_id authority owner_node_did seal_hash lifecycle_state }
    }"#).await.expect("read artifact live evidence as the node's client key")["data"].take();
    let payloads = fetch_tool_call_payloads_for_request_as(&access, "artifact-live-request").await;
    booted.shutdown().await;
    if let Some(path) = std::env::var_os("GENTS_ARTIFACT_LIVE_EVIDENCE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"endpoint":target.endpoint(),"model":target.model(),"terminal":terminal,"evidence":evidence}),
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
            let payload = payloads.iter().find(|payload| {
                call["tool_call_id"].as_str() == Some(payload.tool_call_id.as_str())
            });
            let args = payload
                .and_then(|payload| payload.arguments.as_deref())
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or(Value::Null);
            let result = payload
                .and_then(|payload| payload.result.as_deref())
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or(Value::Null);
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
                && call["backend_id"] == target.backend_id()
                && call["call_state"] == "completed"
                && call["call_id"].as_str().is_some_and(|id| !id.is_empty())),
        "every inference must belong to this physical request and configured backend: {evidence}"
    );
    assert!(inference.iter().any(|call| call["completion_tokens"]
        .as_u64()
        .is_some_and(|tokens| tokens > 0)));
    assert_eq!(row["workspace_id"], workspace_id.as_str());
    assert_eq!(row["workspace_owner_node_did"], did);
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
