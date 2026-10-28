use super::*;
use crate::plugin::approval::workspace::{self, Mode, Reviewer, WorkspaceKey};
use crate::toolset::file_tools::{content_hash, file_mutation_lock_for};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    key: WorkspaceKey,
    _pool: LspPool,
    lease: super::super::pool::LspLease,
    context: ToolContext,
}

impl Fixture {
    async fn new() -> Option<Self> {
        if !python3_available() {
            return None;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        let key = WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
        workspace::save_mode(
            &home,
            &key,
            0,
            Mode::AutoReview,
            Some(&workspace::ReviewTaskSelection::for_task(
                &workspace::default_review_task("node", "agent"),
            )),
        )
        .unwrap();
        let server = fixture_server(FIXTURE_PY.into());
        let config = sample_config(
            root.clone(),
            FileToolMode::ReadWrite,
            "consent",
            vec![server.clone()],
        );
        let pool = LspPool::new();
        let lease = pool
            .get_or_start(
                PoolKey {
                    runtime_home: None,
                    session_id: "consent".into(),
                    agent_id: "agent".into(),
                    workspace_root: root.clone(),
                    server_name: server.name.clone(),
                    config_digest: config.digest.clone(),
                    operator_guard_home: None,
                },
                &server,
                &config,
            )
            .await
            .unwrap();
        let context = ToolContext::new(root.clone(), false).unwrap();
        Some(Self {
            _directory: directory,
            root,
            home,
            key,
            _pool: pool,
            lease,
            context,
        })
    }

    fn edit(&self, path: &Path) -> super::super::edits::PreparedEdit {
        std::fs::write(path, "original\n").unwrap();
        super::super::edits::PreparedEdit {
            path: path.to_owned(),
            new_bytes: b"changed\n".to_vec(),
            expected_hash: Some(content_hash(b"original\n")),
            version: None,
            rename_from: None,
            rename_destination: None,
        }
    }

    async fn apply(
        &self,
        edits: &[super::super::edits::PreparedEdit],
        reviewer: Reviewer,
    ) -> Result<usize, crate::toolset::shared::ToolError> {
        let scope = workspace::for_request(
            Some(&self.home),
            "node",
            Some("requester"),
            "agent",
            Some(&self.key.path),
        )
        .unwrap()
        .unwrap()
        .with_reviewer(reviewer);
        workspace::scope(
            Some(scope),
            super::super::edits::apply_prepared_edits(&self.context, self.lease.client(), edits),
        )
        .await
    }
}

fn verdict(invocation: workspace::ReviewInvocation, allowed: bool) -> Option<String> {
    Some(
        serde_json::json!({ "actionId": invocation.action_id,
        "decision": if allowed { "allow" } else { "deny" },
        "reason": "Checked the requested file change." })
        .to_string(),
    )
}

#[tokio::test]
async fn lsp_edit_consent_is_complete_before_the_first_write() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let first = fixture.root.join("first.rs");
    let second = fixture.root.join("second.rs");
    let edits = [fixture.edit(&first), fixture.edit(&second)];
    let calls = Arc::new(AtomicUsize::new(0));
    let reviewer: Reviewer = Arc::new({
        let calls = calls.clone();
        move |invocation| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(invocation.payload["newText"], "changed\n");
            Box::pin(async move { Ok(verdict(invocation, index == 0)) })
        }
    });
    let error = fixture.apply(&edits, reviewer).await.unwrap_err();
    assert!(error.to_string().contains("not approved"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(std::fs::read_to_string(first).unwrap(), "original\n");
    assert_eq!(std::fs::read_to_string(second).unwrap(), "original\n");
}

#[tokio::test]
async fn lsp_edit_inside_the_original_workspace_does_not_request_review() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let path = fixture.key.path.join("lib.rs");
    let edits = [fixture.edit(&path)];
    let reviewer: Reviewer =
        Arc::new(|_| panic!("a routine workspace edit must not ask for review"));
    assert_eq!(fixture.apply(&edits, reviewer).await.unwrap(), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "changed\n");
}

