use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::toolset::{
    validate_command_policy, CommandConstraints, CommandExecutionMode, CommandExecutionPolicy,
    CommandNetworkMode,
};

use super::adapter::{bound_dirty_base_summary, DIRTY_BASE_SUMMARY_LIMIT};
use super::*;

fn capabilities() -> BTreeSet<String> {
    [
        CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
        CAP_CLONE_ARTIFACTS,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn git_worktree_caps() -> BTreeSet<String> {
    [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn seal_caps() -> BTreeSet<String> {
    [CAP_SEAL_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn integrate_caps() -> BTreeSet<String> {
    [CAP_INTEGRATE_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn cleanup_caps() -> BTreeSet<String> {
    [CAP_CLEANUP_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
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
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    base_sha: String,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "ws@example.com"]);
        git(&repo, &["config", "user.name", "Workspace Test"]);
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

    fn parent(&self) -> &Path {
        self.repo.parent().expect("repo parent")
    }

    fn action(
        &self,
        workspace_id: &str,
        work_unit_id: &str,
        branch: &str,
    ) -> CreateWorkspaceAction {
        CreateWorkspaceAction {
            path_capability: WorkspacePathCapability::exact_paths(
                [
                    "README.md",
                    "patch.rs",
                    "after-seal.txt",
                    "AGENTS.md",
                    "blob.bin",
                    "gone.rs",
                    "old.rs",
                    "new.rs",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            )
            .unwrap(),
            workspace_id: workspace_id.to_string(),
            work_unit_id: work_unit_id.to_string(),
            repository_id: "repo-1".to_string(),
            base_sha: self.base_sha.clone(),
            branch: branch.to_string(),
            creation_policy: CreationPolicy::GitWorktreeDiff,
            adapter: WorkspaceAdapterKind::GitWorktree,
            clone_artifacts: None,
        }
    }

    fn commit(&mut self, rel: &str, content: &str) {
        fs::write(self.repo.join(rel), content).unwrap();
        git(&self.repo, &["add", rel]);
        git(&self.repo, &["commit", "-m", rel]);
        self.base_sha = git(&self.repo, &["rev-parse", "HEAD"]);
    }

    fn ctx<'a>(
        &'a self,
        docs: &'a mut MemoryWorkspaceDocuments,
        caps: BTreeSet<String>,
    ) -> HostExecutorContext<'a> {
        HostExecutorContext {
            deployment_id: "deploy-1".to_string(),
            repository: RepositoryPlacementRef {
                repository_id: "repo-1".to_string(),
                deployment_id: "deploy-1".to_string(),
                host_path: self.repo.clone(),
                enabled: true,
            },
            ceiling: Some(&self.repo),
            capabilities: caps,
            writer_principal: "did:key:zWriter".to_string(),
            integrator_principal: "did:key:zIntegrator".to_string(),
            caused_by_invocation_id: "inv-1".to_string(),
            caused_by_correlation: "corr-1".to_string(),
            documents: docs,
        }
    }
}

fn git_worktree_diff_policy() -> CommandExecutionPolicy {
    CommandExecutionPolicy::write_capable()
        .with_mode(CommandExecutionMode::WorkspaceWrite)
        .with_git_worktree_diff()
}

#[test]
fn builtin_emitter_omits_absolute_destination() {
    let action = CreateWorkspaceAction {
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
        owner_deployment_id: "deploy-1".into(),
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
    };
    let mutation = isolated_workspace_upsert_mutation(&doc);
    assert!(!mutation.contains("host_path"));
    assert!(mutation.contains("upsert_IsolatedWorkspace"));
    assert!(!mutation.contains("create_IsolatedWorkspace"));
    assert!(mutation.contains("seal_hash: null"));
    assert!(mutation.contains("instruction_manifest:"));
    let placement = WorkspacePlacementDoc {
        workspace_id: "ws-1".into(),
        deployment_id: "deploy-1".into(),
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
    assert!(!placement_mutation.contains("[]"));
    let repository_mutation = repository_placement_upsert_mutation(
        &RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            deployment_id: "deploy-1".into(),
            host_path: PathBuf::from("/tmp/repo\"quoted"),
            enabled: true,
        },
        "2026-08-21T00:00:00Z",
    )
    .unwrap();
    assert!(repository_mutation.contains("upsert_RepositoryPlacement"));
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
        checks_run: None,
        unresolved_conflicts: None,
        integration_instructions: None,
    };
    let receipt_mutation = workspace_receipt_create_mutation(&receipt);
    assert!(receipt_mutation.contains("upsert_WorkspaceReceipt"));
    assert!(receipt_mutation.contains("changed_files: null"));
    assert!(!receipt_mutation.contains("[]"));
}

#[test]
fn create_workspace_is_idempotent_when_identity_matches() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-match", "unit-1", "topic-match");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let first = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("first provision");
    assert_eq!(first.workspace.lifecycle_state, "ready");
    let observation: ProvisioningObservation =
        serde_json::from_str(&first.placement.provisioning_state).unwrap();
    assert!(observation.path_exists);
    assert!(observation.worktree_registered);
    assert!(Path::new(&first.placement.host_path)
        .join("README.md")
        .is_file());
    assert_eq!(
        journal.last().map(|entry| entry.state),
        Some(ActionJournalState::ResultDocsWritten)
    );

    let mut journal2 = Vec::new();
    let second = execute_create_workspace_plan(
        &plan,
        &mut journal2,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("idempotent provision");
    assert_eq!(second.workspace.lifecycle_state, "ready");
    assert_eq!(second.placement.host_path, first.placement.host_path);
    assert_eq!(
        docs.workspaces.len(),
        1,
        "idempotent retry must not mint a second IsolatedWorkspace"
    );
}

#[test]
fn existing_target_mismatch_does_not_overwrite_or_cleanup() {
    let fx = Fixture::new();
    let dest = fx.parent().join("foreign");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep-me.txt"), "untouched\n").unwrap();

    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("foreign", "unit-1", "topic-foreign");
    // Force dest to be the pre-existing foreign directory.
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
    let mut journal = Vec::new();
    let err = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect_err("mismatch must fail");
    assert!(err.identity_mismatch(), "{err}");
    assert_eq!(
        err.outcome()
            .map(|outcome| outcome.workspace.lifecycle_state.as_str()),
        Some("provisionFailed")
    );
    assert_eq!(
        fs::read_to_string(planned.join("keep-me.txt")).unwrap(),
        "untouched\n"
    );
    assert!(
        !planned.join("README.md").exists(),
        "mismatch must not check out the repo over the leftover"
    );
    let stored = docs
        .load_isolated_workspace("foreign")
        .unwrap()
        .expect("ProvisionFailed row");
    assert_eq!(stored.lifecycle_state, "provisionFailed");
    assert!(!stored.dirty_base_on_replicated_row());
}

#[test]
fn matching_ready_workspace_is_not_overwritten_by_identity_mismatch_retry() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let original = fx.action("ws-stable", "unit-1", "topic-stable");
    let plan = emit_create_workspace_plan(original.clone());
    let mut journal = Vec::new();
    let first = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&first.placement.host_path);
    let readme = fs::read_to_string(dest.join("README.md")).unwrap();

    let mut mismatched = original;
    mismatched.work_unit_id = "unit-OTHER".into();
    let plan = emit_create_workspace_plan(mismatched);
    let mut journal = Vec::new();
    let err = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect_err("identity mismatch");
    assert!(err.identity_mismatch(), "{err}");
    assert_eq!(fs::read_to_string(dest.join("README.md")).unwrap(), readme);
    let stored = docs.load_isolated_workspace("ws-stable").unwrap().unwrap();
    assert_eq!(stored.lifecycle_state, "ready");
    assert_eq!(stored.work_unit_id, "unit-1");
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
fn destination_escaping_ceiling_is_denied() {
    let fx = Fixture::new();
    let other = fx.parent().join("other-ceiling");
    fs::create_dir_all(&other).unwrap();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-escape", "unit-1", "topic-escape");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let mut ctx = fx.ctx(&mut docs, git_worktree_caps());
    ctx.ceiling = Some(&other);
    let err =
        execute_create_workspace_plan(&plan, &mut journal, &mut ctx).expect_err("ceiling escape");
    assert!(matches!(err, HostExecuteError::Denied { .. }), "{err}");
    assert!(docs.workspaces.is_empty());
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
fn git_worktree_diff_denies_metadata_writes_allows_reads() {
    let policy = git_worktree_diff_policy();
    for sub in [
        "add",
        "commit",
        "merge",
        "rebase",
        "push",
        "update-ref",
        "symbolic-ref",
    ] {
        let err = validate_command_policy("git", &[sub.to_string()], &policy).unwrap_err();
        let payload = err.to_string();
        assert!(
            payload.contains("gitMetadataWriteDenied") || payload.contains("git_worktree_diff"),
            "expected git metadata denial for {sub}, got {payload}"
        );
    }
    for (command, args) in [
        ("git", vec!["status"]),
        ("git", vec!["diff"]),
        ("git", vec!["log"]),
        ("git", vec!["rev-parse", "HEAD"]),
    ] {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        validate_command_policy(command, &argv, &policy)
            .unwrap_or_else(|err| panic!("{command} {argv:?} should be allowed: {err}"));
    }

    let err = validate_command_policy(
        "/bin/sh",
        &["-lc".into(), "git commit -am 'x'".into()],
        &policy,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("gitMetadataWriteDenied")
            || err.to_string().contains("git_worktree_diff")
            || err.to_string().contains("commit"),
        "{err}"
    );

    let unrestricted =
        CommandExecutionPolicy::write_capable().with_mode(CommandExecutionMode::WorkspaceWrite);
    validate_command_policy("git", &[String::from("commit")], &unrestricted)
        .expect("without git_worktree_diff, WorkspaceWrite still allows git commit at argv layer");

    for script in [
        "git --exec-path=/tmp/evil status",
        "git --git-dir=/tmp/evil status",
        "git --work-tree=/tmp/evil diff",
    ] {
        let err = validate_command_policy("/bin/sh", &["-lc".into(), script.into()], &policy)
            .unwrap_err();
        assert!(
            err.to_string().contains("gitMetadataWriteDenied")
                || err.to_string().contains("git_worktree_diff")
                || err.to_string().contains("exec-path")
                || err.to_string().contains("git-dir")
                || err.to_string().contains("work-tree"),
            "script {script:?} should be denied, got {err}"
        );
    }

    let constraints = CommandConstraints {
        allowed_argv_prefixes: Vec::new(),
        forbidden_argv_prefixes: Vec::new(),
        network_mode: CommandNetworkMode::Inherit,
        execution_mode: CommandExecutionMode::WorkspaceWrite,
        sandbox: CommandExecutionMode::WorkspaceWrite,
        deny_all_argv: false,
        deny_git_metadata_writes: true,
    };
    assert!(constraints.to_spawn_policy().deny_git_metadata_writes());
}

#[test]
fn dirty_base_summary_truncates_on_char_boundary() {
    let cjk = "文".repeat(700);
    assert!(cjk.len() > DIRTY_BASE_SUMMARY_LIMIT);
    assert!(!cjk.is_char_boundary(DIRTY_BASE_SUMMARY_LIMIT));
    let summary = bound_dirty_base_summary(&cjk);
    assert!(summary.len() <= DIRTY_BASE_SUMMARY_LIMIT);
    assert!(cjk.is_char_boundary(summary.len()));
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

fn seal_writer(
    fx: &Fixture,
    docs: &mut MemoryWorkspaceDocuments,
    workspace_id: &str,
    request_id: &str,
) -> SealWorkspaceOutcome {
    let plan = emit_seal_workspace_plan(SealWorkspaceAction {
        workspace_id: workspace_id.to_string(),
        produced_by_request_id: request_id.to_string(),
        produced_by_request_doc_id: format!("{request_id}-doc"),
    });
    let mut journal = Vec::new();
    execute_seal_workspace_plan(&plan, &mut journal, &mut fx.ctx(docs, seal_caps())).expect("seal")
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
        crate::toolset::WorkspaceAuthority::ReadWrite,
        "deploy-1",
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
            crate::toolset::WorkspaceAuthority::ReadWrite,
            "deploy-1",
            sealed.workspace.seal_hash.as_deref(),
        ),
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not bindable"), "{err:#}");
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
            crate::toolset::WorkspaceAuthority::ReadOnly,
            "deploy-1",
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
            crate::toolset::WorkspaceAuthority::ReadOnly,
            "deploy-1",
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

#[test]
fn seal_drift_fails_closed() {
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
    let live = super::adapter::working_tree_hash(&dest).unwrap();
    assert_ne!(live, hash);

    let mut workspace = super::IsolatedWorkspaceRecord {
        workspace_id: "ws-drift".into(),
        owner_deployment_id: "deploy-1".into(),
        writer_principal: "did:key:zWriter".into(),
        integrator_principal: "did:key:zIntegrator".into(),
        lifecycle_state: "sealed".into(),
        seal_hash: Some(hash.clone()),
        instruction_manifest: sealed.workspace.instruction_manifest.clone(),
    };
    let mut placed = super::WorkspacePlacementRecord {
        workspace_id: "ws-drift".into(),
        deployment_id: "deploy-1".into(),
        host_path: dest.to_string_lossy().into_owned(),
        observed_tree_hash: Some(hash.clone()),
    };
    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            workspace_id: "ws-drift",
            authority: crate::toolset::WorkspaceAuthority::ReadOnly,
            owner_deployment_id: "deploy-1",
            seal_hash: Some(&hash),
            request_cwd: None,
            local_deployment_id: "deploy-1",
            operator_tool_root: Some(fx.parent()),
            enabled_workspace_roots: &[],
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
            authority: crate::toolset::WorkspaceAuthority::ReadOnly,
            owner_deployment_id: "deploy-1",
            seal_hash: Some(&hash),
            request_cwd: None,
            local_deployment_id: "deploy-1",
            operator_tool_root: Some(fx.parent()),
            enabled_workspace_roots: &[],
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

fn bind_integrate(
    docs: &mut MemoryWorkspaceDocuments,
    workspace_id: &str,
    request_id: &str,
    seal_hash: &str,
) {
    docs.write_binding(super::binding::new_binding(
        workspace_id,
        request_id,
        &format!("{request_id}-doc"),
        crate::toolset::WorkspaceAuthority::Integrate,
        "deploy-1",
        Some(seal_hash),
    ))
    .unwrap();
}

fn integrate_writer(
    fx: &Fixture,
    docs: &mut MemoryWorkspaceDocuments,
    workspace_id: &str,
    request_id: &str,
) -> IntegrateWorkspaceOutcome {
    let plan = emit_integrate_workspace_plan(IntegrateWorkspaceAction {
        workspace_id: workspace_id.to_string(),
        produced_by_request_id: request_id.to_string(),
        produced_by_request_doc_id: format!("{request_id}-doc"),
        mode: IntegrateMode::ApplyDiff,
    });
    let mut journal = Vec::new();
    let outcome =
        execute_integrate_workspace_plan(&plan, &mut journal, &mut fx.ctx(docs, integrate_caps()))
            .expect("integrate");
    finalize_integrate_trunk(&fx.repo, outcome.pending_head_sha.as_deref())
        .expect("finalize trunk");
    if let Some(binding) = docs
        .load_bindings(workspace_id)
        .unwrap()
        .into_iter()
        .find(|binding| binding.is_active_integrate() && binding.request_id == request_id)
    {
        docs.write_binding(super::binding::release_binding(binding))
            .unwrap();
    }
    super::executor::clear_integrate_journal(&fx.repo, workspace_id);
    outcome
}

#[test]
fn integrate_does_not_rewind_trunk_after_later_operator_commit() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-rewind", "unit-1", "topic-rewind");
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-rewind", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-rewind", "req-int", &hash);
    integrate_writer(&fx, &mut docs, "ws-rewind", "req-int");

    fs::write(fx.repo.join("later.txt"), "operator\n").unwrap();
    git(&fx.repo, &["add", "later.txt"]);
    git(&fx.repo, &["commit", "-m", "later"]);
    let operator = git(&fx.repo, &["rev-parse", "HEAD"]);

    bind_integrate(&mut docs, "ws-rewind", "req-int-2", &hash);
    let second = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-rewind".into(),
            produced_by_request_id: "req-int-2".into(),
            produced_by_request_doc_id: "req-int-2-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect("second integrate of already-landed seal");
    finalize_integrate_trunk(&fx.repo, second.pending_head_sha.as_deref()).unwrap();

    assert_eq!(
        git(&fx.repo, &["rev-parse", "HEAD"]),
        operator,
        "already-integrated seal must not rewind trunk past later operator commits"
    );
    assert_eq!(
        fs::read_to_string(fx.repo.join("later.txt")).unwrap(),
        "operator\n"
    );
    assert_eq!(
        fs::read_to_string(fx.repo.join("patch.rs")).unwrap(),
        "fn patch() {}\n"
    );
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
fn integrate_applies_sealed_diff_to_trunk_not_via_worker_git_merge() {
    let policy = git_worktree_diff_policy();
    let merge_err = validate_command_policy("git", &["merge".into()], &policy).unwrap_err();
    assert!(
        merge_err.to_string().contains("gitMetadataWriteDenied")
            || merge_err.to_string().contains("git_worktree_diff"),
        "{merge_err}"
    );

    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = fx.action("ws-int", "unit-1", "topic-int");
    let plan = emit_create_workspace_plan(action);
    let mut journal = Vec::new();
    let created = execute_create_workspace_plan(
        &plan,
        &mut journal,
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .expect("provision");
    let dest = PathBuf::from(&created.placement.host_path);
    assert_ne!(
        dest, fx.repo,
        "worker root must sit outside the source checkout"
    );
    fs::write(dest.join("patch.rs"), "fn patch() {}\n").unwrap();
    assert!(
        !fx.repo.join("patch.rs").exists(),
        "writer edits stay off trunk until integrate"
    );

    let sealed = seal_writer(&fx, &mut docs, "ws-int", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-int", "req-int", &hash);

    let first = super::binding::admit_workspace_binding(
        "ws-int",
        "sealed",
        Some(&hash),
        &docs.load_bindings("ws-int").unwrap(),
        super::binding::new_binding(
            "ws-int",
            "req-review-a",
            "doc-a",
            crate::toolset::WorkspaceAuthority::ReadOnly,
            "deploy-1",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let super::binding::AdmitBinding::Create { binding: a, .. } = first else {
        panic!("expected readonly create");
    };
    docs.write_binding(a.clone()).unwrap();
    let second = super::binding::admit_workspace_binding(
        "ws-int",
        "sealed",
        Some(&hash),
        &docs.load_bindings("ws-int").unwrap(),
        super::binding::new_binding(
            "ws-int",
            "req-review-b",
            "doc-b",
            crate::toolset::WorkspaceAuthority::ReadOnly,
            "deploy-1",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let super::binding::AdmitBinding::Create { binding: b, .. } = second else {
        panic!("expected concurrent readonly");
    };
    docs.write_binding(b.clone()).unwrap();
    assert!(a.is_active());
    assert!(b.is_active());

    let integrated = integrate_writer(&fx, &mut docs, "ws-int", "req-int");
    let mutation = super::documents::workspace_integrate_docs_mutation(&[], &integrated.receipt);
    assert!(mutation.contains("kind: \"integrator\""));
    assert!(mutation.contains("upsert_WorkspaceReceipt"));
    assert_eq!(integrated.receipt.kind, "integrator");
    assert_eq!(integrated.receipt.produced_by_request_id, "req-int");
    assert_eq!(integrated.receipt.seal_hash, hash);
    assert!(integrated.receipt.head_sha.is_some());
    assert_eq!(integrated.workspace.lifecycle_state, "sealed");
    assert_eq!(
        fs::read_to_string(fx.repo.join("patch.rs")).unwrap(),
        "fn patch() {}\n"
    );
    assert!(
        dest.join("patch.rs").is_file(),
        "worker tree stays inspectable after integrate"
    );
    let log = git(&fx.repo, &["log", "-1", "--format=%s"]);
    assert!(
        log.contains("gents: integrate workspace"),
        "host commit on trunk, not worker git merge: {log}"
    );
    assert!(
        !log.to_ascii_lowercase().contains("merge"),
        "integrate must not be a merge commit: {log}"
    );
    let merge_heads = git(&fx.repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        merge_heads.split_whitespace().count(),
        1,
        "trunk head must have a single parent (apply+commit), not a merge: {merge_heads}"
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

    docs.receipts.clear();
    // Commit objects include second-resolution timestamps. Cross a timestamp
    // boundary so regenerating the commit cannot accidentally look like
    // successful recovery of the persisted pending SHA.
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
fn integrate_overlapping_dirty_trunk_path_fails_closed() {
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
    fs::write(dest.join("README.md"), "writer\n").unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-dirty", "req-writer");
    let hash = sealed.workspace.seal_hash.clone().unwrap();
    bind_integrate(&mut docs, "ws-dirty", "req-int", &hash);
    fs::write(fx.repo.join("README.md"), "operator wip\n").unwrap();
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);
    let err = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-dirty".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect_err("overlapping dirty trunk path must fail closed");
    assert!(err.to_string().contains("overlapping"), "{err}");
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        fs::read_to_string(fx.repo.join("README.md")).unwrap(),
        "operator wip\n"
    );
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
        crate::toolset::WorkspaceAuthority::ReadOnly,
        "deploy-1",
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

/// Actual Git/executor consumers for the emitted seal subset. Other operation
/// cases retain follow-up coverage until their recovery fixtures are wired.
fn generated_workspace_path_capability_seal_cases_drive_real_git_executor() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["workspace_path_capability_cases"]
        .as_array()
        .unwrap();
    let mut exercised = BTreeSet::new();
    for case in cases.iter().filter(|case| case["operation"] == "seal") {
        let name = case["name"].as_str().unwrap();
        // Path parser rejection is exercised separately against real admission;
        // it must not be represented by a fabricated Git path outside the root.
        if name == "noncanonical_delta_rejected" {
            continue;
        }
        exercised.insert(name.to_owned());
        let mut fx = Fixture::new();
        fs::create_dir(fx.repo.join("src")).unwrap();
        fx.commit("src/main.rs", "base\n");
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-cap", "unit-cap", "topic-cap");
        action.path_capability =
            serde_json::from_value(case["before"]["capability"].clone()).unwrap();
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        if case["before"]["state"] == "sealed" {
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
                git(&dest.join("src/main.rs"), &["init", "-b", "main"]);
                git(
                    &dest.join("src/main.rs"),
                    &["config", "user.email", "ws@example.com"],
                );
                git(
                    &dest.join("src/main.rs"),
                    &["config", "user.name", "Workspace Test"],
                );
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
        let old_workspace = docs.workspaces["ws-cap"].clone();
        let old_receipts = docs.receipts.clone();
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
        assert_eq!(
            result.is_ok(),
            case["disposition"] == "accepted",
            "{name}: {result:?}"
        );
        if case["disposition"] == "denied" {
            assert_eq!(
                docs.workspaces["ws-cap"], old_workspace,
                "{name}: workspace changed"
            );
            assert_eq!(
                docs.receipts, old_receipts,
                "{name}: success receipt changed"
            );
        } else {
            assert_eq!(docs.workspaces["ws-cap"].lifecycle_state, "sealed");
            assert_eq!(
                docs.receipts
                    .values()
                    .filter(|r| r.kind == "writer")
                    .count(),
                1
            );
        }
        assert_eq!(
            git(&fx.repo, &["rev-parse", "HEAD"]),
            trunk,
            "{name}: seal changed trunk"
        );
    }
    assert_eq!(
        exercised.len(),
        11,
        "generated seal coverage changed: {exercised:?}"
    );
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

fn generated_workspace_receipt_cases_recover_without_checkout_or_reapplying() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["workspace_path_capability_cases"]
        .as_array()
        .unwrap();
    let mut exercised = 0;
    for case in cases.iter().filter(|case| {
        matches!(
            case["operation"].as_str(),
            Some("replay_seal" | "replay_integrate")
        )
    }) {
        exercised += 1;
        let name = case["name"].as_str().unwrap();
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
        if case["operation"] == "replay_integrate" {
            bind_integrate(
                &mut docs,
                "ws-replay-cap",
                "req-int",
                sealed.workspace.seal_hash.as_deref().unwrap(),
            );
            integrate_writer(&fx, &mut docs, "ws-replay-cap", "req-int");
        }
        if name == "changed_capability_cannot_replay_receipt" {
            // Mutable test storage deliberately violates immutable row identity;
            // the existing receipt must not authenticate this widened binding.
            docs.workspaces
                .get_mut("ws-replay-cap")
                .unwrap()
                .path_capability = WorkspacePathCapability::UnrestrictedCompatibility;
        }
        fs::remove_dir_all(&dest).unwrap();
        let before_receipts = docs.receipts.clone();
        let before_workspace = docs.workspaces["ws-replay-cap"].clone();
        let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
        let mut journal = vec![ActionJournalEntry::new(
            0,
            ActionJournalState::ResultDocsWritten,
        )];
        let succeeded = if case["operation"] == "replay_seal" {
            execute_seal_workspace_plan(
                &emit_seal_workspace_plan(SealWorkspaceAction {
                    workspace_id: "ws-replay-cap".into(),
                    produced_by_request_id: "req-writer".into(),
                    produced_by_request_doc_id: "req-writer-doc".into(),
                }),
                &mut journal,
                &mut fx.ctx(&mut docs, seal_caps()),
            )
            .is_ok()
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
            .is_ok()
        };
        assert_eq!(succeeded, case["disposition"] == "recovered", "{name}");
        assert!(
            !dest.exists(),
            "receipt replay must not recreate a checkout"
        );
        assert_eq!(
            git(&fx.repo, &["rev-parse", "HEAD"]),
            before_head,
            "{name}: replay applied again"
        );
        assert_eq!(docs.receipts, before_receipts, "{name}: receipt changed");
        assert_eq!(docs.workspaces["ws-replay-cap"], before_workspace);
    }
    assert_eq!(exercised, 3);
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

fn generated_workspace_integration_cases_apply_only_authorized_snapshot() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["workspace_path_capability_cases"]
        .as_array()
        .unwrap();
    let mut exercised = 0;
    for case in cases.iter().filter(|case| case["operation"] == "integrate") {
        exercised += 1;
        let name = case["name"].as_str().unwrap();
        let mut fx = Fixture::new();
        fs::create_dir(fx.repo.join("src")).unwrap();
        fx.commit("src/main.rs", "base\n");
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-cap-integrate", "unit", "topic-cap-integrate");
        action.path_capability =
            serde_json::from_value(case["before"]["capability"].clone()).unwrap();
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
        let before_receipts = docs.receipts.clone();
        let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
        let before_count: usize = git(&fx.repo, &["rev-list", "--count", "HEAD"])
            .parse()
            .unwrap();
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
        assert_eq!(
            result.is_ok(),
            case["disposition"] == "accepted",
            "{name}: {result:?}"
        );
        if let Ok(integrated) = result {
            finalize_integrate_trunk(&fx.repo, integrated.pending_head_sha.as_deref()).unwrap();
            assert_eq!(
                fs::read_to_string(fx.repo.join("src/main.rs")).unwrap(),
                "authorized\n"
            );
            let after_count: usize = git(&fx.repo, &["rev-list", "--count", "HEAD"])
                .parse()
                .unwrap();
            assert_eq!(
                after_count - before_count,
                case["expected"]["trunk_effects"].as_u64().unwrap() as usize
            );
            assert_eq!(
                docs.receipts
                    .values()
                    .filter(|r| r.kind == "integrator")
                    .count(),
                1
            );
        } else {
            assert_eq!(
                git(&fx.repo, &["rev-parse", "HEAD"]),
                before_head,
                "{name}: trunk moved"
            );
            assert_eq!(
                fs::read_to_string(fx.repo.join("src/main.rs")).unwrap(),
                "base\n"
            );
            assert!(!fx.repo.join("outside.rs").exists());
            assert_eq!(docs.receipts, before_receipts, "{name}: receipt changed");
        }
        assert_eq!(
            docs.workspaces["ws-cap-integrate"], before_workspace,
            "{name}: immutable seal changed"
        );
    }
    assert_eq!(exercised, 4);
}

fn generated_workspace_fresh_admission_requires_exact_canonical_capability() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["workspace_path_capability_cases"]
        .as_array()
        .unwrap();
    let names = [
        "fresh_exact_provisions",
        "fresh_legacy_cannot_provision",
        "malformed_manifest_denied",
    ];
    for name in names {
        let case = cases.iter().find(|case| case["name"] == name).unwrap();
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-fresh-cap", "unit", "topic-fresh-cap");
        action.path_capability =
            serde_json::from_value(case["before"]["capability"].clone()).unwrap();
        if name == "malformed_manifest_denied" {
            action.path_capability = WorkspacePathCapability::ExactPaths {
                paths: vec!["../escape".into()],
            };
        }
        let result = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        );
        assert_eq!(
            result.is_ok(),
            case["disposition"] == "accepted",
            "{name}: {result:?}"
        );
        if let Ok(created) = result {
            assert_eq!(created.workspace.lifecycle_state, "ready");
            assert!(created.workspace.path_capability.is_exact());
        } else {
            assert!(
                docs.workspaces.is_empty(),
                "{name}: denied creation persisted workspace"
            );
            assert!(docs.receipts.is_empty());
        }
    }
}

fn generated_legacy_workspace_cases_recover_identity_without_fresh_provision() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    for name in [
        "existing_identical_legacy_recovers",
        "legacy_recovery_missing_checkout_cannot_reprovision",
    ] {
        let case = snapshot["workspace_path_capability_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut action = fx.action("ws-legacy-cap", "unit", "topic-legacy-cap");
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(action.clone()),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        // Simulate the precise migration boundary: persisted old row now has
        // explicit compatibility, while the historical host marker lacks it.
        docs.workspaces
            .get_mut("ws-legacy-cap")
            .unwrap()
            .path_capability = WorkspacePathCapability::UnrestrictedCompatibility;
        action.path_capability = WorkspacePathCapability::UnrestrictedCompatibility;
        let git_dir = PathBuf::from(git(&dest, &["rev-parse", "--absolute-git-dir"]));
        let marker = git_dir.join("gents-workspace-identity.json");
        let mut old: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        old.as_object_mut().unwrap().remove("path_capability");
        fs::write(&marker, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
        let present = case["evidence"]["checkout_present"].as_bool().unwrap();
        if !present {
            fs::remove_dir_all(&dest).unwrap();
        }
        let before = docs.workspaces["ws-legacy-cap"].clone();
        let result = execute_create_workspace_plan(
            &emit_create_workspace_plan(action),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        );
        assert_eq!(
            result.is_ok(),
            case["disposition"] == "recovered",
            "{name}: {result:?}"
        );
        assert_eq!(
            dest.exists(),
            present,
            "{name}: legacy recovery reprovisioned a missing checkout"
        );
        assert_eq!(
            docs.workspaces["ws-legacy-cap"].path_capability,
            before.path_capability
        );
        assert!(docs.receipts.is_empty());
    }
}

fn generated_noncanonical_git_delta_is_rejected_without_filesystem_traversal() {
    use std::io::Write;
    use std::process::Stdio;
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let case = snapshot["workspace_path_capability_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "noncanonical_delta_rejected")
        .unwrap();
    let fx = Fixture::new();
    let cap: WorkspacePathCapability =
        serde_json::from_value(case["before"]["capability"].clone()).unwrap();
    let blob = git(&fx.repo, &["rev-parse", "HEAD:README.md"]);
    let mut tree = b"100644 ../outside\0".to_vec();
    for byte in blob.as_bytes().chunks_exact(2) {
        tree.push(u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap());
    }
    let mut child = Command::new("git")
        .args(["hash-object", "--literally", "-t", "tree", "-w", "--stdin"])
        .current_dir(&fx.repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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
    assert!(
        super::adapter::validate_tree_delta(&fx.repo, &fx.base_sha, tree_id.trim(), &cap).is_err()
    );
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before);
    assert!(!fx.parent().join("outside").exists());
}

#[path = "tests/capability_regressions.rs"]
mod capability_regressions;

#[test]
fn generated_workspace_path_capability_cases_drive_real_git_executor() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(
        snapshot["workspace_path_capability_cases"]
            .as_array()
            .unwrap()
            .len(),
        24,
        "review and wire every new generated operation case"
    );
    generated_workspace_path_capability_seal_cases_drive_real_git_executor();
    generated_workspace_receipt_cases_recover_without_checkout_or_reapplying();
    generated_workspace_integration_cases_apply_only_authorized_snapshot();
    generated_workspace_fresh_admission_requires_exact_canonical_capability();
    generated_legacy_workspace_cases_recover_identity_without_fresh_provision();
    generated_noncanonical_git_delta_is_rejected_without_filesystem_traversal();
}

#[path = "tests/capability_runtime.rs"]
mod capability_runtime;
