use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::adapter::DIRTY_BASE_SUMMARY_LIMIT;
use super::fixture::*;
use super::*;

#[test]
fn builtin_emitter_omits_absolute_destination() {
    let action = CreateWorkspaceAction {
        writer_agent_did: None,
        writer_agent_doc_id: None,
        integrator_agent_did: None,
        integrator_agent_doc_id: None,
        readers: Vec::new(),
        path_capability: WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
        workspace_id: "ws-1".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: "abc".into(),
        branch: "topic".into(),
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::MakeWorktree,
        clone_artifacts: None,
    };
    let plan: ActionPlan = emit_create_workspace_plan(action);
    assert!(action_journal_prefix_legal(&[]));
    assert_eq!(
        DEFAULT_MAKE_WORKTREE_ARTIFACTS,
        ["target/", "crates/gents/proofs/.lake"]
    );
    let json = serde_json::to_value(&plan).unwrap();
    let encoded = json.to_string();
    assert!(!encoded.contains("host_path"));
    assert!(!encoded.contains("/tmp"));
    assert_eq!(json["abi"], 1);
    assert_eq!(json["actions"][0]["type"], "create_workspace");
    assert!(serde_json::from_value::<HostAction>(serde_json::json!({
        "type": "create_workspace",
        "workspace_id": "ws-1",
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "host_path": "/tmp/evil"
    }))
    .is_err());
    assert!(
        serde_json::from_value::<ActionPlan>(serde_json::json!({
            "abi": 1,
            "actions": [{
                "type": "create_workspace",
                "workspace_id": "ws-1",
                "work_unit_id": "unit-1",
                "repository_id": "repo-1",
                "base_sha": "abc",
                "branch": "topic"
            }],
            "host_path": "/tmp/evil"
        }))
        .is_err(),
        "destination on the plan root must be Denied, not dropped"
    );
}

#[test]
fn isolated_workspace_mutation_has_no_host_path() {
    let doc = IsolatedWorkspaceDoc {
        path_capability: WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
        workspace_id: "ws-1".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: "abc".into(),
        branch: "topic".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_node_did: "did:key:zWorkspaceOwner".into(),
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
        route: Default::default(),
    };
    let mutation = isolated_workspace_upsert_mutation(&doc);
    assert!(!mutation.contains("host_path"));
    assert!(mutation.contains("owner_node_did: { _eq: \"did:key:zWorkspaceOwner\" }"));
    assert!(mutation.contains("upsert_IsolatedWorkspace"));
    assert!(!mutation.contains("create_IsolatedWorkspace"));
    assert!(mutation.contains("seal_hash: null"));
    assert!(mutation.contains("instruction_manifest:"));
    let placement = WorkspacePlacementDoc {
        workspace_id: "ws-1".into(),
        owner_node_did: "did:key:zWorkspaceOwner".into(),
        host_path: "/tmp/ws".into(),
        repository_placement_id: "repo-1".into(),
        adapter: "git_worktree".into(),
        adapter_version: "gents-workspace-adapter/1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "{}".into(),
        observed_tree_hash: "tree".into(),
    };
    let placement_mutation =
        workspace_placement_upsert_mutation(&placement, "2026-08-21T00:00:00Z");
    assert!(placement_mutation.contains("host_path:"));
    assert!(placement_mutation.contains("owner_node_did: { _eq: \"did:key:zWorkspaceOwner\" }"));
    assert!(!placement_mutation.contains("[]"));
    let repository_mutation = repository_placement_upsert_mutation(
        &RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            owner_node_did: "did:key:zWorkspaceOwner".into(),
            host_path: PathBuf::from("/tmp/repo\"quoted"),
            enabled: true,
        },
        "2026-08-21T00:00:00Z",
    )
    .unwrap();
    assert!(repository_mutation.contains("upsert_RepositoryPlacement"));
    assert!(repository_mutation.contains("node_did: { _eq: \"did:key:zWorkspaceOwner\" }"));
    assert!(repository_mutation.contains("/tmp/repo\\\"quoted"));
    assert!(!repository_mutation.contains("[]"));
    let receipt = WorkspaceReceiptDoc {
        path_capability_digest: WorkspacePathCapability::exact_paths(Vec::new())
            .unwrap()
            .digest(),
        receipt_id: "receipt-writer-ws-1-req-1".into(),
        workspace_id: "ws-1".into(),
        produced_by_request_id: "req-1".into(),
        produced_by_request_doc_id: "doc-1".into(),
        kind: "writer".into(),
        base_sha: "abc".into(),
        seal_hash: "tree".into(),
        work_unit_id: Some("unit-1".into()),
        caused_by_correlation: Some("corr-1".into()),
        head_sha: None,
        changed_files: None,
        diff_artifact: None,
        unresolved_conflicts: None,
        integration_instructions: None,
        route: Default::default(),
    };
    let receipt_mutation = workspace_receipt_create_mutation(&receipt);
    assert!(receipt_mutation.contains("upsert_WorkspaceReceipt"));
    assert!(receipt_mutation.contains("changed_files: null"));
    assert!(!receipt_mutation.contains("[]"));
}

#[test]
fn workspace_creation_rejects_foreign_repository_owner_before_effects() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let plan = emit_create_workspace_plan(fx.action("ws-foreign-owner", "unit-1", "foreign-owner"));
    let mut journal = Vec::new();
    let mut ctx = fx.ctx(&mut docs, git_worktree_caps());
    ctx.repository.owner_node_did = "did:key:zOtherOwner".into();
    let error = execute_create_workspace_plan(&plan, &mut journal, &mut ctx).unwrap_err();
    assert!(matches!(error, HostExecuteError::Denied { .. }));
    assert!(
        journal.is_empty(),
        "foreign principal cannot start an action journal"
    );
    assert!(docs.workspaces.is_empty());
    assert!(docs.placements.is_empty());
    assert!(!fx.parent().join("foreign-owner").exists());
}

#[test]
fn workspace_creation_matches_the_lean_provision_model() {
    gents_lean_vocab::families::workspace_provision_create::replay::<String, String>(|input| {
        let fx = Fixture::new();
        let other = fx.parent().join("other-ceiling");
        fs::create_dir_all(&other).unwrap();
        let mut docs = MemoryWorkspaceDocuments::default();
        let unit = |n: u64| format!("unit-{n}");
        let planned = |action: &CreateWorkspaceAction| {
            workspace_host_path(
                &fx.repo,
                &action.workspace_id,
                &action.branch,
                Some(&fx.repo),
            )
            .unwrap()
        };
        if let Some(prior) = input.prior_work_unit {
            let action = fx.action("ws-provision", &unit(prior), "topic-provision");
            execute_create_workspace_plan(
                &emit_create_workspace_plan(action),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, git_worktree_caps()),
            )
            .expect("prior creation");
        }
        let action = fx.action("ws-provision", &unit(input.work_unit), "topic-provision");
        let dest = planned(&action);
        if input.target == "foreign" {
            fs::create_dir_all(&dest).unwrap();
            fs::write(dest.join("keep-me.txt"), "untouched\n").unwrap();
        }
        let mut ctx = fx.ctx(&mut docs, git_worktree_caps());
        if input.ceiling_escapes {
            ctx.ceiling = Some(&other);
        }
        let result = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut ctx,
        );
        let (result, identity_mismatch, outcome_state) = match &result {
            Ok(outcome) => (
                "ready",
                false,
                Some(outcome.workspace.lifecycle_state.clone()),
            ),
            Err(HostExecuteError::Denied { .. }) => ("denied", false, None),
            Err(error) => (
                "failed",
                error.identity_mismatch(),
                error
                    .outcome()
                    .map(|outcome| outcome.workspace.lifecycle_state.clone()),
            ),
        };
        let stored = docs.load_isolated_workspace("ws-provision").unwrap();
        assert_eq!(
            docs.workspaces.len(),
            usize::from(stored.is_some()),
            "creation stores no workspace row other than its own"
        );
        let readme = fs::read_to_string(dest.join("README.md")).ok();
        let keep = fs::read_to_string(dest.join("keep-me.txt")).ok();
        let target = match (readme.as_deref(), keep.as_deref()) {
            (None, None) if !dest.exists() => "absent",
            (None, Some("untouched\n")) => "foreign",
            (Some("hello\n"), None) => "checkout",
            other => panic!("unmodeled destination contents {other:?}"),
        };
        gents_lean_vocab::families::workspace_provision_create::ProvisionOutput {
            result: result.to_owned(),
            identity_mismatch,
            outcome_state,
            stored_state: stored.as_ref().map(|row| row.lifecycle_state.clone()),
            stored_work_unit: stored.as_ref().map(|row| {
                row.work_unit_id
                    .strip_prefix("unit-")
                    .and_then(|n| n.parse().ok())
                    .expect("modeled work unit")
            }),
            target: target.to_owned(),
            row_carries_dirty_base: stored
                .as_ref()
                .is_some_and(|row| row.dirty_base_on_replicated_row()),
        }
    });
}

