use std::path::PathBuf;

use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::tool_surface::FileToolMode;
use crate::toolset::{
    apply_workspace_authority, CommandExecutionMode, CommandExecutionPolicy, WorkspaceAuthority,
};

use super::{
    bind_workspace_overlay, workspace_authority_file_mode, IsolatedWorkspaceRecord,
    WorkspaceBindInput, WorkspacePlacementRecord,
};

fn temp_tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let placement = root.path().join("worktrees").join("ws-1");
    std::fs::create_dir_all(&placement).unwrap();
    let canonical_root = std::fs::canonicalize(root.path()).unwrap();
    let canonical_placement = std::fs::canonicalize(&placement).unwrap();
    (root, canonical_root, canonical_placement)
}

fn ready_workspace() -> IsolatedWorkspaceRecord {
    IsolatedWorkspaceRecord {
        workspace_id: "ws-1".into(),
        owner_deployment_id: "dep-1".into(),
        writer_principal: "did:key:zWriter".into(),
        integrator_principal: "did:key:zIntegrator".into(),
        lifecycle_state: "ready".into(),
        seal_hash: None,
        instruction_manifest: "{}".into(),
    }
}

fn placement(host_path: &std::path::Path) -> WorkspacePlacementRecord {
    WorkspacePlacementRecord {
        workspace_id: "ws-1".into(),
        deployment_id: "dep-1".into(),
        host_path: host_path.to_string_lossy().into_owned(),
        observed_tree_hash: None,
    }
}

fn bind_input<'a>(
    authority: WorkspaceAuthority,
    operator_tool_root: Option<&'a std::path::Path>,
    enabled_workspace_roots: &'a [PathBuf],
    enforced: bool,
) -> WorkspaceBindInput<'a> {
    WorkspaceBindInput {
        workspace_id: "ws-1",
        authority,
        owner_deployment_id: "dep-1",
        seal_hash: None,
        request_cwd: None,
        local_deployment_id: "dep-1",
        operator_tool_root,
        enabled_workspace_roots,
        workspace_write_sandbox_enforced: enforced,
        live_tree_hash: None,
    }
}

#[test]
fn read_write_meets_unrestricted_to_workspace_write() {
    let policy =
        CommandExecutionPolicy::write_capable().with_mode(CommandExecutionMode::Unrestricted);
    let met = apply_workspace_authority(&policy, WorkspaceAuthority::ReadWrite);
    assert_eq!(met.mode, CommandExecutionMode::WorkspaceWrite);
    assert!(met.deny_git_metadata_writes());
}

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

#[test]
fn read_only_and_integrate_meet_command_mode_to_read_only() {
    let policy =
        CommandExecutionPolicy::write_capable().with_mode(CommandExecutionMode::Unrestricted);
    assert_eq!(
        apply_workspace_authority(&policy, WorkspaceAuthority::ReadOnly).mode,
        CommandExecutionMode::ReadOnly
    );
    assert_eq!(
        apply_workspace_authority(&policy, WorkspaceAuthority::Integrate).mode,
        CommandExecutionMode::ReadOnly
    );
}

#[test]
fn authority_file_mode_matches_spec() {
    assert_eq!(
        workspace_authority_file_mode(WorkspaceAuthority::ReadWrite),
        FileToolMode::ReadWrite
    );
    assert_eq!(
        FileToolMode::ReadWrite.meet(workspace_authority_file_mode(WorkspaceAuthority::ReadOnly)),
        FileToolMode::ReadOnly
    );
    assert_eq!(
        FileToolMode::ReadWrite.meet(workspace_authority_file_mode(WorkspaceAuthority::Integrate)),
        FileToolMode::ReadOnly
    );
}

#[test]
fn read_write_binds_ready_placement_under_operator_root() {
    let (_guard, operator, placement_path) = temp_tree();
    let overlay = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadWrite, Some(&operator), &[], true),
    )
    .unwrap();
    assert_eq!(overlay.root, placement_path);
    assert_eq!(overlay.cwd, placement_path);
    assert_eq!(overlay.authority, WorkspaceAuthority::ReadWrite);
}

#[test]
fn read_write_fails_closed_without_workspace_write_sandbox() {
    let (_guard, operator, placement_path) = temp_tree();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadWrite, Some(&operator), &[], false),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("enforceable WorkspaceWrite sandbox"),
        "{error:#}"
    );
}

