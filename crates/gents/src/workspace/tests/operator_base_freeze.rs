//! Real Git / existing memory document-adapter consumer; not a native DB isolation proof.
use super::*;

#[test]
fn generated_operator_base_freeze_cases_drive_real_git_executor() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["operator_base_freeze_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 16);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let mut create = fx.action("workspace-1", "work-1", "workspace-branch");
        create.path_capability = WorkspacePathCapability::exact_paths(Vec::new()).unwrap();
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(create),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        let dest = PathBuf::from(&created.placement.host_path);
        let base_tree = git(
            &fx.repo,
            &["rev-parse", &format!("{}^{{tree}}", fx.base_sha)],
        );
        assert_ne!(
            fx.base_sha, base_tree,
            "commit and tree are distinct identities"
        );
        let mut action = FreezeWorkspaceBaseAction {
            workspace_id: "workspace-1".into(),
            base_sha: fx.base_sha.clone(),
        };
        if case["before_state"] != "ready" {
            execute_freeze_workspace_base_plan(
                &emit_freeze_workspace_base_plan(action.clone()),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, seal_caps()),
            )
            .expect("prepare actual operator seal, never fabricate a writer request");
            docs.workspaces
                .get_mut("workspace-1")
                .unwrap()
                .lifecycle_state = case["before_state"].as_str().unwrap().into();
        }
        let mut caps = seal_caps();
        let mut wrong_owner = false;
        match name {
            "clean_empty_base_freezes" | "identical_seal_replay" => {}
            "missing_seal_capability_denied" => caps.clear(),
            "wrong_owner_or_placement_denied" => wrong_owner = true,
            "nonempty_exact_capability_denied" | "legacy_capability_denied" => {
                let workspace = docs.workspaces.get_mut("workspace-1").unwrap();
                workspace.path_capability =
                    serde_json::from_value(case["capability"].clone()).unwrap();
                // Keep the host marker canonical and consistent: the rejected
                // capability itself must be the failing guard, not marker drift.
                super::super::adapter::write_identity(&dest, &workspace.identity()).unwrap();
            }
            "active_writer_denied" => {
                docs.write_binding(super::super::binding::new_binding(
                    "workspace-1",
                    "existing-writer",
                    "existing-writer-doc",
                    crate::toolset::WorkspaceAuthority::ReadWrite,
                    "deploy-1",
                    None,
                ))
                .unwrap();
            }
            "dirty_delta_denied" => {
                fs::write(dest.join("changed.txt"), "untracked delta\n").unwrap();
            }
            "changed_committed_head_denied" => {
                fs::write(dest.join("README.md"), "committed worker delta\n").unwrap();
                git(&dest, &["add", "README.md"]);
                git(&dest, &["commit", "-m", "different head"]);
            }
            "missing_checkout_denied"
            | "identical_replay_without_checkout"
            | "cleaned_replay_denied" => {
                fs::remove_dir_all(&dest).unwrap();
            }
            "malformed_manifest_denied" => {
                let git_dir = super::super::adapter::absolute_git_dir(&dest).unwrap();
                fs::write(git_dir.join("gents-workspace-identity.json"), b"not-json").unwrap();
            }
            "wrong_identity_binding_denied" => {
                // A real different commit, not an unresolvable invented SHA.
                git(&fx.repo, &["commit", "--allow-empty", "-m", "other base"]);
                action.base_sha = git(&fx.repo, &["rev-parse", "HEAD"]);
            }
            "different_seal_replay_denied" => {
                docs.workspaces.get_mut("workspace-1").unwrap().seal_hash =
                    Some("other-tree".into());
            }
            "cleaning_replay_denied" => {}
            other => panic!("unmapped emitted operator-freeze case {other}"),
        }
        assert_eq!(case["base_tree"], "base-tree");
        assert_eq!(
            case["expected_binding"]["workspace_id"],
            action.workspace_id
        );
        assert_eq!(case["expected_binding"]["owner"], "host-1"); // abstract host-1 -> Fixture deploy-1
        assert_eq!(case["expected_binding"]["tree"], "base-tree"); // abstract base-tree -> actual Git tree
        assert_eq!(case["expected_binding"]["capability"], case["capability"]);
        assert_eq!(
            case["expected_binding"]["base"] == "base-commit",
            action.base_sha == fx.base_sha
        );
        let before = docs.workspaces["workspace-1"].clone();
        assert_eq!(
            before.lifecycle_state,
            case["before_state"].as_str().unwrap()
        );
        let expected_seal = match case["before_seal"].as_str() {
            None => None,
            Some("base-tree") => Some(base_tree.as_str()),
            Some("other-tree") => Some("other-tree"),
            Some(other) => panic!("unmapped seal label {other}"),
        };
        assert_eq!(before.seal_hash.as_deref(), expected_seal);
        assert_eq!(
            serde_json::to_value(&before.path_capability).unwrap(),
            case["capability"]
        );
        assert_eq!(case["seal_capability"], caps.contains(CAP_SEAL_WORKSPACE));
        assert_eq!(case["owner_and_placement_verified"], !wrong_owner);
        assert_eq!(case["checkout_present"], dest.exists());
        assert_eq!(
            case["no_active_writer"],
            !docs.bindings.values().any(|row| row.is_active_read_write())
        );
        assert_eq!(
            case["manifest_canonical"],
            name != "malformed_manifest_denied"
        );
        assert_eq!(
            case["captured_base_matches"],
            name != "changed_committed_head_denied"
        );
        assert_eq!(
            case["changed_paths"].as_array().unwrap().is_empty(),
            name != "dirty_delta_denied"
        );
        let before_docs = serde_json::json!({"workspaces": docs.workspaces, "placements": docs.placements, "bindings": docs.bindings, "receipts": docs.receipts});
        let trunk_before = git(&fx.repo, &["rev-parse", "HEAD"]);
        let index_before = git(&fx.repo, &["write-tree"]);
        let plan = emit_freeze_workspace_base_plan(action);
        let encoded = serde_json::to_value(&plan).unwrap().to_string();
        assert!(!encoded.contains("produced_by_request"));
        let mut journal = Vec::new();
        let result = {
            let mut ctx = fx.ctx(&mut docs, caps);
            if wrong_owner {
                ctx.deployment_id = "foreign-deployment".into();
            }
            execute_freeze_workspace_base_plan(&plan, &mut journal, &mut ctx)
        };
        let disposition = case["expected_disposition"].as_str().unwrap();
        if disposition == "denied" {
            assert!(result.is_err(), "{name}: unauthorized freeze succeeded");
            assert_eq!(
                serde_json::json!({"workspaces": docs.workspaces, "placements": docs.placements, "bindings": docs.bindings, "receipts": docs.receipts}),
                before_docs,
                "{name}: denial mutated authoritative documents"
            );
        } else {
            let result = result.unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(
                result.workspace.seal_hash.as_deref(),
                Some(base_tree.as_str())
            );
            assert_eq!(result.placement.observed_tree_hash, base_tree);
            if disposition == "recovered" {
                assert_eq!(
                    serde_json::json!({"workspaces": docs.workspaces, "placements": docs.placements, "bindings": docs.bindings, "receipts": docs.receipts}),
                    before_docs,
                    "{name}: replay must not manufacture effects"
                );
            }
            assert!(super::super::journal::action_journal_prefix_legal(&journal));
        }
        assert_eq!(
            docs.workspaces["workspace-1"].lifecycle_state,
            case["expected_state"].as_str().unwrap()
        );
        assert_eq!(case["expected_writer_receipt"], false);
        assert!(
            docs.receipts.is_empty(),
            "{name}: operator freeze must not invent writer/integrator provenance"
        );
        assert_eq!(case["expected_trunk_effects"], 0);
        assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), trunk_before);
        assert_eq!(git(&fx.repo, &["write-tree"]), index_before);
        if name == "clean_empty_base_freezes" {
            bind_integrate(&mut docs, "workspace-1", "existing-integrator", &base_tree);
            let integration = execute_integrate_workspace_plan(
                &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
                    workspace_id: "workspace-1".into(),
                    produced_by_request_id: "existing-integrator".into(),
                    produced_by_request_doc_id: "existing-integrator-doc".into(),
                    mode: IntegrateMode::ApplyDiff,
                }),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, integrate_caps()),
            );
            assert!(
                integration.is_err(),
                "operator seal cannot substitute for a real writer receipt"
            );
            assert!(docs.receipts.is_empty());
            assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), trunk_before);
        }
    }
}