#[test]
fn recover_from_executing_observes_existing_effect() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-recover", "unit-1", "topic-recover");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let first = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");

    docs.workspaces.clear();
    docs.placements.clear();
    let mut journal = vec![ActionJournalEntry::new(0, ActionJournalState::Executing)];
    let recovered = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("recover from Executing");
    assert_eq!(recovered.workspace.lifecycle_state, "ready");
    assert_eq!(recovered.placement.host_path, first.placement.host_path);
    assert_eq!(
        journal.last().map(|entry| entry.state),
        Some(ActionJournalState::ResultDocsWritten)
    );
}

#[test]
fn dirty_base_is_recorded_on_placement_not_copied() {
    let fx = Fixture::new();
    fs::write(fx.repo.join("dirty.txt"), "only in source\n").unwrap();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-dirty", "unit-1", "topic-dirty");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let outcome = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    assert!(outcome.placement.dirty_base);
    assert!(outcome.placement.dirty_base_summary.contains("dirty.txt"));
    let dest = PathBuf::from(&outcome.placement.host_path);
    assert!(
        !dest.join("dirty.txt").exists(),
        "git_worktree must not copy dirty source files"
    );
    assert!(dest.join("README.md").is_file());
    let isolated = docs.load_isolated_workspace("ws-dirty").unwrap().unwrap();
    assert!(!isolated.dirty_base_on_replicated_row());
}

#[test]
fn make_worktree_clones_artifacts_git_worktree_does_not() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.repo.join("target")).unwrap();
    fs::write(fx.repo.join("target").join("cache.bin"), "warm").unwrap();
    fs::create_dir_all(fx.repo.join("crates/gents/proofs/.lake")).unwrap();
    fs::write(
        fx.repo.join("crates/gents/proofs/.lake").join("pkg"),
        "mathlib",
    )
    .unwrap();

    let mut docs = MemoryWorkspaceDocuments::default();
    let mut make = fx.action("ws-make", "unit-1", "topic-make");
    make.adapter = WorkspaceAdapterKind::MakeWorktree;
    let plan = emit_create_workspace_plan(make);
    let mut journal = Vec::new();
    let make_out =
        execute_create_workspace_plan(&plan, &mut journal, &mut fx.ctx(&mut docs, capabilities()))
            .expect("make_worktree");
    let make_dest = PathBuf::from(&make_out.placement.host_path);
    assert_eq!(
        fs::read_to_string(make_dest.join("target").join("cache.bin")).unwrap(),
        "warm"
    );
    assert_eq!(
        fs::read_to_string(make_dest.join("crates/gents/proofs/.lake").join("pkg")).unwrap(),
        "mathlib"
    );

    let mut docs = MemoryWorkspaceDocuments::default();
    let git_only = fx.action("ws-git", "unit-1", "topic-git");
    let plan = emit_create_workspace_plan(git_only);
    let mut journal = Vec::new();
    let git_out = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("git_worktree");
    let git_dest = PathBuf::from(&git_out.placement.host_path);
    assert!(!git_dest.join("target").exists());
    assert!(!git_dest.join("crates/gents/proofs/.lake").exists());
}

#[test]
fn provision_succeeds_when_ceiling_is_repository_checkout() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-under", "unit-1", "topic-under");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let mut ctx = fx.ctx(&mut docs, git_worktree_caps());
    ctx.ceiling = Some(&fx.repo);
    let created = execute_create_workspace_plan(&plan, &mut journal, &mut ctx)
        .expect("provision under checkout ceiling");
    let dest = PathBuf::from(&created.placement.host_path);
    let checkout = fx.repo.canonicalize().unwrap();
    let dest = dest.canonicalize().unwrap_or(dest);
    assert!(
        dest.starts_with(&checkout),
        "dest {} must sit under checkout {}",
        dest.display(),
        checkout.display()
    );
    assert_ne!(dest, checkout);
    assert!(dest.join("README.md").is_file());
    let exclude = git(&fx.repo, &["rev-parse", "--git-path", "info/exclude"]);
    let exclude_path = if Path::new(&exclude).is_absolute() {
        PathBuf::from(&exclude)
    } else {
        fx.repo.join(exclude)
    };
    let body = fs::read_to_string(exclude_path).unwrap();
    assert!(
        body.contains(".gents/"),
        "nested dest must stay out of operator git status: {body}"
    );
}

#[test]
fn dirty_base_observation_survives_multibyte_porcelain() {
    let fx = Fixture::new();
    let name = "文".repeat(80);
    for i in 0..16 {
        fs::write(fx.repo.join(format!("{name}-{i}")), "x").unwrap();
    }
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-cjk", "unit-1", "topic-cjk");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let outcome = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("multibyte dirty porcelain must not panic");
    assert!(outcome.placement.dirty_base);
    assert!(outcome.placement.dirty_base_summary.len() <= DIRTY_BASE_SUMMARY_LIMIT);
}

#[test]
fn make_worktree_resumes_missing_artifact_dirs_on_match() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.repo.join("target")).unwrap();
    fs::write(fx.repo.join("target").join("cache.bin"), "warm").unwrap();
    fs::create_dir_all(fx.repo.join("crates/gents/proofs/.lake")).unwrap();
    fs::write(
        fx.repo.join("crates/gents/proofs/.lake").join("pkg"),
        "mathlib",
    )
    .unwrap();

    let mut docs = MemoryWorkspaceDocuments::default();
    let mut make = fx.action("ws-resume", "unit-1", "topic-resume");
    make.adapter = WorkspaceAdapterKind::MakeWorktree;
    let plan = emit_create_workspace_plan(make);
    let mut journal = Vec::new();
    let first =
        execute_create_workspace_plan(&plan, &mut journal, &mut fx.ctx(&mut docs, capabilities()))
            .expect("make_worktree");
    let dest = PathBuf::from(&first.placement.host_path);
    fs::remove_dir_all(dest.join("crates/gents/proofs/.lake")).unwrap();
    assert!(!dest.join("crates/gents/proofs/.lake").exists());

    let mut journal = Vec::new();
    execute_create_workspace_plan(&plan, &mut journal, &mut fx.ctx(&mut docs, capabilities()))
        .expect("resume clone");
    assert_eq!(
        fs::read_to_string(dest.join("crates/gents/proofs/.lake").join("pkg")).unwrap(),
        "mathlib"
    );
    assert_eq!(
        fs::read_to_string(dest.join("target").join("cache.bin")).unwrap(),
        "warm"
    );
}

