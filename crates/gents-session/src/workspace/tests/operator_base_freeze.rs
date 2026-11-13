use super::*;

#[test]
fn generated_operator_base_freeze_cases_drive_real_git_executor() {
    use gents_lean_vocab::families::operator_base_freeze_contracts_freeze::CapabilityWire;
    fn capability_of(wire: &CapabilityWire) -> WorkspacePathCapability {
        match wire.mode.as_str() {
            "exactPaths" => {
                WorkspacePathCapability::exact_paths(wire.paths.clone().unwrap()).unwrap()
            }
            "unrestrictedCompatibility" => WorkspacePathCapability::UnrestrictedCompatibility,
            other => panic!("unmapped modeled capability {other}"),
        }
    }
    for case in gents_lean_vocab::families::operator_base_freeze_contracts_freeze::cases::<String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
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
        if input.before_state != "ready" {
            execute_freeze_workspace_base_plan(
                &emit_freeze_workspace_base_plan(action.clone()),
                &mut Vec::new(),
                &mut fx.ctx(&mut docs, seal_caps()),
            )
            .expect("prepare actual operator seal, never fabricate a writer request");
            if input.before_state == "cleaned" {
                execute_cleanup_workspace_plan(
                    &emit_cleanup_workspace_plan(CleanupWorkspaceAction {
                        workspace_id: "workspace-1".into(),
                    }),
                    &mut Vec::new(),
                    &mut fx.ctx(&mut docs, cleanup_caps()),
                )
                .expect("prepare the cleaned row through the real cleanup owner");
            } else if input.before_state == "cleaning" {
                let real_host_path = docs.placements["workspace-1"].host_path.clone();
                docs.placements.get_mut("workspace-1").unwrap().host_path = dest
                    .with_file_name("gents-ws-workspace-1-drifted")
                    .display()
                    .to_string();
                let cleanup_error = execute_cleanup_workspace_plan(
                    &emit_cleanup_workspace_plan(CleanupWorkspaceAction {
                        workspace_id: "workspace-1".into(),
                    }),
                    &mut Vec::new(),
                    &mut fx.ctx(&mut docs, cleanup_caps()),
                )
                .expect_err("drifted placement must stop the cleanup effect");
                assert!(
                    cleanup_error
                        .to_string()
                        .contains("does not match host-chosen path"),
                    "{cleanup_error}"
                );
                assert_eq!(
                    docs.workspaces["workspace-1"].lifecycle_state, "cleaning",
                    "the owner must have persisted the Cleaning row before failing"
                );
                assert!(
                    dest.is_dir(),
                    "the real tree must survive the failed effect"
                );
                docs.placements.get_mut("workspace-1").unwrap().host_path = real_host_path;
            } else if input.before_state == "sealed" {
            } else {
                panic!("unmapped emitted operator-freeze before_state {input:?}");
            }
        }
        let mut caps = seal_caps();
        let mut wrong_owner = false;
        match name {
            "clean_empty_base_freezes" | "identical_seal_replay" => {}
            "missing_seal_capability_denied" => caps.clear(),
            "wrong_owner_or_placement_denied" => wrong_owner = true,
            "nonempty_exact_capability_denied" | "legacy_capability_denied" => {
                let workspace = docs.workspaces.get_mut("workspace-1").unwrap();
                workspace.path_capability = capability_of(&input.capability);
                super::super::adapter::write_identity(&dest, &workspace.identity()).unwrap();
            }
            "active_writer_denied" => {
                docs.write_binding(super::super::binding::new_binding(
                    "workspace-1",
                    "existing-writer",
                    "existing-writer-doc",
                    gents_loop::tool_policy::WorkspaceAuthority::ReadWrite,
                    "did:key:zWorkspaceOwner",
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
            "missing_checkout_denied" | "identical_replay_without_checkout" => {
                fs::remove_dir_all(&dest).unwrap();
            }
            "malformed_manifest_denied" => {
                let git_dir = super::super::adapter::absolute_git_dir(&dest).unwrap();
                fs::write(git_dir.join("gents-workspace-identity.json"), b"not-json").unwrap();
            }
            "wrong_identity_binding_denied" => {
                git(&fx.repo, &["commit", "--allow-empty", "-m", "other base"]);
                action.base_sha = git(&fx.repo, &["rev-parse", "HEAD"]);
            }
            "different_seal_replay_denied" => {
                docs.workspaces.get_mut("workspace-1").unwrap().seal_hash =
                    Some("other-tree".into());
            }
            "cleaning_replay_denied" | "cleaned_replay_denied" => {}
            other => panic!("unmapped emitted operator-freeze case {other}"),
        }
        assert_eq!(input.base_tree, "base-tree");
        assert_eq!(input.expected_binding.workspace_id, action.workspace_id);
        assert_eq!(input.expected_binding.owner, "host-1");
        assert_eq!(input.expected_binding.tree, "base-tree");
        assert_eq!(
            capability_of(&input.expected_binding.capability),
            capability_of(&input.capability)
        );
        assert_eq!(
            input.expected_binding.base == "base-commit",
            action.base_sha == fx.base_sha
        );
        let before = docs.workspaces["workspace-1"].clone();
        assert_eq!(before.lifecycle_state, input.before_state);
        let expected_seal = match input.before_seal.as_deref() {
            None => None,
            Some("base-tree") => Some(base_tree.as_str()),
            Some("other-tree") => Some("other-tree"),
            Some(other) => panic!("unmapped seal label {other}"),
        };
        assert_eq!(before.seal_hash.as_deref(), expected_seal);
        assert_eq!(before.path_capability, capability_of(&input.capability));
        assert_eq!(input.seal_capability, caps.contains(CAP_SEAL_WORKSPACE));
        assert_eq!(input.owner_and_placement_verified, !wrong_owner);
        assert_eq!(input.checkout_present, dest.exists());
        assert_eq!(
            input.no_active_writer,
            !docs.bindings.values().any(|row| row.is_active_read_write())
        );
        assert_eq!(
            input.manifest_canonical,
            name != "malformed_manifest_denied"
        );
        assert_eq!(
            input.captured_base_matches,
            name != "changed_committed_head_denied"
        );
        assert_eq!(input.changed_paths.is_empty(), name != "dirty_delta_denied");
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
                ctx.owner_node_did = "foreign-owner".into();
            }
            execute_freeze_workspace_base_plan(&plan, &mut journal, &mut ctx)
        };
        let unchanged = serde_json::json!({"workspaces": docs.workspaces, "placements": docs.placements, "bindings": docs.bindings, "receipts": docs.receipts})
            == before_docs;
        let disposition = match &result {
            Err(_) => "denied",
            Ok(_) if unchanged => "recovered",
            Ok(_) => "accepted",
        };
        if disposition == "denied" {
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
            assert!(super::super::journal::action_journal_prefix_legal(&journal));
        }
        let state = docs.workspaces["workspace-1"].lifecycle_state.clone();
        let writer_receipt = !docs.receipts.is_empty();
        let trunk_effects: u64 = git(
            &fx.repo,
            &["rev-list", "--count", &format!("{trunk_before}...HEAD")],
        )
        .parse()
        .unwrap();
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
        case.check(
            gents_lean_vocab::families::operator_base_freeze_contracts_freeze::FreezeOutput {
                state,
                disposition: disposition.to_owned(),
                writer_receipt,
                trunk_effects,
            },
        );
    }
}