// Partial result-document persistence must converge through the existing executor.

struct FailFreezePlacementOnce {
    inner: MemoryWorkspaceDocuments,
    fail_placement: bool,
    workspace_writes: usize,
    placement_writes: usize,
}

impl WorkspaceDocuments for FailFreezePlacementOnce {
    fn load_isolated_workspace(&self, id: &str) -> anyhow::Result<Option<IsolatedWorkspaceDoc>> {
        self.inner.load_isolated_workspace(id)
    }
    fn load_placement(&self, id: &str) -> anyhow::Result<Option<WorkspacePlacementDoc>> {
        self.inner.load_placement(id)
    }
    fn write_isolated_workspace(&mut self, doc: IsolatedWorkspaceDoc) -> anyhow::Result<()> {
        self.workspace_writes += 1;
        self.inner.write_isolated_workspace(doc)
    }
    fn write_placement(&mut self, doc: WorkspacePlacementDoc) -> anyhow::Result<()> {
        self.placement_writes += 1;
        if std::mem::take(&mut self.fail_placement) {
            anyhow::bail!("injected placement persistence failure after workspace write");
        }
        self.inner.write_placement(doc)
    }
    fn load_bindings(&self, id: &str) -> anyhow::Result<Vec<WorkspaceBindingDoc>> {
        self.inner.load_bindings(id)
    }
    fn write_binding(&mut self, doc: WorkspaceBindingDoc) -> anyhow::Result<()> {
        self.inner.write_binding(doc)
    }
    fn load_receipts(&self, id: &str) -> anyhow::Result<Vec<WorkspaceReceiptDoc>> {
        self.inner.load_receipts(id)
    }
    fn write_receipt(&mut self, doc: WorkspaceReceiptDoc) -> anyhow::Result<()> {
        self.inner.write_receipt(doc)
    }
}