#[test]
fn provision_failed_is_terminal_and_does_not_become_ready() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-failed", "unit-1", "topic-failed");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    docs.workspaces
        .get_mut("ws-failed")
        .expect("row")
        .lifecycle_state = "provisionFailed".to_string();

    let mut journal = Vec::new();
    let err = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect_err("provisionFailed must not become ready");
    assert!(err.to_string().contains("provisionFailed"), "{err}");
    let stored = docs.load_isolated_workspace("ws-failed").unwrap().unwrap();
    assert_eq!(stored.lifecycle_state, "provisionFailed");
}

trait IsolatedWorkspaceDocExt {
    fn dirty_base_on_replicated_row(&self) -> bool;
}

impl IsolatedWorkspaceDocExt for IsolatedWorkspaceDoc {
    fn dirty_base_on_replicated_row(&self) -> bool {
        let encoded = serde_json::to_string(self).unwrap();
        encoded.contains("dirty_base") || encoded.contains("host_path")
    }
}

#[test]
fn writer_seal_persists_receipt_and_forbids_read_write() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-seal", "unit-1", "topic-seal");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();

    let mut writer = super::binding::new_binding(
        "ws-seal",
        "req-writer",
        "req-writer-doc",
        gents_loop::tool_policy::WorkspaceAuthority::ReadWrite,
        "did:key:zWorkspaceOwner",
        None,
    );
    docs.write_binding(writer.clone()).unwrap();

    let sealed = seal_writer(&fx, &mut docs, "ws-seal", "req-writer");
    assert_eq!(sealed.workspace.lifecycle_state, "sealed");
    assert!(sealed.workspace.seal_hash.is_some());
    assert_eq!(
        sealed.placement.observed_tree_hash,
        sealed.workspace.seal_hash.clone().unwrap()
    );
    assert_eq!(sealed.receipt.kind, "writer");
    assert_eq!(sealed.receipt.produced_by_request_id, "req-writer");
    assert!(sealed
        .receipt
        .changed_files
        .as_deref()
        .is_some_and(|files| files.contains("patch.rs")));
    writer = docs
        .load_bindings("ws-seal")
        .unwrap()
        .into_iter()
        .find(|binding| binding.request_id == "req-writer")
        .unwrap();
    assert_eq!(writer.lifecycle_state, "released");

    let err = super::binding::admit_workspace_binding(
        "ws-seal",
        &sealed.workspace.lifecycle_state,
        sealed.workspace.seal_hash.as_deref(),
        &docs.load_bindings("ws-seal").unwrap(),
        super::binding::new_binding(
            "ws-seal",
            "req-writer-2",
            "req-writer-2-doc",
            gents_loop::tool_policy::WorkspaceAuthority::ReadWrite,
            "did:key:zWorkspaceOwner",
            sealed.workspace.seal_hash.as_deref(),
        ),
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not bindable"), "{err:#}");
}

#[test]
fn seal_snapshot_preserves_tracked_files_hidden_by_ignore_rules() {
    let mut fx = Fixture::new();
    let ignored = fx.repo.join("demo-runs");
    fs::create_dir_all(&ignored).unwrap();
    fs::write(fx.repo.join(".gitignore"), "demo-runs/\n").unwrap();
    fs::write(ignored.join(".gitignore"), "*\n!.gitignore\n").unwrap();
    git(&fx.repo, &["add", ".gitignore"]);
    git(&fx.repo, &["add", "-f", "demo-runs/.gitignore"]);
    git(&fx.repo, &["commit", "-m", "track ignored sentinel"]);
    fx.base_sha = git(&fx.repo, &["rev-parse", "HEAD"]);

    let mut docs = MemoryWorkspaceDocuments::default();
    let plan = emit_create_workspace_plan(fx.action("ws-ignore", "unit-ignore", "topic-ignore"));
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();

    let snapshot = super::adapter::capture_seal_snapshot(&dest, &created.workspace.identity())
        .expect("snapshot");
    assert_eq!(snapshot.changed_files, vec!["patch.rs"]);
    let diff = String::from_utf8(snapshot.diff).unwrap();
    assert!(!diff.contains("demo-runs/.gitignore"), "{diff}");
}

#[test]
fn concurrent_read_only_after_seal_with_matching_hash() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-ro", "unit-1", "topic-ro");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let sealed = seal_writer(&fx, &mut docs, "ws-ro", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();

    let first = super::binding::admit_workspace_binding(
        "ws-ro",
        "sealed",
        Some(&hash),
        &[],
        super::binding::new_binding(
            "ws-ro",
            "req-review-a",
            "doc-a",
            gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            "did:key:zWorkspaceOwner",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let super::binding::AdmitBinding::Create { binding: a, .. } = first else {
        panic!("expected create");
    };
    let second = super::binding::admit_workspace_binding(
        "ws-ro",
        "sealed",
        Some(&hash),
        std::slice::from_ref(&a),
        super::binding::new_binding(
            "ws-ro",
            "req-review-b",
            "doc-b",
            gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            "did:key:zWorkspaceOwner",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let super::binding::AdmitBinding::Create { binding: b, .. } = second else {
        panic!("expected concurrent create");
    };
    assert_eq!(a.seal_hash.as_deref(), Some(hash.as_str()));
    assert_eq!(b.seal_hash.as_deref(), Some(hash.as_str()));
    assert!(a.is_active());
    assert!(b.is_active());
}

#[tokio::test]
async fn seal_drift_fails_closed() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-drift", "unit-1", "topic-drift");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    let sealed = seal_writer(&fx, &mut docs, "ws-drift", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    fs::write(dest.join("after-seal.txt"), "mutated after review\n").unwrap();
    let live = super::adapter::working_tree_hash(&dest).await.unwrap();
    assert_ne!(live, hash);

    let mut workspace = super::IsolatedWorkspaceRecord {
        workspace_id: "ws-drift".into(),
        work_unit_id: None,
        caused_by_invocation_id: None,
        owner_node_did: "did:key:zWorkspaceOwner".into(),
        writer_principal: "did:key:zWriter".into(),
        integrator_principal: "did:key:zIntegrator".into(),
        lifecycle_state: "sealed".into(),
        seal_hash: Some(hash.clone()),
        instruction_manifest: sealed.workspace.instruction_manifest.clone(),
    };
    let mut placed = super::WorkspacePlacementRecord {
        workspace_id: "ws-drift".into(),
        owner_node_did: "did:key:zWorkspaceOwner".into(),
        host_path: dest.to_string_lossy().into_owned(),
        observed_tree_hash: Some(hash.clone()),
    };
    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            workspace_id: "ws-drift",
            authority: gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            seal_hash: Some(&hash),
            request_cwd: None,
            node_did: "did:key:zWorkspaceOwner",
            operator_tool_root: Some(fx.parent()),
            workspace_write_sandbox_enforced: false,
            live_tree_hash: Some(&live),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("live tree hash"), "{error:#}");
    workspace.seal_hash = Some(hash.clone());
    placed.observed_tree_hash = Some("stale".into());
    let stored = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            workspace_id: "ws-drift",
            authority: gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            seal_hash: Some(&hash),
            request_cwd: None,
            node_did: "did:key:zWorkspaceOwner",
            operator_tool_root: Some(fx.parent()),
            workspace_write_sandbox_enforced: false,
            live_tree_hash: Some(&hash),
        },
    )
    .unwrap_err();
    assert!(
        stored
            .to_string()
            .contains("observed_tree_hash stale does not match"),
        "{stored:#}"
    );
}