#[test]
fn freeze_rejects_drifted_placement_binding() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut create = fx.action("freeze-placement", "freeze-work", "freeze-placement-branch");
    create.path_capability = WorkspacePathCapability::exact_paths(Vec::new()).unwrap();
    execute_create_workspace_plan(
        &emit_create_workspace_plan(create),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    let before_workspace = docs.workspaces["freeze-placement"].clone();
    let freeze_plan = emit_freeze_workspace_base_plan(FreezeWorkspaceBaseAction {
        workspace_id: "freeze-placement".into(),
        base_sha: fx.base_sha.clone(),
    });

    docs.placements
        .get_mut("freeze-placement")
        .unwrap()
        .repository_placement_id = "repo-2".into();
    let mut journal = Vec::new();
    let error = execute_freeze_workspace_base_plan(
        &freeze_plan,
        &mut journal,
        &mut fx.ctx(&mut docs, seal_caps()),
    )
    .expect_err("drifted placement binding must not freeze");
    assert!(matches!(error, HostExecuteError::Denied { .. }), "{error}");
    assert!(
        error
            .to_string()
            .contains("owner/repository/placement mismatch"),
        "{error}"
    );
    assert_eq!(
        docs.workspaces["freeze-placement"], before_workspace,
        "denial must not mutate the authoritative workspace"
    );
    assert!(docs.receipts.is_empty());
    assert_eq!(
        super::super::journal::current_state(&journal, 0),
        Some(ActionJournalState::Validated)
    );

    docs.placements
        .get_mut("freeze-placement")
        .unwrap()
        .repository_placement_id = "repo-1".into();
    let real_host_path = docs.placements["freeze-placement"].host_path.clone();
    docs.placements
        .get_mut("freeze-placement")
        .unwrap()
        .host_path = PathBuf::from(&real_host_path)
        .with_file_name("gents-ws-freeze-placement-drifted")
        .display()
        .to_string();
    let mut journal = Vec::new();
    let error = execute_freeze_workspace_base_plan(
        &freeze_plan,
        &mut journal,
        &mut fx.ctx(&mut docs, seal_caps()),
    )
    .expect_err("drifted placement host path must not freeze");
    assert!(matches!(error, HostExecuteError::Denied { .. }), "{error}");
    assert!(
        error
            .to_string()
            .contains("placement differs from host-chosen workspace path"),
        "{error}"
    );
    assert_eq!(docs.workspaces["freeze-placement"], before_workspace);
    assert!(docs.receipts.is_empty());
}

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
        owner_node_did: "did:key:zWorkspaceOwner".into(),
        repository: RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            owner_node_did: "did:key:zWorkspaceOwner".into(),
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
        route: Default::default(),
    }
}