#[test]
fn read_write_rejects_sealed_workspace() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let error = bind_workspace_overlay(
        &workspace,
        &placement(&placement_path),
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::ReadWrite, Some(&operator), &[], true)
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("not bindable for authority"),
        "{error:#}"
    );
}

#[test]
fn read_only_binds_ready_and_sealed() {
    let (_guard, operator, placement_path) = temp_tree();
    bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false),
    )
    .unwrap();

    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let mut placed = placement(&placement_path);
    placed.observed_tree_hash = Some("hash-1".into());
    bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            live_tree_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap();
}

#[test]
fn integrate_only_binds_sealed_with_matching_hash() {
    let (_guard, operator, placement_path) = temp_tree();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::Integrate, Some(&operator), &[], false),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not bindable"), "{error:#}");

    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let mut placed = placement(&placement_path);
    placed.observed_tree_hash = Some("hash-1".into());
    bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            live_tree_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::Integrate, Some(&operator), &[], false)
        },
    )
    .unwrap();
}

#[test]
fn sealed_mismatch_and_missing_hash_fail_closed() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let mut hashed = placement(&placement_path);
    hashed.observed_tree_hash = Some("hash-1".into());
    let error = bind_workspace_overlay(
        &workspace,
        &hashed,
        WorkspaceBindInput {
            seal_hash: Some("hash-other"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match workspace seal_hash"),
        "{error:#}"
    );

    let mut drifted = placement(&placement_path);
    drifted.observed_tree_hash = Some("drifted".into());
    let error = bind_workspace_overlay(
        &workspace,
        &drifted,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("observed_tree_hash drifted does not match"),
        "{error:#}"
    );
}

#[test]
fn sealed_requires_live_tree_hash() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let mut placed = placement(&placement_path);
    placed.observed_tree_hash = Some("hash-1".into());
    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            live_tree_hash: None,
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("requires live tree hash"),
        "{error:#}"
    );
}

#[test]
fn live_tree_hash_drift_fails_closed() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let mut placed = placement(&placement_path);
    placed.observed_tree_hash = Some("hash-1".into());
    let overlay = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            live_tree_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap();
    assert_eq!(overlay.seal_hash.as_deref(), Some("hash-1"));

    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            live_tree_hash: Some("drifted"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("live tree hash drifted does not match"),
        "{error:#}"
    );
}

#[test]
fn request_lifecycle_treats_input_required_live_and_terminals_not_live() {
    assert!(super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Processing
    )));
    assert!(super::request_lifecycle_is_live(Some(
        RequestLifecycleState::InputRequired
    )));
    assert!(super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Claimed
    )));
    assert!(!super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Completed
    )));
    assert!(!super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Failed
    )));
    assert!(!super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Dead
    )));
    assert!(!super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Interrupted
    )));
    assert!(!super::request_lifecycle_is_live(Some(
        RequestLifecycleState::Superseded
    )));
    assert!(!super::request_lifecycle_is_live(None));
}

#[test]
fn frozen_instruction_manifest_is_copied_onto_overlay() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.instruction_manifest = r#"{"schema":1,"base_sha":"abc","files":[]}"#.into();
    let overlay = bind_workspace_overlay(
        &workspace,
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false),
    )
    .unwrap();
    assert!(overlay.instruction_manifest.contains("base_sha"));
}

#[test]
fn empty_bound_overlay_does_not_live_walk_writer_tree() {
    let root = tempfile::tempdir().unwrap();
    let operator = std::fs::canonicalize(root.path()).unwrap();
    let placement_path = operator.join("worktrees").join("ws-1");
    let nested = placement_path.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        placement_path.join("AGENTS.md"),
        "live-writer-instructions\n",
    )
    .unwrap();
    std::fs::write(nested.join("AGENTS.md"), "nested-live-instructions\n").unwrap();
    let placement_path = std::fs::canonicalize(&placement_path).unwrap();
    let nested = std::fs::canonicalize(&nested).unwrap();

    let overlay = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false),
    )
    .unwrap();
    let frozen = super::frozen_instruction_manifest_from_overlay(Some(&overlay));
    assert_eq!(frozen, Some("{}"));
    assert!(crate::workspace::instruction_body_for_request(
        frozen,
        Some(&nested),
        Some(&placement_path)
    )
    .is_none());

    let live = crate::workspace::instruction_body_for_request(
        super::frozen_instruction_manifest_from_overlay(None),
        Some(&nested),
        Some(&placement_path),
    )
    .unwrap();
    assert!(live.contains("live-writer-instructions"));
    assert!(live.contains("nested-live-instructions"));
}