#[test]
fn frozen_agents_md_is_used_instead_of_live_writer_tree() {
    let mut fx = Fixture::new();
    fx.commit("AGENTS.md", "frozen-base-instructions\n");
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-agents", "unit-1", "topic-agents");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("AGENTS.md"), "live-writer-instructions\n").unwrap();

    let manifest = InstructionManifest::parse(&created.workspace.instruction_manifest)
        .expect("instruction_manifest");
    assert_eq!(manifest.base_sha, fx.base_sha);
    let agents = manifest
        .files
        .iter()
        .find(|file| file.path == "AGENTS.md")
        .expect("AGENTS.md from base_sha");
    assert!(agents.text.contains("frozen-base-instructions"));
    assert!(!agents.text.contains("live-writer-instructions"));

    let section = instruction_context_section(&created.workspace.instruction_manifest).unwrap();
    assert!(section.contains("frozen-base-instructions"));
    assert!(!section.contains("live-writer-instructions"));
    let live = fs::read_to_string(dest.join("AGENTS.md")).unwrap();
    assert!(live.contains("live-writer-instructions"));

    let sealed = seal_writer(&fx, &mut docs, "ws-agents", "req-writer");
    let sealed_manifest = InstructionManifest::parse(&sealed.workspace.instruction_manifest)
        .expect("sealed manifest");
    let sealed_agents = sealed_manifest
        .files
        .iter()
        .find(|file| file.path == "AGENTS.md")
        .expect("AGENTS.md still frozen");
    assert!(sealed_agents.text.contains("frozen-base-instructions"));
    assert!(!sealed_agents.text.contains("live-writer-instructions"));
}

#[test]
fn already_sealed_repairs_placement_hash_and_receipt() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-repair", "unit-1", "topic-repair");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-repair", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    docs.placements
        .get_mut("ws-repair")
        .unwrap()
        .observed_tree_hash = created.placement.observed_tree_hash.clone();
    docs.receipts.clear();

    let repaired = seal_writer(&fx, &mut docs, "ws-repair", "req-writer");
    assert_eq!(repaired.workspace.lifecycle_state, "sealed");
    assert_eq!(repaired.placement.observed_tree_hash, hash);
    assert_eq!(repaired.receipt.produced_by_request_id, "req-writer");
    assert_eq!(repaired.receipt.seal_hash, hash);
}

#[test]
fn instruction_manifest_fails_closed_on_git_show_error() {
    let fx = Fixture::new();
    let err = super::adapter::capture_instruction_manifest(&fx.repo, "not-a-commit")
        .expect_err("invalid base_sha must not become empty files");
    assert!(err.to_string().contains("git show"), "{err:#}");
}

#[test]
fn seal_workspace_plan_omits_host_path() {
    let plan = emit_seal_workspace_plan(SealWorkspaceAction {
        workspace_id: "ws-1".into(),
        produced_by_request_id: "req-1".into(),
        produced_by_request_doc_id: "doc-1".into(),
    });
    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["actions"][0]["type"], "seal_workspace");
    assert!(json["actions"][0].get("host_path").is_none());
    assert!(serde_json::from_value::<HostAction>(serde_json::json!({
        "type": "seal_workspace",
        "workspace_id": "ws-1",
        "produced_by_request_id": "req-1",
        "produced_by_request_doc_id": "doc-1",
        "host_path": "/tmp/evil"
    }))
    .is_err());
}

fn cleanup_ws(
    fx: &Fixture,
    docs: &mut MemoryWorkspaceDocuments,
    workspace_id: &str,
) -> CleanupWorkspaceOutcome {
    let plan = emit_cleanup_workspace_plan(CleanupWorkspaceAction {
        workspace_id: workspace_id.to_string(),
    });
    let mut journal = Vec::new();
    execute_cleanup_workspace_plan(&plan, &mut journal, &mut fx.ctx(docs, cleanup_caps()))
        .expect("cleanup")
}

#[test]
fn integrate_denied_before_sealed() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-ready", "unit-1", "topic-ready");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    bind_integrate(&mut docs, "ws-ready", "req-int", "not-sealed");
    let err = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-ready".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect_err("Ready workspace cannot integrate");
    assert!(
        err.to_string().contains("cannot be integrated")
            || err.to_string().contains("require Sealed"),
        "{err}"
    );
    assert!(
        docs.load_receipts("ws-ready")
            .unwrap()
            .iter()
            .all(|receipt| receipt.kind != "integrator"),
        "no integrator receipt before Sealed"
    );
}

#[test]
fn merge_to_trunk_mode_is_denied_for_git_worktree_diff() {
    let action = IntegrateWorkspaceAction {
        workspace_id: "ws-1".into(),
        produced_by_request_id: "req-int".into(),
        produced_by_request_doc_id: "doc".into(),
        mode: IntegrateMode::MergeToTrunk,
    };
    let plan = emit_integrate_workspace_plan(action);
    let err = plan
        .validate_against(&integrate_caps())
        .expect_err("merge_to_trunk is not v1");
    assert!(
        err.to_string().contains("apply_diff") || err.to_string().contains("not implemented"),
        "{err:#}"
    );
}

#[test]
fn cleanup_is_explicit_and_leaves_disk_until_called() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-clean", "unit-1", "topic-clean");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    assert!(dest.is_dir());

    let sealed = seal_writer(&fx, &mut docs, "ws-clean", "req-writer");
    assert!(dest.is_dir(), "seal must not remove the worker tree");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-clean", "req-int", &hash);
    integrate_writer(&fx, &mut docs, "ws-clean", "req-int");
    assert!(
        dest.is_dir(),
        "integrate must not implicit-cleanup the worker tree"
    );
    assert!(fx.repo.join("patch.rs").is_file());

    let cleaned = cleanup_ws(&fx, &mut docs, "ws-clean");
    assert_eq!(cleaned.workspace.lifecycle_state, "cleaned");
    assert!(
        !dest.exists(),
        "explicit cleanup_workspace removes the worker tree"
    );
    assert!(
        fx.repo.join("README.md").is_file(),
        "cleanup must not delete the source checkout"
    );
    assert!(fx.repo.join("patch.rs").is_file());
    let mutation = super::documents::workspace_cleanup_docs_mutation(&cleaned.workspace, &[]);
    assert!(mutation.contains("lifecycle_state: \"cleaned\""));
    assert!(!mutation.contains("host_path"));
    assert!(mutation.contains("owner_node_did: { _eq: \"did:key:zWorkspaceOwner\" }"));
}