#[tokio::test]
async fn lsp_edit_rechecks_approval_after_waiting_for_mutation_locks() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let path = fixture.root.join("outside.rs");
    let edits = [fixture.edit(&path)];
    let calls = Arc::new(AtomicUsize::new(0));
    let reviewer: Reviewer = Arc::new({
        let calls = calls.clone();
        move |invocation| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(verdict(invocation, true)) })
        }
    });
    let guard = file_mutation_lock_for(&path).lock_owned().await;
    let mut applying = Box::pin(tokio::task::unconstrained(fixture.apply(&edits, reviewer)));
    assert!(futures::poll!(applying.as_mut()).is_pending());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    workspace::save_mode(&fixture.home, &fixture.key, 1, Mode::FullAccess, None).unwrap();
    drop(guard);
    let error = applying.await.unwrap_err();
    assert!(error.to_string().contains("permissions changed"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "original\n");
}

#[tokio::test]
async fn lsp_edit_rechecks_operator_aliases_before_any_write() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let first = fixture.root.join("first.rs");
    let second = fixture.root.join("second.rs");
    let edits = [fixture.edit(&first), fixture.edit(&second)];
    let operator = workspace::preferences_file(&fixture.home);
    let original = std::fs::read(&operator).unwrap();
    crate::plugin::allowed::protect(&fixture.home);
    let guard = file_mutation_lock_for(&first).lock_owned().await;
    let mut applying = Box::pin(super::super::edits::apply_prepared_edits(
        &fixture.context,
        fixture.lease.client(),
        &edits,
    ));
    assert!(futures::poll!(applying.as_mut()).is_pending());
    std::fs::remove_file(&second).unwrap();
    std::fs::hard_link(&operator, &second).unwrap();
    drop(guard);
    let error = applying.await.unwrap_err();
    assert!(error.to_string().contains("operator"));
    assert_eq!(std::fs::read_to_string(first).unwrap(), "original\n");
    assert_eq!(std::fs::read(operator).unwrap(), original);
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_edit_rechecks_parent_paths_before_any_write() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let outside = tempfile::tempdir().unwrap();
    let first = fixture.root.join("first.rs");
    let folder = fixture.root.join("folder");
    std::fs::create_dir(&folder).unwrap();
    let second = folder.join("second.rs");
    let edits = [fixture.edit(&first), fixture.edit(&second)];
    std::fs::write(outside.path().join("second.rs"), "original\n").unwrap();
    let guard = file_mutation_lock_for(&first).lock_owned().await;
    let mut applying = Box::pin(super::super::edits::apply_prepared_edits(
        &fixture.context,
        fixture.lease.client(),
        &edits,
    ));
    assert!(futures::poll!(applying.as_mut()).is_pending());
    std::fs::rename(&folder, fixture.root.join("old-folder")).unwrap();
    std::os::unix::fs::symlink(outside.path(), &folder).unwrap();
    drop(guard);
    let error = applying.await.unwrap_err();
    assert!(error.to_string().contains("outside the allowed tool root"));
    assert_eq!(std::fs::read_to_string(first).unwrap(), "original\n");
    assert_eq!(
        std::fs::read_to_string(outside.path().join("second.rs")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn lsp_rename_rechecks_the_source_content_before_moving_it() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let source = fixture.root.join("before.rs");
    let destination = fixture.root.join("after.rs");
    std::fs::write(&source, "original\n").unwrap();
    let edits = super::super::edits::prepare_workspace_edit(&fixture.context, &serde_json::json!({
        "documentChanges": [{ "kind":"rename", "oldUri": super::super::uri::path_to_file_uri(&source),
            "newUri": super::super::uri::path_to_file_uri(&destination) }],
    }), super::super::encoding::PositionEncoding::Utf16).unwrap();
    std::fs::write(&source, "changed elsewhere\n").unwrap();
    let error =
        super::super::edits::apply_prepared_edits(&fixture.context, fixture.lease.client(), &edits)
            .await
            .unwrap_err();
    assert!(error.to_string().contains("changed between preflight"));
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_to_string(source).unwrap(),
        "changed elsewhere\n"
    );
}

#[tokio::test]
async fn lsp_rename_does_not_overwrite_a_changed_destination() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    for existed in [false, true] {
        let source = fixture.root.join("source.rs");
        let destination = fixture.root.join("destination.rs");
        std::fs::write(&source, "source\n").unwrap();
        if existed {
            std::fs::write(&destination, "old destination\n").unwrap();
        }
        let rename =
            super::super::edits::prepare_rename(source.clone(), destination.clone()).unwrap();
        std::fs::write(&destination, "changed elsewhere\n").unwrap();
        let error = super::super::edits::apply_prepared_edits(
            &fixture.context,
            fixture.lease.client(),
            &[rename],
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("changed between preflight and rename"));
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "source\n");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "changed elsewhere\n"
        );
        std::fs::remove_file(destination).unwrap();
    }
}
