use std::path::Path;
use std::process::Command;
use std::time::Duration;

use gents_documents::FileToolMode;
use gents_documents::{FileTools, HostTools, Tools};
use gents_session::workspace::{provision_read_only_workspace, WorkspaceProvisioningSelection};

use crate::support::agent_requests::{canonical_now, RequestSeed};
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_ceiling, live_target, wait_for_request_terminal,
};
use crate::support::test_db;

const PROMPT: &str = "Acknowledge this sealed workspace request in one short sentence. Do not inspect files or call tools.";

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
    let target = live_target().await;
    let actors = bind_target(&db, &target).await;
    let did = actors.node_did().to_owned();
    let agent_did = actors.agent_did().to_owned();

    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().to_owned();
    let (repo, revision) = tokio::task::spawn_blocking(move || {
        let repo = std::fs::canonicalize(path).unwrap().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "recovery@example.com"]);
        git(&repo, &["config", "user.name", "Workspace Recovery"]);
        std::fs::write(repo.join("README.md"), "sealed\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "sealed recovery fixture"]);
        let revision = git(&repo, &["rev-parse", "HEAD"]);
        (repo, revision)
    })
    .await
    .unwrap();

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
        &revision,
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

    let runtime = boot_live_agent_with_ceiling(
        &db,
        db.node_identity.clone(),
        gents_tools::surface::ToolCeiling::readonly_at(&repo),
    )
    .await
    .unwrap();
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