#[test]
fn provision_failed_and_sealed_rejected_remain_until_cleanup() {
    let fx = Fixture::new();
    let dest = fx.parent().join("foreign-left");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep-me.txt"), "untouched\n").unwrap();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("foreign-left", "unit-1", "topic-left");
    let planned = workspace_host_path(
        &fx.repo,
        &action.workspace_id,
        &action.branch,
        Some(&fx.repo),
    )
    .unwrap();
    if let Some(parent) = planned.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::rename(&dest, &planned).unwrap();
    fs::write(planned.join("keep-me.txt"), "untouched\n").unwrap();
    let plan = emit_create_workspace_plan(action);
    let err = execute_create_workspace_plan(
        &plan,
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect_err("mismatch");
    assert_eq!(
        err.outcome()
            .map(|outcome| outcome.workspace.lifecycle_state.as_str()),
        Some("provisionFailed")
    );
    assert!(
        planned.join("keep-me.txt").is_file(),
        "ProvisionFailed leaves disk for inspection"
    );

    let cleaned = cleanup_ws(&fx, &mut docs, "foreign-left");
    assert_eq!(cleaned.workspace.lifecycle_state, "provisionFailed");
    assert!(
        !planned.exists(),
        "explicit cleanup removes ProvisionFailed leftovers"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the retried integration needs a later wall-clock second"
)]
fn integrate_does_not_commit_until_receipt_and_retries_from_journal() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-crash", "unit-1", "topic-crash");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-crash", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-crash", "req-int", &hash);
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);

    let first = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-crash".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("prepare integrate");
    assert!(
        first.pending_head_sha.is_some(),
        "HEAD must not move before receipt flush"
    );
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), base);
    assert!(
        !fx.repo.join("patch.rs").exists(),
        "trunk worktree unchanged until finalize"
    );
    assert_eq!(first.receipt.kind, "integrator");

    docs.receipts.remove(&first.receipt.receipt_id);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let second = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-crash".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("retry observes pending commit");
    assert_eq!(second.receipt.seal_hash, hash);
    assert_eq!(second.receipt.head_sha, first.receipt.head_sha);
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), base);

    finalize_integrate_trunk(&fx.repo, second.pending_head_sha.as_deref()).unwrap();
    assert_eq!(
        fs::read_to_string(fx.repo.join("patch.rs")).unwrap(),
        "fn patch() {}\n"
    );
    let count = git(&fx.repo, &["rev-list", "--count", "HEAD"])
        .parse::<u32>()
        .unwrap();
    let base_count = git(&fx.repo, &["rev-list", "--count", &base])
        .parse::<u32>()
        .unwrap();
    assert_eq!(
        count,
        base_count + 1,
        "retry must not mint a second trunk commit"
    );
}

#[test]
fn integrate_uses_isolated_index_and_ignores_unrelated_staged_files() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-idx", "unit-1", "topic-idx");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-idx", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-idx", "req-int", &hash);

    fs::write(fx.repo.join("unrelated.txt"), "operator wip\n").unwrap();
    git(&fx.repo, &["add", "unrelated.txt"]);
    integrate_writer(&fx, &mut docs, "ws-idx", "req-int");
    let show = git(
        &fx.repo,
        &["show", "--name-only", "--pretty=format:", "HEAD"],
    );
    assert!(show.contains("patch.rs"), "{show}");
    assert!(
        !show.contains("unrelated.txt"),
        "integrator commit must not swallow staged operator files: {show}"
    );
    let staged = git(&fx.repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("unrelated.txt"),
        "unrelated staged file stays in the default index"
    );
}

#[test]
fn integrate_binary_file_round_trips() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-bin", "unit-1", "topic-bin");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    let bytes: Vec<u8> = (0u8..=255).collect();
    fs::write(dest.join("blob.bin"), &bytes).unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-bin", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-bin", "req-int", &hash);
    integrate_writer(&fx, &mut docs, "ws-bin", "req-int");
    let got = fs::read(fx.repo.join("blob.bin")).unwrap();
    assert_eq!(got, bytes);
}

#[test]
fn integrate_deletes_and_renames_update_trunk_worktree() {
    let mut fx = Fixture::new();
    fx.commit("gone.rs", "delete me\n");
    fx.commit("old.rs", "rename me\n");
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-del", "unit-1", "topic-del");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::remove_file(dest.join("gone.rs")).unwrap();
    fs::rename(dest.join("old.rs"), dest.join("new.rs")).unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-del", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-del", "req-int", &hash);
    integrate_writer(&fx, &mut docs, "ws-del", "req-int");

    assert!(
        !fx.repo.join("gone.rs").exists(),
        "deleted file must leave the trunk worktree"
    );
    assert!(
        git(&fx.repo, &["ls-files", "--", "gone.rs"]).is_empty(),
        "deleted file must leave the default index"
    );
    assert!(
        !fx.repo.join("old.rs").exists(),
        "rename source must leave the trunk worktree"
    );
    assert!(
        git(&fx.repo, &["ls-files", "--", "old.rs"]).is_empty(),
        "rename source must leave the default index"
    );
    assert_eq!(
        fs::read_to_string(fx.repo.join("new.rs")).unwrap(),
        "rename me\n"
    );
    assert_eq!(git(&fx.repo, &["ls-files", "--", "new.rs"]), "new.rs");
}

#[test]
fn integrate_dirty_trunk_matches_the_lean_model() {
    for case in gents_lean_vocab::families::workspace_dirty_trunk_integrate::cases() {
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let action = fx.action("ws-dirty", "unit-1", "topic-dirty");
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .expect("provision");
        let dest = PathBuf::from(&created.placement.host_path);
        fs::write(dest.join(&case.input.delta_path), "writer\n").unwrap();
        let sealed = seal_writer(&fx, &mut docs, "ws-dirty", "req-writer");
        let hash = sealed.workspace.seal_hash.clone().unwrap();
        bind_integrate(&mut docs, "ws-dirty", "req-int", &hash);
        if let Some(dirty) = &case.input.dirty_path {
            fs::write(fx.repo.join(dirty), "operator wip\n").unwrap();
        }
        let base = git(&fx.repo, &["rev-parse", "HEAD"]);
        let result = execute_integrate_workspace_plan(
            &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
                workspace_id: "ws-dirty".into(),
                produced_by_request_id: "req-int".into(),
                produced_by_request_doc_id: "req-int-doc".into(),
                mode: IntegrateMode::ApplyDiff,
            }),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, integrate_caps()),
        );
        let overlap_refusal = result
            .as_ref()
            .err()
            .is_some_and(|err| err.to_string().contains("overlapping"));
        let head_moved = git(&fx.repo, &["rev-parse", "HEAD"]) != base;
        let trunk_effects = u64::from(
            head_moved
                || result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.pending_head_sha.is_some()),
        );
        let dirty_preserved = match &case.input.dirty_path {
            Some(dirty) => fs::read_to_string(fx.repo.join(dirty)).unwrap() == "operator wip\n",
            None => true,
        };
        case.check(
            gents_lean_vocab::families::workspace_dirty_trunk_integrate::IntegrateOutput {
                refused: result.is_err(),
                trunk_effects,
                overlap_refusal,
                dirty_preserved,
            },
        );
    }
}

#[test]
fn integrate_finalize_repairs_after_update_ref_crash() {
    let mut fx = Fixture::new();
    fx.commit("gone.rs", "delete me\n");
    fx.commit("old.rs", "rename me\n");
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-crash-co", "unit-1", "topic-crash-co");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::remove_file(dest.join("gone.rs")).unwrap();
    fs::rename(dest.join("old.rs"), dest.join("new.rs")).unwrap();
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-crash-co", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-crash-co", "req-int", &hash);

    let first = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-crash-co".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("prepare integrate");
    let commit = first
        .pending_head_sha
        .clone()
        .expect("pending integrate commit");
    git(&fx.repo, &["update-ref", "HEAD", &commit]);
    git(&fx.repo, &["checkout", &commit, "--", "patch.rs"]);
    assert!(
        fx.repo.join("gone.rs").exists(),
        "kill after update-ref must leave the deletion unapplied"
    );
    assert!(
        fx.repo.join("old.rs").exists(),
        "kill mid-checkout must leave the rename source"
    );

    let second = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-crash-co".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("restart after update-ref");
    assert_eq!(
        second.pending_head_sha.as_deref(),
        Some(commit.as_str()),
        "restart must still finalize when HEAD already equals the integrate commit; status={}",
        git(&fx.repo, &["status", "--porcelain=v1"])
    );
    finalize_integrate_trunk(&fx.repo, second.pending_head_sha.as_deref()).unwrap();

    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), commit);
    assert!(
        !fx.repo.join("gone.rs").exists(),
        "restart must remove the deleted file from disk"
    );
    assert!(
        git(&fx.repo, &["ls-files", "--", "gone.rs"]).is_empty(),
        "restart must drop the deleted file from the default index"
    );
    assert!(!fx.repo.join("old.rs").exists());
    assert!(git(&fx.repo, &["ls-files", "--", "old.rs"]).is_empty());
    assert_eq!(
        fs::read_to_string(fx.repo.join("new.rs")).unwrap(),
        "rename me\n"
    );
    assert_eq!(
        fs::read_to_string(fx.repo.join("patch.rs")).unwrap(),
        "fn patch() {}\n"
    );
}

