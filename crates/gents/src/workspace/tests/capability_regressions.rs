use super::*;

#[test]
fn unchanged_tracked_ignored_file_remains_in_exact_empty_seal() {
    let mut fx = Fixture::new();
    fx.commit(".gitignore", "ignored.txt\n");
    fs::write(fx.repo.join("ignored.txt"), "tracked despite ignore\n").unwrap();
    git(&fx.repo, &["add", "-f", "ignored.txt"]);
    git(&fx.repo, &["commit", "-m", "track ignored file"]);
    fx.base_sha = git(&fx.repo, &["rev-parse", "HEAD"]);
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("ws-ignored", "unit", "topic-ignored");
    action.path_capability = WorkspacePathCapability::exact_paths(Vec::new()).unwrap();
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    let dest = PathBuf::from(&created.placement.host_path);
    assert_eq!(git(&dest, &["ls-files", "ignored.txt"]), "ignored.txt");
    let sealed = seal_writer(&fx, &mut docs, "ws-ignored", "req-writer");
    let tree = sealed.workspace.seal_hash.as_deref().unwrap();
    assert_eq!(tree, git(&fx.repo, &["rev-parse", "HEAD^{tree}"]));
    assert_eq!(
        git(&fx.repo, &["show", &format!("{tree}:ignored.txt")]),
        "tracked despite ignore"
    );
}

#[test]
fn unicode_dirty_trunk_overlap_preserves_operator_bytes_index_and_head() {
    let mut fx = Fixture::new();
    fx.commit("café.rs", "base\n");
    git(&fx.repo, &["config", "core.quotePath", "true"]);
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("ws-unicode", "unit", "topic-unicode");
    action.path_capability = WorkspacePathCapability::exact_paths(vec!["café.rs".into()]).unwrap();
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    fs::write(
        Path::new(&created.placement.host_path).join("café.rs"),
        "worker\n",
    )
    .unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-unicode", "req-writer");
    bind_integrate(
        &mut docs,
        "ws-unicode",
        "req-int",
        sealed.workspace.seal_hash.as_deref().unwrap(),
    );
    fs::write(fx.repo.join("café.rs"), "operator staged\n").unwrap();
    git(&fx.repo, &["add", "café.rs"]);
    fs::write(fx.repo.join("café.rs"), "operator unstaged\n").unwrap();
    let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let before_index = git(&fx.repo, &["write-tree"]);
    let error = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-unicode".into(),
            produced_by_request_id: "req-int".into(),
            produced_by_request_doc_id: "req-int-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect_err("quoted Unicode path must still overlap the admitted literal path");
    assert!(error.to_string().contains("overlapping"), "{error}");
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(git(&fx.repo, &["write-tree"]), before_index);
    assert_eq!(
        fs::read_to_string(fx.repo.join("café.rs")).unwrap(),
        "operator unstaged\n"
    );
}

#[test]
fn empty_seal_rejects_foreign_pending_commit_and_replays_without_effect() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut action = fx.action("ws-empty-pending", "unit", "topic-empty-pending");
    action.path_capability = WorkspacePathCapability::exact_paths(Vec::new()).unwrap();
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(action),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    let dest = PathBuf::from(&created.placement.host_path);
    let sealed = seal_writer(&fx, &mut docs, "ws-empty-pending", "req-writer");
    let seal = sealed.workspace.seal_hash.as_deref().unwrap();
    bind_integrate(&mut docs, "ws-empty-pending", "req-int", seal);
    // A real commit object changes an unowned path, but neither branch moves.
    fs::write(dest.join("README.md"), "unowned effect\n").unwrap();
    git(&dest, &["add", "README.md"]);
    let foreign_tree = git(&dest, &["write-tree"]);
    let foreign_commit = git(
        &dest,
        &[
            "commit-tree",
            &foreign_tree,
            "-p",
            &fx.base_sha,
            "-m",
            "foreign pending effect",
        ],
    );
    git(&dest, &["checkout", &fx.base_sha, "--", "README.md"]);
    let git_dir = PathBuf::from(git(&fx.repo, &["rev-parse", "--absolute-git-dir"]));
    let marker = git_dir.join("gents-integrate-ws-empty-pending.json");
    fs::write(
        &marker,
        serde_json::to_vec(&serde_json::json!({
            "journal": [], "pending_head_sha": foreign_commit,
            "seal_hash": seal, "request_id": "req-int"
        }))
        .unwrap(),
    )
    .unwrap();
    let plan = emit_integrate_workspace_plan(IntegrateWorkspaceAction {
        workspace_id: "ws-empty-pending".into(),
        produced_by_request_id: "req-int".into(),
        produced_by_request_doc_id: "req-int-doc".into(),
        mode: IntegrateMode::ApplyDiff,
    });
    let before_receipts = docs.receipts.clone();
    let before_index = git(&fx.repo, &["write-tree"]);
    let error = execute_integrate_workspace_plan(
        &plan,
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .expect_err("an empty grant must not authorize a journal's unrelated pending commit");
    assert!(
        error.to_string().contains("empty integration receipt"),
        "{error}"
    );
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), fx.base_sha);
    assert_eq!(git(&fx.repo, &["write-tree"]), before_index);
    assert_eq!(
        fs::read_to_string(fx.repo.join("README.md")).unwrap(),
        "hello\n"
    );
    assert_eq!(docs.receipts, before_receipts);

    fs::remove_file(marker).unwrap();
    let first = execute_integrate_workspace_plan(
        &plan,
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .unwrap();
    assert!(first.pending_head_sha.is_none());
    let receipts = docs.receipts.clone();
    fs::remove_dir_all(&dest).unwrap();
    let replay = execute_integrate_workspace_plan(
        &plan,
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .unwrap();
    assert!(replay.pending_head_sha.is_none());
    assert_eq!(docs.receipts, receipts);
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), fx.base_sha);
    assert!(!dest.exists());
}
