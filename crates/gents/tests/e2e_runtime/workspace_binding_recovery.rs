//! A request its Agent authored against a sealed read-only workspace before
//! the runtime started waits in `workspaceBindingPending`; the runtime's
//! startup recovery binds it as that held Agent and runs it.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use gents::documents::{FileTools, HostTools, Tools};
use gents::tools::workspace::{provision_read_only_workspace, WorkspaceProvisioningSelection};
use gents::FileToolMode;

use crate::support::agent_requests::{canonical_now, RequestSeed};
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime,
};
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
use crate::support::test_db;

const BACKEND: &str = "workspace-recovery-backend";
const MODEL: &str = "workspace-recovery-model";
const PROMPT: &str = "Summarize the sealed workspace.";

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_binds_an_agent_authored_workspace_request() {
    let db = test_db("workspace-binding-recovery").await;
    let backend = MockStreamingBackend::start(
        MODEL,
        vec![StreamScript::completes(PROMPT, ["the workspace is sealed"])],
    )
    .unwrap();
    let actors = builder_actors(&db, BACKEND, backend.endpoint(), MODEL, 1).await;
    let did = actors.node_did().to_owned();
    let agent_did = actors.agent_did().to_owned();

    let fixture = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(fixture.path()).unwrap().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "recovery@example.com"]);
    git(&repo, &["config", "user.name", "Workspace Recovery"]);
    std::fs::write(repo.join("README.md"), "sealed\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "sealed recovery fixture"]);

    crate::support::fixtures::configure_agent_tools_as(
        &actors.fixture.access,
        &did,
        &agent_did,
        None,
        Tools {
            tools_id: "workspace-recovery-tools".into(),
            node_did: did.clone(),
            host: Some(HostTools {
                root: Some(repo.display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let sealed = provision_read_only_workspace(
        &actors.fixture.access,
        &actors.agent_access,
        &repo,
        &git(&repo, &["rev-parse", "HEAD"]),
        &WorkspaceProvisioningSelection {
            workspace_id: uuid::Uuid::new_v4().to_string(),
            node_did: did.clone(),
            creator_agent_did: agent_did.clone(),
            writer_agent_did: agent_did.clone(),
            integrator_agent_did: agent_did.clone(),
            reader_agent_dids: Vec::new(),
        },
    )
    .await
    .expect("provision the sealed read-only workspace");

    let now = canonical_now();
    let mut seed = RequestSeed::new("workspace-recovery-request", "workspace-recovery", &now);
    seed.content = Some(PROMPT);
    let mut request = actors.request_create(&seed);
    request.initial_lifecycle_state =
        gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending;
    request.workspace_id = Some(sealed.workspace.workspace_id.clone());
    request.workspace_authority = Some("readOnly".into());
    request.workspace_owner_node_did = Some(did.clone());
    request.workspace_seal_hash = sealed.workspace.seal_hash.clone();
    let doc = actors.create_signed(request).await;

    let runtime = spawn_runtime(
        &db,
        fixture_agent(runtime_builder(&db, &actors), &actors, &agent_did)
            .backend_id(BACKEND)
            .model_name(MODEL)
            .stream_batch_ms(0)
            .deadline_duration_secs(60)
            .done()
            .tool_ceiling(gents::ToolCeiling::readonly_at(&repo))
            .build()
            .await
            .unwrap(),
    )
    .await;
    let terminal = wait_for_request_terminal(
        &actors,
        "workspace-recovery-request",
        Duration::from_secs(60),
    )
    .await;
    let row = actors
        .request_row(
            &doc,
            "lifecycle_state failure_reason workspace_id workspace_seal_hash",
        )
        .await;
    runtime.shutdown().await;
    assert_eq!(terminal, "completed", "{row}");
    assert_eq!(row["workspace_id"], sealed.workspace.workspace_id.as_str());
    assert_eq!(
        row["workspace_seal_hash"],
        sealed.workspace.seal_hash.as_deref().unwrap()
    );
}