#[test]
fn integrate_journal_marker_sanitizes_workspace_id() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let workspace_id = "ws/../../escape";
    let action = fx.action(workspace_id, "unit-1", "topic-escape");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, workspace_id, "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, workspace_id, "req-int", &hash);
    execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: workspace_id.into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("integrate");
    let git_dir = PathBuf::from(git(&fx.repo, &["rev-parse", "--absolute-git-dir"]));
    let marker = git_dir.join("gents-integrate-ws-------escape.json");
    assert!(
        marker.is_file(),
        "journal marker must stay under .git/: {}",
        marker.display()
    );
    assert!(
        !fx.repo.join("escape.json").exists(),
        "raw workspace_id must not escape .git/"
    );
}

#[test]
fn cleanup_refuses_ready_and_active_bindings() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-ready-c", "unit-1", "topic-ready-c");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    let err = execute_cleanup_workspace_plan(
        &emit_cleanup_workspace_plan(CleanupWorkspaceAction {
            workspace_id: "ws-ready-c".into(),
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, cleanup_caps()),
    )
    .expect_err("Ready cleanup denied");
    assert!(err.to_string().contains("ready"), "{err}");
    assert!(dest.is_dir(), "Ready tree stays on disk");
    assert_eq!(
        docs.load_isolated_workspace("ws-ready-c")
            .unwrap()
            .unwrap()
            .lifecycle_state,
        "ready"
    );

    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-ready-c", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    docs.write_binding(super::binding::new_binding(
        "ws-ready-c",
        "req-review",
        "doc-review",
        gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
        "did:key:zWorkspaceOwner",
        Some(&hash),
    ))
    .unwrap();
    let err = execute_cleanup_workspace_plan(
        &emit_cleanup_workspace_plan(CleanupWorkspaceAction {
            workspace_id: "ws-ready-c".into(),
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, cleanup_caps()),
    )
    .expect_err("Active bindings block cleanup");
    assert!(err.to_string().contains("Active binding"), "{err}");
    assert!(dest.is_dir());
}

#[test]
fn integrate_and_cleanup_plans_omit_host_path() {
    let integrate = emit_integrate_workspace_plan(IntegrateWorkspaceAction {
        workspace_id: "ws-1".into(),
        produced_by_request_id: "req-1".into(),
        produced_by_request_doc_id: "doc-1".into(),
        mode: IntegrateMode::ApplyDiff,
    });
    let json = serde_json::to_value(&integrate).unwrap();
    assert_eq!(json["actions"][0]["type"], "integrate_workspace");
    assert!(json["actions"][0].get("host_path").is_none());
    assert!(serde_json::from_value::<HostAction>(serde_json::json!({
        "type": "integrate_workspace",
        "workspace_id": "ws-1",
        "produced_by_request_id": "req-1",
        "produced_by_request_doc_id": "doc-1",
        "host_path": "/tmp/evil"
    }))
    .is_err());

    let cleanup = emit_cleanup_workspace_plan(CleanupWorkspaceAction {
        workspace_id: "ws-1".into(),
    });
    let json = serde_json::to_value(&cleanup).unwrap();
    assert_eq!(json["actions"][0]["type"], "cleanup_workspace");
    assert!(json["actions"][0].get("host_path").is_none());
    assert!(serde_json::from_value::<HostAction>(serde_json::json!({
        "type": "cleanup_workspace",
        "workspace_id": "ws-1",
        "host_path": "/tmp/evil"
    }))
    .is_err());
}

fn workspace_path_capability(mode: &str, paths: Option<&Vec<String>>) -> WorkspacePathCapability {
    let mut encoded = serde_json::Map::new();
    encoded.insert("mode".into(), mode.into());
    if let Some(paths) = paths {
        encoded.insert("paths".into(), serde_json::Value::from(paths.clone()));
    }
    serde_json::from_value(encoded.into()).unwrap()
}

fn workspace_records(docs: &MemoryWorkspaceDocuments) -> serde_json::Value {
    serde_json::json!({"workspaces": docs.workspaces, "receipts": docs.receipts})
}

fn workspace_disposition<T, E>(result: &Result<T, E>, unchanged: bool) -> String {
    match result {
        Err(_) => "denied",
        Ok(_) if unchanged => "recovered",
        Ok(_) => "accepted",
    }
    .to_owned()
}

fn holds_receipt(docs: &MemoryWorkspaceDocuments, kind: &str) -> bool {
    let held = docs.receipts.values().filter(|r| r.kind == kind).count();
    assert!(held <= 1, "{held} {kind} receipts");
    held == 1
}

fn trunk_effects_since(repo: &Path, before: &str) -> u64 {
    git(repo, &["rev-list", "--count", &format!("{before}...HEAD")])
        .parse()
        .unwrap()
}

#[test]
fn generated_workspace_path_capability_seal_cases_drive_real_git_executor() {
    use gents_lean_vocab::families::workspace_path_capability_contracts_sealing as seal_family;
    for case in
        gents_lean_vocab::families::workspace_path_capability_contracts_sealing::cases::<String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let mut fx = Fixture::new();
        fs::create_dir(fx.repo.join("src")).unwrap();
        fx.commit("src/main.rs", "base\n");
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-cap", "unit-cap", "topic-cap");
        action.path_capability =
            workspace_path_capability(&input.capability.mode, input.capability.paths.as_ref());
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        if input.state == "sealed" {
            fs::write(dest.join("src/main.rs"), "authorized\n").unwrap();
            seal_writer(&fx, &mut docs, "ws-cap", "req-writer");
        }
        match name {
            "owned_tracked_edit_seals" | "empty_exact_rejects_changes" => {
                fs::write(dest.join("src/main.rs"), "owned change\n").unwrap();
            }
            "owned_untracked_addition_seals" => {
                fs::write(dest.join("src/new.rs"), "new file\n").unwrap();
            }
            "empty_exact_empty_delta_seals" => {}
            "unowned_build_log_rejected" => {
                fs::create_dir(dest.join(".tmp-build")).unwrap();
                fs::write(dest.join(".tmp-build/test-build.log"), "build noise\n").unwrap();
            }
            "rename_unowned_destination_rejected" => {
                fs::rename(dest.join("src/main.rs"), dest.join("outside.rs")).unwrap();
            }
            "rename_both_owned_accepted" => {
                fs::rename(dest.join("src/main.rs"), dest.join("src/new.rs")).unwrap();
            }
            "changed_symlink_rejected" => {
                fs::remove_file(dest.join("src/main.rs")).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink("../README.md", dest.join("src/main.rs")).unwrap();
                #[cfg(not(unix))]
                panic!("symlink conformance requires a supported filesystem fixture");
            }
            "changed_gitlink_rejected" => {
                fs::remove_file(dest.join("src/main.rs")).unwrap();
                fs::create_dir(dest.join("src/main.rs")).unwrap();
                init_test_repo(&dest.join("src/main.rs"), &["-b", "main"]);
                fs::write(dest.join("src/main.rs/nested"), "nested\n").unwrap();
                git(&dest.join("src/main.rs"), &["add", "nested"]);
                git(&dest.join("src/main.rs"), &["commit", "-m", "nested"]);
            }
            "mutable_head_cannot_replace_immutable_base" => {
                fs::write(dest.join("outside.rs"), "hidden in worker commit\n").unwrap();
                git(&dest, &["add", "outside.rs"]);
                git(&dest, &["commit", "-m", "worker advances HEAD"]);
                assert_ne!(git(&dest, &["rev-parse", "HEAD"]), fx.base_sha);
            }
            "sealed_repair_rechecks_actual_delta" => {
                fs::write(dest.join("outside.rs"), "unauthorized repair\n").unwrap();
            }
            other => panic!("unmapped generated seal case {other}"),
        }
        let before = workspace_records(&docs);
        let trunk = git(&fx.repo, &["rev-parse", "HEAD"]);
        let result = execute_seal_workspace_plan(
            &emit_seal_workspace_plan(SealWorkspaceAction {
                workspace_id: "ws-cap".into(),
                produced_by_request_id: "req-writer".into(),
                produced_by_request_doc_id: "req-writer-doc".into(),
            }),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, seal_caps()),
        );
        let unchanged = workspace_records(&docs) == before;
        assert!(
            result.is_ok() || unchanged,
            "{name}: denial changed the workspace or its receipts"
        );
        case.check(seal_family::Observation {
            state: docs.workspaces["ws-cap"].lifecycle_state.clone(),
            disposition: workspace_disposition(&result, unchanged),
            writer_receipt: holds_receipt(&docs, "writer"),
            integrator_receipt: holds_receipt(&docs, "integrator"),
            trunk_effects: trunk_effects_since(&fx.repo, &trunk),
        });
    }
}

