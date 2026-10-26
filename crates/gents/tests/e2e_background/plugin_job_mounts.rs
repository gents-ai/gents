//! Read-only pack and project mounts reach a plugin job's steps end to end.

use gents_protocol::error_chain::ErrorChain;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gents::pack::{
    install_prepared_document_pack, list_home_installs, load_pack_config,
    prepare_document_pack_install, BindAccess, DriftPolicy, InstalledVia, PackInferenceBindings,
    PackInstallOptions, PackKind, PluginDirBinding,
};
use gents::pack_archive::{pack_dir, PackArchive};
use gents::plugin::executor::PluginExecutor;
use gents::plugin::store;
use gents::plugin::BoundDir;
use gents::plugin_job::driver::JobRuntime;
use gents::plugin_job::input::{build_step_input, StepKind};
use gents::plugin_job::job::{AssetMount, JobPolicy, JobState, PluginJobSettings};
use gents::{ConfigAccess, DocumentRuntimeOptions, Gents, NodeIdentity};
use serde_json::{json, Value};

use crate::plugin_job_support::{
    effects, guest_pin, install_job_fixture, job_call, job_settings, result, spawn, wait_job,
    JobHarness, PlanEntry, CHILD, MODEL, PACK, PARENT,
};
use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::streaming_backend::{StreamPlan, StreamResponse};
use crate::support::{test_db, TestDb};

const CHILD_PROMPT: &str = "mount child a";
const TOOL_CALL: &str = "job-m";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/job_plugin_fixture")
}

fn fixture_asset() -> String {
    std::fs::read_to_string(fixture_dir().join("fixture_assets/a.txt"))
        .expect("read the fixture asset")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("list the source") {
        let entry = entry.expect("read the source entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy the file");
        }
    }
}

fn entry(when: Value, output: gents::plugin_job::job::JobStepOutput, note: bool) -> PlanEntry {
    PlanEntry {
        when,
        output,
        note,
        scratch: false,
        control: None,
    }
}

/// Step 0 pins the job and spawns slot `a`; the job completes when it does.
/// With `note`, the completing step also writes a `FixtureNote` holding the
/// mounted asset's text.
fn plan(note: bool) -> Vec<PlanEntry> {
    vec![
        entry(
            json!({"step": 0}),
            effects(
                Some(guest_pin("job-m-handle")),
                vec![spawn("a", CHILD_PROMPT)],
            ),
            false,
        ),
        entry(
            json!({"children": {"a": "completed"}}),
            result(json!("done")),
            note,
        ),
        entry(
            json!({"children": {"a": "live"}}),
            effects(None, Vec::new()),
            false,
        ),
    ]
}

/// Boots the fixture job with `settings` and a request working folder of `cwd`.
async fn boot(
    name: &str,
    settings: PluginJobSettings,
    cwd: &Path,
    note: bool,
) -> (JobHarness, tempfile::TempDir) {
    let db = test_db(name).await;
    let plugin_home = tempfile::tempdir().expect("plugin home");
    install_job_fixture(&db, plugin_home.path(), settings).await;
    let session_id = format!("{name}-session");
    let cwd = cwd.to_string_lossy().into_owned();
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "plugin-job-backend",
            model: MODEL,
            parent_agent_id: PARENT,
            configured_agent_ids: &[PARENT, CHILD],
            request_id: &format!("{session_id}-parent"),
            session_id: &session_id,
            prompt: &format!("{session_id} start"),
            accepted_chunks: vec![job_call(TOOL_CALL, &plan(note))],
            child_plans: vec![StreamPlan::current_authored_user(
                CHILD_PROMPT,
                vec![StreamResponse::completes(CHILD_PROMPT, ["child a done"])],
            )],
            valid_until: None,
            request_hop: None,
            request_setup: Some(Box::new(move |request| request.input.cwd = Some(cwd))),
        },
    )
    .await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .expect("build the plugin job runtime");
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let harness = JobHarness {
        db,
        plugin_home: plugin_home.path().to_path_buf(),
        runtime,
        session_id,
    };
    (harness, plugin_home)
}

/// Runs the fixture job to completion and returns the job row.
async fn completed_job(h: &JobHarness) -> String {
    let view = wait_job(
        h,
        TOOL_CALL,
        |view| view.state == JobState::Completed,
        Duration::from_secs(30),
    )
    .await;
    view.row_doc_id
}

/// The mounts the host builds into a later step of the pinned job row.
async fn step_mounts(h: &JobHarness, row_doc_id: &str) -> Vec<AssetMount> {
    let runtime = JobRuntime {
        node: h.db.node.clone(),
        plugins: Arc::new(PluginExecutor::new(Some(h.plugin_home.clone()))),
        home: h.plugin_home.clone(),
        tool_root: None,
    };
    build_step_input(
        &ConfigAccess::local(h.db.node.clone()),
        &runtime,
        row_doc_id,
        StepKind::Later,
    )
    .await
    .expect("build the step input")
    .0
    .mounts
}

