use std::fs;
use std::path::PathBuf;

use gents_loop::tool_policy::{CommandExecutionMode, CommandNetworkMode, WorkspaceAuthority};

use crate::tools::native::{
    apply_workspace_authority, validate_command_policy, CommandConstraints, CommandExecutionPolicy,
};
use crate::workspace::{
    admit_workspace_binding, bind_integrate, emit_create_workspace_plan,
    execute_create_workspace_plan, git, git_worktree_caps, integrate_writer, new_binding,
    seal_writer, workspace_integrate_docs_mutation, AdmitBinding, Fixture,
    MemoryWorkspaceDocuments, WorkspaceDocuments,
};

#[test]
fn read_write_never_meets_to_unrestricted() {
    for behavior in [
        CommandExecutionMode::ReadOnly,
        CommandExecutionMode::WorkspaceWrite,
        CommandExecutionMode::Unrestricted,
    ] {
        let policy = CommandExecutionPolicy::write_capable().with_mode(behavior);
        let met = apply_workspace_authority(&policy, WorkspaceAuthority::ReadWrite);
        assert_ne!(met.mode, CommandExecutionMode::Unrestricted);
        assert_eq!(
            met.mode,
            behavior.meet(CommandExecutionMode::WorkspaceWrite)
        );
    }
}

fn git_worktree_diff_policy() -> CommandExecutionPolicy {
    CommandExecutionPolicy::write_capable()
        .with_mode(CommandExecutionMode::WorkspaceWrite)
        .with_git_worktree_diff()
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

    let first = admit_workspace_binding(
        "ws-int",
        "sealed",
        Some(&hash),
        &docs.load_bindings("ws-int").unwrap(),
        new_binding(
            "ws-int",
            "req-review-a",
            "doc-a",
            gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            "did:key:zWorkspaceOwner",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let AdmitBinding::Create { binding: a, .. } = first else {
        panic!("expected readonly create");
    };
    docs.write_binding(a.clone()).unwrap();
    let second = admit_workspace_binding(
        "ws-int",
        "sealed",
        Some(&hash),
        &docs.load_bindings("ws-int").unwrap(),
        new_binding(
            "ws-int",
            "req-review-b",
            "doc-b",
            gents_loop::tool_policy::WorkspaceAuthority::ReadOnly,
            "did:key:zWorkspaceOwner",
            Some(&hash),
        ),
        false,
    )
    .unwrap();
    let AdmitBinding::Create { binding: b, .. } = second else {
        panic!("expected concurrent readonly");
    };
    docs.write_binding(b.clone()).unwrap();
    assert!(a.is_active());
    assert!(b.is_active());

    let integrated = integrate_writer(&fx, &mut docs, "ws-int", "req-int");
    let mutation = workspace_integrate_docs_mutation(&[], &integrated.receipt);
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