#[test]
fn base_freeze_recovery_matches_the_lean_model() {
    gents_lean_vocab::families::base_freeze_recovery_steps::replay::<String>(|input| {
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
        if input.placement_cleared {
            memory
                .placements
                .get_mut("freeze-recovery")
                .unwrap()
                .observed_tree_hash
                .clear();
        }
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
            fail_placement: false,
            workspace_writes: 0,
            placement_writes: 0,
        };
        input
            .faults
            .iter()
            .map(|&fault| {
                docs.fail_placement = fault;
                let mut journal = Vec::new();
                let result = execute_freeze_workspace_base_plan(
                    &plan,
                    &mut journal,
                    &mut freeze_fault_context(&fx, &mut docs),
                );
                let workspace = &docs.inner.workspaces["freeze-recovery"];
                gents_lean_vocab::families::base_freeze_recovery_steps::StepOutput {
                    ok: result.is_ok(),
                    returned_placement_at_base: result
                        .as_ref()
                        .ok()
                        .map(|outcome| outcome.placement.observed_tree_hash == base_tree),
                    journal: match super::super::journal::current_state(&journal, 0) {
                        Some(ActionJournalState::Validated) => "validated",
                        Some(ActionJournalState::Executing) => "executing",
                        Some(ActionJournalState::EffectObserved) => "effect_observed",
                        Some(ActionJournalState::ResultDocsWritten) => "result_docs_written",
                        other => panic!("unmodeled journal state {other:?}"),
                    }
                    .to_owned(),
                    workspace_state: workspace.lifecycle_state.clone(),
                    sealed_at_base: workspace.seal_hash.as_deref() == Some(base_tree.as_str()),
                    placement_at_base: docs.inner.placements["freeze-recovery"]
                        .observed_tree_hash
                        == base_tree,
                    workspace_writes: docs.workspace_writes as u64,
                    placement_writes: docs.placement_writes as u64,
                    receipts: docs.inner.receipts.len() as u64,
                    trunk_moved: git(&fx.repo, &["rev-parse", "HEAD"]) != trunk,
                }
            })
            .collect()
    });
}