#[test]
fn workspace_capability_admission_rejects_missing_and_invalid_manifest() {
    let fx = Fixture::new();
    let valid = fx.action("ws-path-admission", "unit", "topic");
    let mut encoded = serde_json::to_value(&valid).unwrap();
    encoded.as_object_mut().unwrap().remove("path_capability");
    assert!(serde_json::from_value::<CreateWorkspaceAction>(encoded).is_err());
    for path in [
        "../outside",
        "/tmp/outside",
        ".git/config",
        "src\\file",
        "src/*.rs",
        "src/file\n",
    ] {
        let mut action = valid.clone();
        action.path_capability = WorkspacePathCapability::ExactPaths {
            paths: vec![path.into()],
        };
        let mut docs = MemoryWorkspaceDocuments::default();
        assert!(
            execute_create_workspace_plan(
                &emit_create_workspace_plan(action),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, git_worktree_caps())
            )
            .is_err(),
            "accepted {path:?}"
        );
        assert!(docs.workspaces.is_empty());
        assert!(docs.receipts.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn exact_owned_executable_bit_change_seals_and_integrates() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(fx.action("ws-mode", "unit", "topic-mode")),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    let dest = PathBuf::from(&created.placement.host_path);
    fs::set_permissions(dest.join("README.md"), fs::Permissions::from_mode(0o755)).unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-mode", "req-writer");
    bind_integrate(
        &mut docs,
        "ws-mode",
        "req-int",
        sealed.workspace.seal_hash.as_deref().unwrap(),
    );
    integrate_writer(&fx, &mut docs, "ws-mode", "req-int");
    assert_eq!(
        git(&fx.repo, &["ls-tree", "HEAD", "README.md"])
            .split_whitespace()
            .next(),
        Some("100755")
    );
}

#[test]
fn generated_workspace_receipt_cases_recover_without_checkout_or_reapplying() {
    use gents_lean_vocab::families::workspace_path_capability_contracts_replay as replay_family;
    for case in
        gents_lean_vocab::families::workspace_path_capability_contracts_replay::cases::<String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(fx.action("ws-replay-cap", "unit", "topic-replay-cap")),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        fs::write(dest.join("patch.rs"), "owned\n").unwrap();
        let sealed = seal_writer(&fx, &mut docs, "ws-replay-cap", "req-writer");
        if input.operation == "replay_integrate" {
            bind_integrate(
                &mut docs,
                "ws-replay-cap",
                "req-int",
                sealed.workspace.seal_hash.as_deref().unwrap(),
            );
            integrate_writer(&fx, &mut docs, "ws-replay-cap", "req-int");
        }
        if name == "changed_capability_cannot_replay_receipt" {
            docs.workspaces
                .get_mut("ws-replay-cap")
                .unwrap()
                .path_capability = WorkspacePathCapability::UnrestrictedCompatibility;
        }
        fs::remove_dir_all(&dest).unwrap();
        let before = workspace_records(&docs);
        let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
        let mut journal = vec![ActionJournalEntry::new(
            0,
            ActionJournalState::ResultDocsWritten,
        )];
        let result = if input.operation == "replay_seal" {
            execute_seal_workspace_plan(
                &emit_seal_workspace_plan(SealWorkspaceAction {
                    workspace_id: "ws-replay-cap".into(),
                    produced_by_request_id: "req-writer".into(),
                    produced_by_request_doc_id: "req-writer-doc".into(),
                }),
                &mut journal,
                &mut fx.ctx(&mut docs, seal_caps()),
            )
            .map(|_| ())
        } else {
            execute_integrate_workspace_plan(
                &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
                    workspace_id: "ws-replay-cap".into(),
                    produced_by_request_id: "req-int".into(),
                    produced_by_request_doc_id: "req-int-doc".into(),
                    mode: IntegrateMode::ApplyDiff,
                }),
                &mut journal,
                &mut fx.ctx(&mut docs, integrate_caps()),
            )
            .map(|outcome| {
                assert!(
                    outcome.pending_head_sha.is_none(),
                    "{name}: completed receipt must not request another trunk effect"
                );
            })
        };
        assert!(
            !dest.exists(),
            "receipt replay must not recreate a checkout"
        );
        let unchanged = workspace_records(&docs) == before;
        assert!(
            result.is_ok() || unchanged,
            "{name}: denial changed the workspace or its receipts"
        );
        case.check(replay_family::Observation {
            state: docs.workspaces["ws-replay-cap"].lifecycle_state.clone(),
            disposition: workspace_disposition(&result, unchanged),
            writer_receipt: holds_receipt(&docs, "writer"),
            integrator_receipt: holds_receipt(&docs, "integrator"),
            trunk_effects: trunk_effects_since(&fx.repo, &before_head),
        });
    }
}

#[test]
fn workspace_create_retry_cannot_change_admitted_capability() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("ws-immutable-cap", "unit", "topic-immutable-cap");
    let original = execute_create_workspace_plan(
        &emit_create_workspace_plan(action.clone()),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    action.path_capability =
        WorkspacePathCapability::exact_paths(vec!["outside.rs".into()]).unwrap();
    assert!(execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps())
    )
    .is_err());
    assert_eq!(docs.workspaces["ws-immutable-cap"], original.workspace);
    assert!(docs.receipts.is_empty());
}