#[test]
fn sealed_missing_observed_tree_hash_fails_closed() {
    let (_guard, operator, placement_path) = temp_tree();
    let mut workspace = ready_workspace();
    workspace.lifecycle_state = "sealed".into();
    workspace.seal_hash = Some("hash-1".into());
    let error = bind_workspace_overlay(
        &workspace,
        &placement(&placement_path),
        WorkspaceBindInput {
            seal_hash: Some("hash-1"),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("missing placement observed_tree_hash"),
        "{error:#}"
    );
}

#[test]
fn placement_outside_operator_root_fails_closed() {
    let (_guard, operator, _) = temp_tree();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = std::fs::canonicalize(outside.path()).unwrap();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&outside_path),
        bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("escapes operator tool root"),
        "{error:#}"
    );
}

#[test]
fn missing_ceiling_fails_closed() {
    let (_guard, _, placement_path) = temp_tree();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(WorkspaceAuthority::ReadOnly, None, &[], false),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("operator tool-root or enabled WorkspaceRoot"),
        "{error:#}"
    );
}

#[test]
fn enabled_workspace_root_allowlist_is_required_when_present() {
    let (_guard, operator, placement_path) = temp_tree();
    let other = tempfile::tempdir().unwrap();
    let other_root = std::fs::canonicalize(other.path()).unwrap();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        bind_input(
            WorkspaceAuthority::ReadOnly,
            Some(&operator),
            &[other_root],
            false,
        ),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not under an enabled WorkspaceRoot"),
        "{error:#}"
    );
}

#[test]
fn persisted_cwd_must_stay_under_placement() {
    let (_guard, operator, placement_path) = temp_tree();
    let nested = placement_path.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    let overlay = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        WorkspaceBindInput {
            request_cwd: Some(&nested),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap();
    assert_eq!(overlay.cwd, std::fs::canonicalize(&nested).unwrap());

    let outside = operator.join("other");
    std::fs::create_dir_all(&outside).unwrap();
    let error = bind_workspace_overlay(
        &ready_workspace(),
        &placement(&placement_path),
        WorkspaceBindInput {
            request_cwd: Some(&outside),
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not a directory under workspace root"),
        "{error:#}"
    );
}

#[test]
fn workspace_authority_parse_and_write_flags() {
    assert!(WorkspaceAuthority::parse("readWrite").is_ok());
    assert!(WorkspaceAuthority::ReadWrite.allows_file_writes());
    assert!(!WorkspaceAuthority::ReadOnly.allows_file_writes());
    assert!(!WorkspaceAuthority::Integrate.allows_file_writes());
    assert_eq!(
        WorkspaceAuthority::ReadOnly.infimum(WorkspaceAuthority::ReadWrite),
        WorkspaceAuthority::ReadOnly
    );
}

#[test]
fn write_and_integrate_authority_require_the_configured_principal() {
    let workspace = ready_workspace();
    assert!(super::require_workspace_principal(
        &workspace,
        "did:key:zWriter",
        WorkspaceAuthority::ReadWrite
    )
    .is_ok());
    assert!(super::require_workspace_principal(
        &workspace,
        "did:key:zOther",
        WorkspaceAuthority::ReadWrite
    )
    .is_err());
    assert!(super::require_workspace_principal(
        &workspace,
        "did:key:zIntegrator",
        WorkspaceAuthority::Integrate
    )
    .is_ok());
    assert!(super::require_workspace_principal(
        &workspace,
        "did:key:zOther",
        WorkspaceAuthority::ReadOnly
    )
    .is_ok());
}

#[test]
fn blank_workspace_id_is_unbound() {
    assert!(super::optional_id(None).is_none());
    assert!(super::optional_id(Some("")).is_none());
    assert!(super::optional_id(Some("  ")).is_none());
    assert_eq!(super::optional_id(Some("ws-1")), Some("ws-1"));
}

#[test]
fn identity_mismatches_fail_closed() {
    let (_guard, operator, placement_path) = temp_tree();
    let workspace = ready_workspace();
    let placed = placement(&placement_path);

    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            local_deployment_id: "dep-other",
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("not this host dep-other"),
        "{error:#}"
    );

    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            owner_deployment_id: "dep-other",
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("does not match workspace owner"),
        "{error:#}"
    );

    let mut foreign = placed.clone();
    foreign.deployment_id = "dep-other".into();
    let error = bind_workspace_overlay(
        &workspace,
        &foreign,
        bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match owner_deployment_id"),
        "{error:#}"
    );

    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            owner_deployment_id: "",
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("workspace_owner_deployment_id is missing"),
        "{error:#}"
    );

    let error = bind_workspace_overlay(
        &workspace,
        &placed,
        WorkspaceBindInput {
            local_deployment_id: "",
            ..bind_input(WorkspaceAuthority::ReadOnly, Some(&operator), &[], false)
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("HostDeployment.deployment_id is missing"),
        "{error:#}"
    );
}

