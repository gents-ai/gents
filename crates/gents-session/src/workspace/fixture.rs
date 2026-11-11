use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

pub fn capabilities() -> BTreeSet<String> {
    [
        CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
        CAP_CLONE_ARTIFACTS,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub fn git_worktree_caps() -> BTreeSet<String> {
    [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn seal_caps() -> BTreeSet<String> {
    [CAP_SEAL_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn integrate_caps() -> BTreeSet<String> {
    [CAP_INTEGRATE_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn cleanup_caps() -> BTreeSet<String> {
    [CAP_CLEANUP_WORKSPACE]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = crate::exec::native_spawn::output_std(
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_TERMINAL_PROMPT", "0"),
    )
    .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub struct Fixture {
    pub _root: TempDir,
    pub repo: PathBuf,
    pub base_sha: String,
}

impl Fixture {
    pub fn new() -> Self {
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

    pub fn parent(&self) -> &Path {
        self.repo.parent().expect("repo parent")
    }

    pub fn action(
        &self,
        workspace_id: &str,
        work_unit_id: &str,
        branch: &str,
    ) -> CreateWorkspaceAction {
        CreateWorkspaceAction {
            writer_agent_did: None,
            writer_agent_doc_id: None,
            integrator_agent_did: None,
            integrator_agent_doc_id: None,
            readers: Vec::new(),
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

    pub fn commit(&mut self, rel: &str, content: &str) {
        fs::write(self.repo.join(rel), content).unwrap();
        git(&self.repo, &["add", rel]);
        git(&self.repo, &["commit", "-m", rel]);
        self.base_sha = git(&self.repo, &["rev-parse", "HEAD"]);
    }

    pub fn ctx<'a>(
        &'a self,
        docs: &'a mut MemoryWorkspaceDocuments,
        caps: BTreeSet<String>,
    ) -> HostExecutorContext<'a> {
        HostExecutorContext {
            owner_node_did: "did:key:zWorkspaceOwner".to_string(),
            repository: RepositoryPlacementRef {
                repository_id: "repo-1".to_string(),
                owner_node_did: "did:key:zWorkspaceOwner".to_string(),
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
            route: Default::default(),
        }
    }
}

pub fn seal_writer(
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

pub fn bind_integrate(
    docs: &mut MemoryWorkspaceDocuments,
    workspace_id: &str,
    request_id: &str,
    seal_hash: &str,
) {
    docs.write_binding(super::binding::new_binding(
        workspace_id,
        request_id,
        &format!("{request_id}-doc"),
        gents_loop::tool_policy::WorkspaceAuthority::Integrate,
        "did:key:zWorkspaceOwner",
        Some(seal_hash),
    ))
    .unwrap();
}

pub fn integrate_writer(
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
    super::executor::integrate::clear_integrate_journal(&fx.repo, workspace_id);
    outcome
}