fn freeze_fault_context<'a>(
    fx: &'a Fixture,
    docs: &'a mut dyn WorkspaceDocuments,
) -> HostExecutorContext<'a> {
    HostExecutorContext {
        deployment_id: "deploy-1".into(),
        repository: RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            deployment_id: "deploy-1".into(),
            host_path: fx.repo.clone(),
            enabled: true,
        },
        ceiling: Some(&fx.repo),
        capabilities: seal_caps(),
        writer_principal: "did:key:zWriter".into(),
        integrator_principal: "did:key:zIntegrator".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
        documents: docs,
    }
}

#[test]
fn operator_base_freeze_recovers_partial_documents_with_fresh_journal() {
    let fx = Fixture::new();
    let mut memory = MemoryWorkspaceDocuments::default();
    let mut create = fx.action("freeze-recovery", "freeze-work", "freeze-recovery-branch");
    create.path_capability = WorkspacePathCapability::exact_paths(vec![]).unwrap();
    execute_create_workspace_plan(
        &emit_create_workspace_plan(create),
        &mut Vec::new(),
        &mut fx.ctx(&mut memory, git_worktree_caps()),
    )
    .unwrap();
    // Placement observation is derived, not the authoritative seal. Start with
    // an unrecorded observation so failed persistence leaves a visible mismatch.
    memory
        .placements
        .get_mut("freeze-recovery")
        .unwrap()
        .observed_tree_hash
        .clear();
    let base_tree = git(
        &fx.repo,
        &["rev-parse", &format!("{}^{{tree}}", fx.base_sha)],
    );
    let trunk = git(&fx.repo, &["rev-parse", "HEAD"]);
    let plan = emit_freeze_workspace_base_plan(FreezeWorkspaceBaseAction {
        workspace_id: "freeze-recovery".into(),
        base_sha: fx.base_sha.clone(),
    });
    let mut docs = FailFreezePlacementOnce {
        inner: memory,
        fail_placement: true,
        workspace_writes: 0,
        placement_writes: 0,
    };
    let mut interrupted = Vec::new();
    let failed = execute_freeze_workspace_base_plan(
        &plan,
        &mut interrupted,
        &mut freeze_fault_context(&fx, &mut docs),
    );
    assert!(failed.is_err());
    assert_eq!(
        docs.inner.workspaces["freeze-recovery"].lifecycle_state,
        "sealed"
    );
    assert_eq!(
        docs.inner.workspaces["freeze-recovery"]
            .seal_hash
            .as_deref(),
        Some(base_tree.as_str())
    );
    assert_eq!(
        docs.inner.placements["freeze-recovery"].observed_tree_hash,
        ""
    );
    assert!(docs.inner.receipts.is_empty());
    assert_eq!(
        super::super::journal::current_state(&interrupted, 0),
        Some(ActionJournalState::EffectObserved)
    );

    // The ordinary runtime can restart with a new in-memory action journal.
    let mut restarted = Vec::new();
    let recovered = execute_freeze_workspace_base_plan(
        &plan,
        &mut restarted,
        &mut freeze_fault_context(&fx, &mut docs),
    )
    .unwrap();
    assert_eq!(recovered.placement.observed_tree_hash, base_tree);
    assert_eq!(
        docs.inner.placements["freeze-recovery"].observed_tree_hash,
        base_tree
    );
    assert!(docs.inner.receipts.is_empty());
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), trunk);
    assert_eq!(
        super::super::journal::current_state(&restarted, 0),
        Some(ActionJournalState::ResultDocsWritten)
    );

    let writes = (docs.workspace_writes, docs.placement_writes);
    execute_freeze_workspace_base_plan(
        &plan,
        &mut Vec::new(),
        &mut freeze_fault_context(&fx, &mut docs),
    )
    .unwrap();
    assert_eq!(
        (docs.workspace_writes, docs.placement_writes),
        writes,
        "identical persisted seal replay must not write documents"
    );
}