#[test]
fn missing_or_ambiguous_host_deployment_fails_closed() {
    let error = super::local_deployment_id_from_rows(Vec::new()).unwrap_err();
    assert!(
        error.to_string().contains("HostDeployment is missing"),
        "{error:#}"
    );
    let error = super::local_deployment_id_from_rows(vec![
        super::HostDeploymentRow {
            deployment_id: Some("dep-1".into()),
        },
        super::HostDeploymentRow {
            deployment_id: Some("dep-2".into()),
        },
    ])
    .unwrap_err();
    assert!(error.to_string().contains("ambiguous"), "{error:#}");
    let id = super::local_deployment_id_from_rows(vec![super::HostDeploymentRow {
        deployment_id: Some("dep-1".into()),
    }])
    .unwrap();
    assert_eq!(id, "dep-1");
}

/// Actual Git workspace, runtime schemas, signed request and held execution lease.
/// Source files are authored before sealing; consumers must not mutate source.
pub(crate) struct ArtifactTestFixture {
    pub grant: crate::workspace::ArtifactGrant,
    pub owner: crate::lifecycle::RequestLifecycle,
    pub node: std::sync::Arc<defra_node::EmbeddedNode>,
    pub _dir: tempfile::TempDir,
}

pub(crate) async fn artifact_test_fixture(files: &[(&str, &str)]) -> ArtifactTestFixture {
    use crate::identity::AgentIdentity;
    use crate::workspace::*;
    use std::{collections::BTreeSet, sync::Arc};
    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    async fn execute(node: &defra_node::EmbeddedNode, query: &str) {
        let result = node.execute(query).await;
        assert!(!result.has_errors(), "{query}: {:?}", result.errors);
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "artifact@example.com"]);
    git(&repo, &["config", "user.name", "Artifact Test"]);
    std::fs::write(repo.join("README.md"), "sealed source\n").unwrap();
    for (path, contents) in files {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "fixture"]);
    let identity =
        crate::identity::KeyIdentity::load_or_create(root.join("agent.key"), None).unwrap();
    let did = identity.did().to_owned();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(root.join("db"))
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let mut documents = MemoryWorkspaceDocuments::default();
    let mut context = HostExecutorContext {
        deployment_id: "artifact-deployment".into(),
        repository: RepositoryPlacementRef {
            repository_id: "artifact-repo".into(),
            deployment_id: "artifact-deployment".into(),
            host_path: repo.clone(),
            enabled: true,
        },
        ceiling: Some(&repo),
        capabilities: BTreeSet::from([
            CAP_CREATE_WORKSPACE.to_owned(),
            CAP_OBSERVE_DIRTY_BASE.to_owned(),
            CAP_SEAL_WORKSPACE.to_owned(),
        ]),
        writer_principal: did.clone(),
        integrator_principal: did.clone(),
        caused_by_invocation_id: "artifact-invocation".into(),
        caused_by_correlation: "artifact-correlation".into(),
        documents: &mut documents,
    };
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(CreateWorkspaceAction {
            path_capability: WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
            workspace_id: "artifact-workspace".into(),
            work_unit_id: "artifact-unit".into(),
            repository_id: "artifact-repo".into(),
            base_sha: git(&repo, &["rev-parse", "HEAD"]),
            branch: "artifact-review".into(),
            creation_policy: CreationPolicy::GitWorktreeDiff,
            adapter: WorkspaceAdapterKind::GitWorktree,
            clone_artifacts: None,
        }),
        &mut Vec::new(),
        &mut context,
    )
    .unwrap();
    let sealed = execute_seal_workspace_plan(
        &emit_seal_workspace_plan(SealWorkspaceAction {
            workspace_id: "artifact-workspace".into(),
            produced_by_request_id: "artifact-writer".into(),
            produced_by_request_doc_id: "artifact-writer-doc".into(),
        }),
        &mut Vec::new(),
        &mut context,
    )
    .unwrap();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut placement = created.placement;
    placement.observed_tree_hash = sealed.workspace.seal_hash.clone().unwrap();
    execute(
        &node,
        &isolated_workspace_upsert_mutation(&sealed.workspace),
    )
    .await;
    execute(
        &node,
        &workspace_placement_upsert_mutation(&placement, &now),
    )
    .await;
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        "artifact-reader",
        &did,
        &did,
        "general",
        "artifact-session",
        "Review sealed source",
        "interactive",
        &now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
    );
    create.workspace_id = Some("artifact-workspace".into());
    create.workspace_authority = Some("readOnly".into());
    create.workspace_owner_deployment_id = Some("artifact-deployment".into());
    create.workspace_seal_hash = sealed.workspace.seal_hash.clone();
    crate::request_admission::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    execute(&node, &create.graphql_mutation().unwrap()).await;
    let result = node
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&result, "AgentRequest")
            .unwrap()
            .unwrap();
    let request = crate::watcher::AgentRequest::try_from(row).unwrap();
    let mut owner = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        &did,
        request,
        300,
    );
    owner.claim().await.unwrap();
    let workspace = super::load_isolated_workspace_record(&node, "artifact-workspace")
        .await
        .unwrap()
        .unwrap();
    super::ensure_request_binding(
        &node,
        owner.request(),
        &workspace,
        "artifact-deployment",
        WorkspaceAuthority::ReadOnly,
    )
    .await
    .unwrap();
    let grant = crate::workspace::ArtifactGrant::create(
        node.clone(),
        owner.request(),
        owner.execution_generation().unwrap(),
        std::path::Path::new(&placement.host_path),
        "artifact-deployment",
        sealed.workspace.seal_hash.as_deref().unwrap(),
    )
    .await
    .unwrap();
    ArtifactTestFixture {
        grant,
        owner,
        node,
        _dir: dir,
    }
}

