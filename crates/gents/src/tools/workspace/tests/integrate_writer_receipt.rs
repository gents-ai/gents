use super::*;

#[test]
fn integration_requires_a_writer_receipt_matching_the_sealed_identity() {
    for case in [
        "valid",
        "missing",
        "workspace",
        "base",
        "capability",
        "seal",
        "kind",
        "receipt_id",
        "empty_request",
        "empty_doc",
    ] {
        let fx = Fixture::new();
        let mut docs = MemoryWorkspaceDocuments::default();
        let created = execute_create_workspace_plan(
            &emit_create_workspace_plan(fx.action("ws-witness", "unit-1", "topic-witness")),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, git_worktree_caps()),
        )
        .unwrap();
        fs::write(
            PathBuf::from(&created.placement.host_path).join("patch.rs"),
            "fn proof() {}\n",
        )
        .unwrap();
        let sealed = seal_writer(&fx, &mut docs, "ws-witness", "writer");
        let hash = sealed.workspace.seal_hash.clone().unwrap();
        bind_integrate(&mut docs, "ws-witness", "integrator", &hash);
        let mut witness = docs.receipts.remove(&sealed.receipt.receipt_id).unwrap();
        match case {
            "valid" | "missing" => {}
            "workspace" => witness.workspace_id = "another-workspace".into(),
            "base" => witness.base_sha = "different-base".into(),
            "capability" => witness.path_capability_digest = "different-capability".into(),
            "seal" => witness.seal_hash = "different-tree".into(),
            "kind" => witness.kind = "integrator".into(),
            "receipt_id" => witness.receipt_id = "noncanonical-writer-receipt".into(),
            "empty_request" => {
                witness.produced_by_request_id.clear();
                witness.receipt_id = super::super::documents::writer_receipt_id("ws-witness", "");
            }
            "empty_doc" => witness.produced_by_request_doc_id.clear(),
            _ => unreachable!(),
        }
        if case != "missing" {
            docs.write_receipt(witness).unwrap();
        }
        let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
        let before_index = git(&fx.repo, &["write-tree"]);
        let before_receipts = docs.receipts.clone();
        let result = execute_integrate_workspace_plan(
            &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
                workspace_id: "ws-witness".into(),
                produced_by_request_id: "integrator".into(),
                produced_by_request_doc_id: "integrator-doc".into(),
                mode: IntegrateMode::ApplyDiff,
            }),
            &mut Vec::new(),
            &mut fx.ctx(&mut docs, integrate_caps()),
        );
        if case == "valid" {
            let integrated = result.unwrap();
            assert_eq!(integrated.receipt.kind, "integrator");
            finalize_integrate_trunk(&fx.repo, integrated.pending_head_sha.as_deref()).unwrap();
            assert!(fx.repo.join("patch.rs").is_file());
        } else {
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains("matching writer receipt"),
                "{case}: {error}"
            );
            assert_eq!(
                docs.receipts, before_receipts,
                "{case}: denial created a receipt"
            );
            assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before_head, "{case}");
            assert_eq!(git(&fx.repo, &["write-tree"]), before_index, "{case}");
            assert!(!fx.repo.join("patch.rs").exists(), "{case}");
        }
    }
}

#[test]
fn matching_integrator_receipt_replays_without_a_writer_receipt() {
    let fx = Fixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(fx.action(
            "ws-replay-witness",
            "unit-1",
            "topic-replay-witness",
        )),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, git_worktree_caps()),
    )
    .unwrap();
    fs::write(
        PathBuf::from(&created.placement.host_path).join("patch.rs"),
        "fn replay() {}\n",
    )
    .unwrap();
    let sealed = seal_writer(&fx, &mut docs, "ws-replay-witness", "writer");
    bind_integrate(
        &mut docs,
        "ws-replay-witness",
        "integrator",
        sealed.workspace.seal_hash.as_deref().unwrap(),
    );
    let first = integrate_writer(&fx, &mut docs, "ws-replay-witness", "integrator");
    docs.receipts.remove(&sealed.receipt.receipt_id);
    let head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let second = execute_integrate_workspace_plan(
        &emit_integrate_workspace_plan(IntegrateWorkspaceAction {
            workspace_id: "ws-replay-witness".into(),
            produced_by_request_id: "integrator".into(),
            produced_by_request_doc_id: "integrator-doc".into(),
            mode: IntegrateMode::ApplyDiff,
        }),
        &mut Vec::new(),
        &mut fx.ctx(&mut docs, integrate_caps()),
    )
    .unwrap();
    assert_eq!(second.receipt, first.receipt);
    assert!(second.pending_head_sha.is_none());
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), head);
}