#[test]
fn generated_workspace_integration_cases_apply_only_authorized_snapshot() {
    use gents_lean_vocab::families::workspace_path_capability_contracts_integrate as integrate_family;
    for case in
        gents_lean_vocab::families::workspace_path_capability_contracts_integrate::cases::<String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let mut fx = Fixture::new();
        fs::create_dir(fx.repo.join("src")).unwrap();
        fx.commit("src/main.rs", "base\n");
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-cap-integrate", "unit", "topic-cap-integrate");
        action.path_capability =
            workspace_path_capability(&input.capability.mode, input.capability.paths.as_ref());
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        fs::write(dest.join("src/main.rs"), "authorized\n").unwrap();
        let sealed = seal_writer(&fx, &mut docs, "ws-cap-integrate", "req-writer");
        let seal = sealed.workspace.seal_hash.as_deref().unwrap();
        bind_integrate(
            &mut docs,
            "ws-cap-integrate",
            "req-int",
            if name == "different_seal_cannot_integrate" {
                "different-seal"
            } else {
                seal
            },
        );
        match name {
            "authorized_integration_once" | "different_seal_cannot_integrate" => {}
            "integration_rejects_unowned_delta" => {
                fs::write(dest.join("outside.rs"), "unowned\n").unwrap();
            }
            "snapshot_drift_cannot_apply_other_bytes" => {
                fs::write(dest.join("src/main.rs"), "different snapshot\n").unwrap();
            }
            other => panic!("unmapped integration case {other}"),
        }
        let before_workspace = docs.workspaces["ws-cap-integrate"].clone();
        let before = workspace_records(&docs);
        let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
        let result = execute_integrate_workspace_plan(
            &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
                workspace_id: "ws-cap-integrate".into(),
                produced_by_request_id: "req-int".into(),
                produced_by_request_doc_id: "req-int-doc".into(),
                mode: IntegrateMode::ApplyDiff,
            }),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, integrate_caps()),
        );
        let unchanged = workspace_records(&docs) == before;
        if let Ok(integrated) = &result {
            finalize_integrate_trunk(&fx.repo, integrated.pending_head_sha.as_deref()).unwrap();
            assert_eq!(
                fs::read_to_string(fx.repo.join("src/main.rs")).unwrap(),
                "authorized\n"
            );
        } else {
            assert!(
                unchanged,
                "{name}: denial changed the workspace or its receipts"
            );
            assert_eq!(
                fs::read_to_string(fx.repo.join("src/main.rs")).unwrap(),
                "base\n"
            );
            assert!(!fx.repo.join("outside.rs").exists());
        }
        assert_eq!(
            docs.workspaces["ws-cap-integrate"], before_workspace,
            "{name}: immutable seal changed"
        );
        case.check(integrate_family::Observation {
            state: docs.workspaces["ws-cap-integrate"].lifecycle_state.clone(),
            disposition: workspace_disposition(&result, unchanged),
            writer_receipt: holds_receipt(&docs, "writer"),
            integrator_receipt: holds_receipt(&docs, "integrator"),
            trunk_effects: trunk_effects_since(&fx.repo, &before_head),
        });
    }
}

#[test]
fn generated_workspace_provision_cases_admit_only_exact_canonical_capability() {
    use gents_lean_vocab::families::workspace_path_capability_contracts_provision as provision_family;
    for case in
        gents_lean_vocab::families::workspace_path_capability_contracts_provision::cases::<String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-provision-cap", "unit", "topic-provision-cap");
        let mut checkout = None;
        if input.existing_identity_matches {
            let created = execute_create_workspace_plan(
                &emit_create_workspace_plan(action.clone()),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, git_worktree_caps()),
            )
            .unwrap();
            let dest = PathBuf::from(&created.placement.host_path);
            docs.workspaces
                .get_mut("ws-provision-cap")
                .unwrap()
                .path_capability = WorkspacePathCapability::UnrestrictedCompatibility;
            let git_dir = PathBuf::from(git(&dest, &["rev-parse", "--absolute-git-dir"]));
            let marker = git_dir.join("gents-workspace-identity.json");
            let mut old: serde_json::Value =
                serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
            old.as_object_mut().unwrap().remove("path_capability");
            fs::write(&marker, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
            if !input.checkout_present {
                fs::remove_dir_all(&dest).unwrap();
            }
            checkout = Some(dest);
        }
        action.path_capability =
            workspace_path_capability(&input.capability.mode, input.capability.paths.as_ref());
        if !input.manifest_canonical {
            action.path_capability = WorkspacePathCapability::ExactPaths {
                paths: vec!["../escape".into()],
            };
        }
        let before = workspace_records(&docs);
        let result = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        );
        let unchanged = workspace_records(&docs) == before;
        if let Some(dest) = &checkout {
            assert_eq!(
                dest.exists(),
                input.checkout_present,
                "{name}: legacy recovery reprovisioned a missing checkout"
            );
        }
        match &result {
            Ok(created) if !unchanged => {
                assert_eq!(created.workspace.lifecycle_state, "ready");
                assert!(created.workspace.path_capability.is_exact());
            }
            Ok(_) => {}
            Err(_) => assert!(unchanged, "{name}: denied creation changed the workspace"),
        }
        assert!(docs.receipts.is_empty());
        case.check(provision_family::Outcome {
            disposition: workspace_disposition(&result, unchanged),
        });
    }
}

#[test]
fn generated_noncanonical_git_delta_is_rejected_without_filesystem_traversal() {
    use gents_lean_vocab::families::workspace_path_capability_contracts_tree_delta as tree_delta_family;
    use std::io::Write;
    use std::process::Stdio;
    for case in gents_lean_vocab::families::workspace_path_capability_contracts_tree_delta::cases::<
        String,
    >() {
        let fx = Fixture::new();
        let cap = workspace_path_capability(
            &case.input.capability.mode,
            case.input.capability.paths.as_ref(),
        );
        let blob = git(&fx.repo, &["rev-parse", "HEAD:README.md"]);
        let mut tree = b"100644 ../outside\0".to_vec();
        for byte in blob.as_bytes().as_chunks::<2>().0 {
            tree.push(u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap());
        }
        let mut child = crate::exec::native_spawn::spawn_std(
            std::process::Command::new("git")
                .args(["hash-object", "--literally", "-t", "tree", "-w", "--stdin"])
                .current_dir(&fx.repo)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        child.stdin.take().unwrap().write_all(&tree).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Git fixture rejected raw object: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree_id = String::from_utf8(output.stdout).unwrap();
        let before = git(&fx.repo, &["rev-parse", "HEAD"]);
        let validated =
            super::adapter::validate_tree_delta(&fx.repo, &fx.base_sha, tree_id.trim(), &cap);
        assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before);
        assert!(!fx.parent().join("outside").exists());
        case.check(tree_delta_family::Outcome {
            disposition: workspace_disposition(&validated, false),
        });
    }
}

#[path = "tests/capability_regressions.rs"]
mod capability_regressions;

#[path = "tests/capability_runtime.rs"]
mod capability_runtime;

#[path = "tests/path_alias_contract.rs"]
mod path_alias_contract;

#[path = "tests/quickstart_base_freeze.rs"]
mod quickstart_base_freeze;

mod operator_base_freeze;

#[path = "tests/integrate_writer_receipt.rs"]
mod integrate_writer_receipt;

#[test]
fn binding_admission_follows_the_lean_workspace_binding_model() {
    use gents_lean_vocab::families::contracts_workspace_binding_admission::BindingWire;
    fn doc(witness: &BindingWire) -> super::WorkspaceBindingDoc {
        super::WorkspaceBindingDoc {
            workspace_doc_id: None,
            binding_id: witness.binding_id.clone(),
            workspace_id: witness.workspace_id.clone(),
            request_id: witness.request_id.clone(),
            request_doc_id: format!("doc-{}", witness.request_id),
            authority: witness.authority.clone(),
            owner_node_did: witness.node_did.clone(),
            seal_hash: witness.seal_hash.clone(),
            lifecycle_state: witness.state.clone(),
        }
    }
    gents_lean_vocab::families::contracts_workspace_binding_admission::replay::<String>(|i| {
        let existing: Vec<_> = i.existing.iter().map(doc).collect();
        super::binding::admit_workspace_binding(
            &i.workspace_id,
            &i.workspace_state,
            i.workspace_seal_hash.as_deref(),
            &existing,
            doc(&i.candidate),
            false,
        )
        .is_ok()
    });
}