#[tokio::test]
async fn artifact_grant_fences_current_lease_and_source_seal() {
    let fx = artifact_test_fixture(&[]).await;
    fx.grant.validate_for_launch().await.unwrap();
    assert!(!fx.grant.root().starts_with(fx.grant.source_root()));
    assert!(fx.grant.root().join("target").is_dir());
    assert!(fx.grant.root().join("tmp").is_dir());
    let doc = crate::graphql::escape_graphql_string(fx.grant.request_doc_id());
    let result = fx.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, input: {{ execution_lease_expires_at: "2000-01-01T00:00:00Z" }}) {{ _docID }} }}"#)).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert!(fx
        .grant
        .validate_for_launch()
        .await
        .unwrap_err()
        .to_string()
        .contains("expired"));
}

#[cfg(unix)]
#[tokio::test]
async fn artifact_grant_rejects_replaced_output_directory() {
    let fx = artifact_test_fixture(&[]).await;
    let target = fx.grant.root().join("target");
    std::fs::remove_dir(&target).unwrap();
    std::os::unix::fs::symlink(fx.grant.source_root(), &target).unwrap();
    assert!(fx.grant.validate_for_launch().await.is_err());
}

#[tokio::test]
async fn artifact_grant_rechecks_binding_generation_and_sealed_bytes() {
    let fx = artifact_test_fixture(&[]).await;
    let doc = crate::graphql::escape_graphql_string(fx.grant.request_doc_id());
    let generation = crate::graphql::escape_graphql_string(fx.grant.execution_generation());
    for (input, restore) in [
        (
            "execution_generation: \"foreign-owner\"".to_owned(),
            format!("execution_generation: \"{generation}\""),
        ),
        (
            "lifecycle_state: \"interrupted\"".to_owned(),
            "lifecycle_state: \"claimed\"".to_owned(),
        ),
    ] {
        for (fields, should_allow) in [(input, false), (restore, true)] {
            let response = fx.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#)).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            assert_eq!(fx.grant.validate_for_launch().await.is_ok(), should_allow);
        }
    }
    // Fixture-only lifecycle resets above isolate each launch guard; production
    // terminal ownership is tested by the execution lease suite, not this seeder.
    let response = fx.node.execute(&format!(r#"mutation {{ update_WorkspaceBinding(filter: {{ request_doc_id: {{ _eq: "{doc}" }} }}, input: {{ lifecycle_state: "released" }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(fx
        .grant
        .validate_for_launch()
        .await
        .unwrap_err()
        .to_string()
        .contains("active ReadOnly binding"));
}

#[tokio::test]
async fn artifact_grant_rejects_source_drift_and_allocates_disjoint_outputs() {
    let fx = artifact_test_fixture(&[]).await;
    let second = crate::workspace::ArtifactGrant::create(
        fx.node.clone(),
        fx.owner.request(),
        fx.owner.execution_generation().unwrap(),
        fx.grant.source_root(),
        "artifact-deployment",
        fx.owner.request().workspace_seal_hash.as_deref().unwrap(),
    )
    .await
    .unwrap();
    assert_ne!(fx.grant.root(), second.root());
    assert_eq!(fx.grant.source_root(), second.source_root());
    std::fs::write(fx.grant.source_root().join("README.md"), "changed source\n").unwrap();
    assert!(fx
        .grant
        .validate_for_launch()
        .await
        .unwrap_err()
        .to_string()
        .contains("seal"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn generated_artifact_admission_cases_drive_live_binding_and_launch_policy() {
    use crate::tool_call_lifecycle::runtime::{
        scope_request_tool_execution_with_workspace_overlay, ToolWorkspaceScope,
    };
    use crate::toolset::{CommandConstraints, CommandNetworkMode};

    async fn execute(node: &defra_node::EmbeddedNode, mutation: &str) {
        let result = node.execute(mutation).await;
        assert!(!result.has_errors(), "{mutation}: {:?}", result.errors);
    }

    let cases = &crate::lean_vocab_test::lean_contract_snapshot().artifact_admission_cases;
    assert_eq!(cases.len(), 13);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let binding = &case["binding"];
        let mode = CommandExecutionMode::parse(case["mode"].as_str().unwrap()).unwrap();
        let fx = artifact_test_fixture(&[]).await;
        if name == "unsupported_platform" {
            // Exercise the actual selector's host observation input; this is not
            // an assertion that the current macOS host lacks Seatbelt.
            assert_eq!(case["expected_admitted"], false);
            assert!(case["expected_bound_mode"].is_null());
            assert!(crate::toolset::select_sandbox_for_policy(mode, false).is_err());
            continue;
        }
        execute(&fx.node, r#"mutation { create_HostDeployment(input: { deployment_id: "artifact-deployment", display_name: "local", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z" }) { _docID } }"#).await;
        let alternate = artifact_alternate_owner(&fx, name, binding).await;
        let (request_owner, claim_outcome) = alternate.unwrap();
        if !matches!(claim_outcome, crate::lifecycle::ClaimOutcome::Claimed) {
            assert_eq!(
                name, "wrong_owner",
                "unexpected preclaim denial: {claim_outcome:?}"
            );
            assert_eq!(
                case["expected_admitted"], false,
                "{name}: {claim_outcome:?}"
            );
            assert!(
                case["expected_bound_mode"].is_null(),
                "{name}: {claim_outcome:?}"
            );
        }
        if name == "unsealed" {
            execute(&fx.node, r#"mutation { update_IsolatedWorkspace(filter: { workspace_id: { _eq: "artifact-workspace" } }, input: { lifecycle_state: "ready" }) { _docID } }"#).await;
        }
        let generation = if name == "stale_incarnation" {
            "stale-execution"
        } else {
            // An intentionally rejected owner remains unclaimed; do not forge
            // an execution tuple merely to reach the later launch guard.
            request_owner.execution_generation().unwrap_or("")
        };
        let resolved = super::resolve_request_workspace_overlay(
            &fx.node,
            request_owner.request(),
            generation,
            mode == CommandExecutionMode::ArtifactWrite,
            Some(fx._dir.path()),
        )
        .await;
        // Resolver failure is evidence of invalid context, not by itself proof
        // that execution fails: always drive actual command launch preparation.
        if matches!(
            name,
            "missing_binding"
                | "integrate_not_artifact"
                | "readwrite_not_artifact"
                | "unsealed"
                | "wrong_seal"
                | "stale_incarnation"
        ) {
            assert!(
                resolved.is_err(),
                "{name}: invalid artifact context must stop at resolver, before tools"
            );
        }
        let overlay = resolved.ok().flatten();
        let authority = overlay.as_ref().map(|o| o.authority).or_else(|| {
            binding["authority"]
                .as_str()
                .map(|value| WorkspaceAuthority::parse(value).unwrap())
        });
        let grant = overlay.as_ref().and_then(|o| o.workspace_artifact.clone());
        if name == "foreign_root" {
            let target = grant
                .as_ref()
                .expect("valid resolver before root replacement")
                .root()
                .join("target");
            std::fs::remove_dir(&target).unwrap();
            std::os::unix::fs::symlink(fx.grant.source_root(), &target).unwrap();
        }
        let requested = CommandExecutionPolicy::write_capable().with_mode(mode);
        let effective = if mode == CommandExecutionMode::ArtifactWrite {
            requested
        } else {
            apply_workspace_authority(&requested, authority.expect("ordinary bound case"))
        };
        let root = fx.grant.source_root().to_owned();
        let constraints = CommandConstraints {
            allowed_argv_prefixes: vec![vec!["/bin/echo".into()]],
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Disabled,
            execution_mode: effective.mode,
            sandbox: effective.mode,
            deny_all_argv: false,
            deny_git_metadata_writes: true,
        };
        let result = scope_request_tool_execution_with_workspace_overlay(
            None,
            tokio_util::sync::CancellationToken::new(),
            ToolWorkspaceScope {
                workspace_cwd: Some(root.clone()),
                workspace_root: Some(root.clone()),
                workspace_authority: authority,
                workspace_artifact: grant,
            },
            None,
            None,
            None,
            Default::default(),
            false,
            crate::toolset::prepare_managed_command(&root, "/bin/echo", &[], &constraints),
        )
        .await;
        let observed_mode = result.as_ref().ok().map(|_| effective.mode.as_str());
        assert_eq!(
            observed_mode,
            case["expected_bound_mode"].as_str(),
            "{name}: {result:?}"
        );
        assert_eq!(
            observed_mode == Some("artifact_write"),
            case["expected_admitted"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "sealed source\n"
        );
    }
}

#[tokio::test]
async fn existing_workspace_cleanup_removes_artifacts_without_grant_drop_authority() {
    let ArtifactTestFixture {
        grant,
        mut owner,
        node,
        _dir,
    } = artifact_test_fixture(&[]).await;
    let artifact_root = grant.root().to_owned();
    let source = grant.source_root().to_owned();
    let git_dir = artifact_root.parent().unwrap().parent().unwrap().to_owned();
    std::fs::write(
        artifact_root.join("target/compiled-output"),
        "compiler artifact",
    )
    .unwrap();
    drop(grant);
    assert!(
        artifact_root.join("target/compiled-output").is_file(),
        "dropping the last grant must not become another cleanup owner"
    );
    let root = std::fs::canonicalize(_dir.path()).unwrap();
    let repository = crate::workspace::RepositoryPlacementRef {
        repository_id: "artifact-repo".into(),
        deployment_id: "artifact-deployment".into(),
        host_path: root.join("repo"),
        enabled: true,
    };
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation =
        crate::workspace::repository_placement_upsert_mutation(&repository, &timestamp).unwrap();
    let result = node.execute(&mutation).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let error = crate::workspace::cleanup_workspace(&node, "artifact-workspace", Some(&root))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Active binding"), "{error}");
    assert!(artifact_root.exists());
    owner
        .terminalize_owned_without_stream(
            crate::lifecycle::RequestTerminalOutcome::Failed,
            Some("artifact cleanup fixture completed"),
        )
        .await
        .unwrap();
    crate::workspace::release_writer_binding(&node, owner.request())
        .await
        .unwrap();
    crate::workspace::cleanup_workspace(&node, "artifact-workspace", Some(&root))
        .await
        .unwrap();
    assert!(
        !source.exists(),
        "the existing executor removes its worktree"
    );
    assert!(
        !git_dir.exists(),
        "Git worktree cleanup removes its metadata directory"
    );
    assert!(
        !artifact_root.exists(),
        "private artifacts share the worktree cleanup owner"
    );
    assert!(
        repository.host_path.join("README.md").is_file(),
        "source repository remains"
    );
    let response = node.execute(r#"{ IsolatedWorkspace(filter: { workspace_id: { _eq: "artifact-workspace" } }) { lifecycle_state } }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.unwrap()["IsolatedWorkspace"][0]["lifecycle_state"],
        "cleaned"
    );
}

#[cfg(target_os = "macos")]
async fn artifact_alternate_owner(
    fx: &ArtifactTestFixture,
    name: &str,
    binding: &serde_json::Value,
) -> anyhow::Result<(
    crate::lifecycle::RequestLifecycle,
    crate::lifecycle::ClaimOutcome,
)> {
    use crate::identity::AgentIdentity;
    let identity =
        crate::identity::KeyIdentity::load_or_create(fx._dir.path().join("agent.key"), None)
            .unwrap();
    let did = identity.did();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        name,
        did,
        did,
        "general",
        &format!("artifact-session-{name}"),
        "Review source",
        "interactive",
        &now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    if !binding.is_null() {
        create.workspace_id = Some("artifact-workspace".into());
        create.workspace_authority = Some(binding["authority"].as_str().unwrap().into());
        create.workspace_owner_deployment_id = Some(
            if binding["owner_matches"] == false {
                "foreign-deployment"
            } else {
                "artifact-deployment"
            }
            .into(),
        );
        create.workspace_seal_hash = if binding["seal_matches"] == false {
            Some("wrong-seal".into())
        } else {
            fx.owner.request().workspace_seal_hash.clone()
        };
    }
    crate::request_admission::sign_agent_request_create(&identity, &mut create).await?;
    let mutation = fx.node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!mutation.has_errors(), "{:?}", mutation.errors);
    let id = crate::graphql::escape_graphql_string(name);
    let result = fx
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{id}" }} }}) {{ {} }} }}"#,
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&result, "AgentRequest")
            .unwrap()
            .unwrap();
    let request = crate::watcher::AgentRequest::try_from(row).unwrap();
    let mut owner = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        fx.node.clone(),
        "general",
        did,
        request,
        300,
    );
    let outcome = owner.claim().await.unwrap();
    Ok((owner, outcome))
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn artifact_resolver_rejects_missing_or_ready_context_before_binding() {
    for authority in [None, Some("readOnly"), Some("readWrite")] {
        let fx = artifact_test_fixture(&[]).await;
        let binding = authority
            .map(|authority| {
                serde_json::json!({
                    "authority": authority, "owner_matches": true, "seal_matches": true
                })
            })
            .unwrap_or(serde_json::Value::Null);
        let name = authority.unwrap_or("unbound");
        let (owner, claim) = artifact_alternate_owner(&fx, name, &binding).await.unwrap();
        assert!(matches!(claim, crate::lifecycle::ClaimOutcome::Claimed));
        for mutation in [
            r#"mutation { create_HostDeployment(input: { deployment_id: "artifact-deployment", display_name: "local", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z" }) { _docID } }"#,
            r#"mutation { update_IsolatedWorkspace(filter: { workspace_id: { _eq: "artifact-workspace" } }, input: { lifecycle_state: "ready" }) { _docID } }"#,
        ] {
            let result = fx.node.execute(mutation).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        let query = format!(
            r#"{{ WorkspaceBinding(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ binding_id lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(name)
        );
        let before = fx.node.execute(&query).await;
        assert!(!before.has_errors());
        assert!(before.data.as_ref().unwrap()["WorkspaceBinding"]
            .as_array()
            .unwrap()
            .is_empty());
        let denied = super::resolve_request_workspace_overlay(
            &fx.node,
            owner.request(),
            owner.execution_generation().unwrap(),
            true,
            Some(fx._dir.path()),
        )
        .await;
        assert!(
            denied
                .unwrap_err()
                .to_string()
                .contains("sealed ReadOnly workspace"),
            "{name}"
        );
        let after = fx.node.execute(&query).await;
        assert!(!after.has_errors());
        assert_eq!(
            after.data, before.data,
            "{name}: rejected artifact request created a binding"
        );
        // No artifact selection preserves the existing independent tool policy.
        let ordinary = super::resolve_request_workspace_overlay(
            &fx.node,
            owner.request(),
            owner.execution_generation().unwrap(),
            false,
            Some(fx._dir.path()),
        )
        .await
        .unwrap();
        assert_eq!(ordinary.is_some(), authority.is_some(), "{name}");
        assert!(ordinary
            .as_ref()
            .is_none_or(|overlay| overlay.workspace_artifact.is_none()));
    }
}