/// Packs and installs the pack in `dir` through the document-pack owners.
async fn install_pack_dir(db: &TestDb, plugin_home: &Path, dir: &Path) {
    let did = db.node_identity.did().to_string();
    gents::ensure_node(&db.node, &did)
        .await
        .expect("create the node document the pack install requires");
    let (bytes, _) = pack_dir(dir).expect("pack the directory");
    let archive = PackArchive::from_bytes(&bytes).expect("read the archive");
    let config = load_pack_config(
        archive.manifest(),
        &PackInstallOptions {
            node_did: did.clone(),
        },
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        &|_| None,
    )
    .expect("load the pack config");
    let access = ConfigAccess::local(db.node.clone());
    let prepared = prepare_document_pack_install(
        &access,
        &did,
        archive.manifest(),
        &config,
        &PackInferenceBindings::new(),
        &[],
        InstalledVia::Operator,
        &|_| None,
    )
    .await
    .expect("prepare the install");
    install_prepared_document_pack(
        &access,
        &did,
        &archive,
        &[],
        &prepared,
        Some(plugin_home),
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
    .expect("install the pack");
}

fn pack_mount_dir(mount: &AssetMount) -> PathBuf {
    PathBuf::from(&mount.path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_asset_is_readable_through_the_job_guest() {
    let cwd = tempfile::tempdir().expect("job working folder");
    let (h, _home) = boot(
        "plugin-job-mount-read",
        job_settings(),
        &cwd.path().canonicalize().unwrap(),
        true,
    )
    .await;
    completed_job(&h).await;

    let response = ConfigAccess::local(h.db.node.clone())
        .execute("{ FixtureNote { text } }")
        .await
        .expect("read the fixture notes");
    let notes = response["data"]["FixtureNote"]
        .as_array()
        .expect("a note list");
    assert_eq!(notes.len(), 1, "{response}");
    assert_eq!(notes[0]["text"], fixture_asset());
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_path_is_digest_pinned_and_a_reinstall_gives_a_new_path() {
    let cwd = tempfile::tempdir().expect("job working folder");
    let (h, home) = boot(
        "plugin-job-mount-digest",
        job_settings(),
        &cwd.path().canonicalize().unwrap(),
        false,
    )
    .await;
    let row = completed_job(&h).await;

    let first = step_mounts(&h, &row).await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].pack, PACK);
    assert_eq!(first[0].class, "fixture_assets");
    let hex = first[0]
        .digest
        .strip_prefix("sha256:")
        .expect("a sha256 digest")
        .to_owned();
    assert_eq!(
        pack_mount_dir(&first[0]),
        home.path()
            .join("packs/.materialized/fixture/job_plugin_fixture")
            .join(&hex)
            .join("fixture_assets")
    );

    let changed = tempfile::tempdir().expect("changed pack copy");
    copy_dir(&fixture_dir(), changed.path());
    std::fs::write(
        changed.path().join("fixture_assets/a.txt"),
        "fixture asset b\n",
    )
    .expect("change the asset");
    install_pack_dir(&h.db, home.path(), changed.path()).await;

    let second = step_mounts(&h, &row).await;
    assert_eq!(second.len(), 1, "{second:?}");
    assert_ne!(second[0].digest, first[0].digest);
    assert_ne!(second[0].path, first[0].path);
    let hex = second[0].digest.strip_prefix("sha256:").unwrap();
    assert!(second[0].path.contains(hex), "{second:?}");
    assert_eq!(
        std::fs::read_to_string(pack_mount_dir(&second[0]).join("a.txt")).unwrap(),
        "fixture asset b\n"
    );
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documents_pack_shipping_assets_is_materialized_with_no_plugin_installed() {
    let db = test_db("plugin-job-mount-assets-only").await;
    let home = tempfile::tempdir().expect("plugin home");
    let pack = tempfile::tempdir().expect("pack copy");
    copy_dir(&fixture_dir(), pack.path());
    std::fs::remove_dir_all(pack.path().join("plugins")).expect("drop the plugin");
    let manifest_path = pack.path().join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["name"] = json!("assets_only");
    manifest["plugins"] = json!([]);
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .retain(|asset| asset != "plugins/job_fixture.afb");
    std::fs::write(&manifest_path, manifest.to_string()).unwrap();

    install_pack_dir(&db, home.path(), pack.path()).await;

    let records = list_home_installs(home.path()).expect("list the home installs");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].coordinate, "fixture/assets_only");
    assert_eq!(records[0].kind, PackKind::Documents);
    assert!(records[0].plugins.is_empty(), "{records:?}");
    assert_eq!(
        std::fs::read_to_string(
            home.path()
                .join(&records[0].assets)
                .join("fixture_assets/a.txt")
        )
        .unwrap(),
        fixture_asset()
    );
    assert!(
        store::read_record(home.path(), "fixture", "job_fixture").is_err(),
        "no plugin record is written"
    );
}

/// A job step binds no directory, so a read-write bind is reachable only
/// through the executor that runs the step. This drives it on the record the
/// real pack install wrote, whose pack tier is materialized in the same home.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_write_bind_with_mounts_is_refused_with_the_next_call() {
    let db = test_db("plugin-job-mount-read-write").await;
    let home = tempfile::tempdir().expect("plugin home");
    install_job_fixture(&db, home.path(), job_settings()).await;
    let mut record = store::read_record(home.path(), "fixture", "job_fixture").expect("record");
    record.declaration.bind_dir = Some(PluginDirBinding {
        input_field: "path".into(),
        original_field: None,
        description: "a directory".into(),
        access: BindAccess::ReadWrite,
        write_fields: Vec::new(),
    });
    store::write_record(home.path(), &record).expect("write the record");
    let directory = tempfile::tempdir().expect("bound folder");

    let error = PluginExecutor::new(Some(home.path().to_path_buf()))
        .call_bound(
            &record,
            json!({}),
            BoundDir::new(directory.path(), None).expect("bind the folder"),
        )
        .await
        .expect_err("a read-write call cannot mount assets");
    let message = ErrorChain(&error).to_string();
    assert!(
        message.contains("this call writes; run it without mounted workflows"),
        "{message}"
    );
}

/// A repository with `.gents/workflows` and `.grok/workflows` under `sub`.
fn repo_with_workflow_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let repo = tempfile::tempdir().expect("repository");
    let root = repo.path().canonicalize().unwrap().join("repo");
    let cwd = root.join("sub");
    std::fs::create_dir_all(cwd.join(".gents/workflows")).unwrap();
    std::fs::create_dir_all(cwd.join(".grok/workflows")).unwrap();
    (repo, root, cwd)
}

fn settings_with_roots(roots: &[&Path]) -> PluginJobSettings {
    PluginJobSettings {
        policy: JobPolicy {
            project_roots: roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            ..job_settings().policy
        },
        ..job_settings()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_tier_appears_only_under_project_roots() {
    let (_repo, root, cwd) = repo_with_workflow_dirs();
    let (h, _home) = boot(
        "plugin-job-mount-project",
        settings_with_roots(&[&root]),
        &cwd,
        false,
    )
    .await;
    let row = completed_job(&h).await;
    let mounts = step_mounts(&h, &row).await;
    let classes: Vec<&str> = mounts
        .iter()
        .map(|mount| {
            if mount.class == "project" {
                "project"
            } else {
                "pack"
            }
        })
        .collect();
    assert_eq!(classes, ["pack", "project", "project"], "{mounts:?}");
    assert_eq!(pack_mount_dir(&mounts[1]), cwd.join(".gents/workflows"));
    assert_eq!(pack_mount_dir(&mounts[2]), cwd.join(".grok/workflows"));
    h.runtime.shutdown().await;

    let (_other_repo, other_root, other_cwd) = repo_with_workflow_dirs();
    let elsewhere = other_root.join("elsewhere");
    for (name, roots) in [
        ("plugin-job-mount-no-roots", Vec::new()),
        ("plugin-job-mount-other-root", vec![elsewhere.as_path()]),
    ] {
        let (h, _home) = boot(name, settings_with_roots(&roots), &other_cwd, false).await;
        let row = completed_job(&h).await;
        let mounts = step_mounts(&h, &row).await;
        assert!(
            mounts.iter().all(|mount| mount.class != "project"),
            "{name}: {mounts:?}"
        );
        assert_eq!(mounts.len(), 1, "{name}: {mounts:?}");
        h.runtime.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symlink_escape_from_gents_workflows_is_refused() {
    let repo = tempfile::tempdir().expect("repository");
    let root = repo.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().expect("outside folder");
    std::fs::create_dir_all(root.join(".gents")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join(".gents/workflows")).unwrap();
    std::fs::create_dir_all(root.join(".grok/workflows")).unwrap();

    let (h, _home) = boot(
        "plugin-job-mount-escape",
        settings_with_roots(&[&root]),
        &root,
        false,
    )
    .await;
    let row = completed_job(&h).await;
    let mounts = step_mounts(&h, &row).await;
    let project: Vec<PathBuf> = mounts
        .iter()
        .filter(|mount| mount.class == "project")
        .map(pack_mount_dir)
        .collect();
    assert_eq!(project, [root.join(".grok/workflows")], "{mounts:?}");
    let outside = outside.path().canonicalize().unwrap();
    assert!(
        mounts
            .iter()
            .all(|mount| !pack_mount_dir(mount).starts_with(&outside)),
        "{mounts:?}"
    );
    h.runtime.shutdown().await;
}
