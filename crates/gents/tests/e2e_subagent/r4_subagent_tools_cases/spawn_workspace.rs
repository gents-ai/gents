use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use gents::workspace::{
    isolated_workspace_upsert_mutation, workspace_placement_upsert_mutation, IsolatedWorkspaceDoc,
    WorkspacePlacementDoc,
};
use serde::Deserialize;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct ChildWorkspaceRow {
    #[allow(dead_code)]
    request_id: String,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_agent_did: Option<String>,
    workspace_seal_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IsolatedWorkspaceRow {
    workspace_id: String,
    path_capability: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    repository_id: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    seal_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspacePlacementRow {
    workspace_id: String,
    host_path: Option<String>,
    owner_agent_did: Option<String>,
}

async fn seed_isolated_workspace(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner_agent_did: &str,
    lifecycle_state: &str,
    seal_hash: Option<&str>,
    repository_id: &str,
    base_sha: &str,
    branch: &str,
    principal_did: &str,
) {
    let doc = IsolatedWorkspaceDoc {
        path_capability: gents::workspace::WorkspacePathCapability::exact_paths(vec![
            "README.md".into()
        ])
        .unwrap(),
        workspace_id: workspace_id.to_string(),
        work_unit_id: format!("{workspace_id}-unit"),
        repository_id: repository_id.to_string(),
        base_sha: base_sha.to_string(),
        branch: branch.to_string(),
        creation_policy: "git_worktree_diff".to_string(),
        adapter: "git_worktree".to_string(),
        owner_agent_did: owner_agent_did.to_string(),
        writer_principal: principal_did.to_string(),
        integrator_principal: principal_did.to_string(),
        instruction_manifest: "{}".to_string(),
        seal_hash: seal_hash.map(str::to_string),
        lifecycle_state: lifecycle_state.to_string(),
        caused_by_invocation_id: format!("{workspace_id}-inv"),
        caused_by_correlation: format!("{workspace_id}-corr"),
    };
    let mutation = isolated_workspace_upsert_mutation(&doc);
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert IsolatedWorkspace failed: {:?}",
        response.errors
    );
}

async fn seed_agent_principal(node: &EmbeddedNode, agent_did: &str) {
    gents::ensure_agent_principal(node, agent_did)
        .await
        .expect("ensure workspace owner principal");
}

async fn seed_repository_placement(
    node: &EmbeddedNode,
    repository_id: &str,
    agent_did: &str,
    host_path: &Path,
) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            create_RepositoryPlacement(input: {{
                repository_id: "{repo}",
                agent_did: "{owner}",
                host_path: "{path}",
                enabled: true,
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        repo = escape_graphql_string(repository_id),
        owner = escape_graphql_string(agent_did),
        path = escape_graphql_string(&host_path.to_string_lossy()),
        now = escape_graphql_string(&now),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create RepositoryPlacement failed: {:?}",
        response.errors
    );
}

async fn seed_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
    agent_did: &str,
    host_path: &Path,
    repository_id: &str,
    observed_tree_hash: &str,
) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc = WorkspacePlacementDoc {
        workspace_id: workspace_id.to_string(),
        owner_agent_did: agent_did.to_string(),
        host_path: host_path.to_string_lossy().into_owned(),
        repository_placement_id: repository_id.to_string(),
        adapter: "git_worktree".to_string(),
        adapter_version: "gents-workspace-adapter/1".to_string(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "{}".to_string(),
        observed_tree_hash: observed_tree_hash.to_owned(),
    };
    let mutation = workspace_placement_upsert_mutation(&doc, &now);
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert WorkspacePlacement failed: {:?}",
        response.errors
    );
}

async fn seed_workspace_root(node: &EmbeddedNode, root_path: &Path) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            create_WorkspaceRoot(input: {{
                root_path: "{path}",
                display_name: "spawn-test",
                enabled: true,
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        path = escape_graphql_string(&root_path.to_string_lossy()),
        now = escape_graphql_string(&now),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create WorkspaceRoot failed: {:?}",
        response.errors
    );
}

async fn seed_local_workspace(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner: &str,
    lifecycle_state: &str,
    seal_hash: Option<&str>,
    repository_id: &str,
    base_sha: &str,
    branch: &str,
    placement_path: &Path,
    principal_did: &str,
) {
    seed_agent_principal(node, owner).await;
    seed_isolated_workspace(
        node,
        workspace_id,
        owner,
        lifecycle_state,
        seal_hash,
        repository_id,
        base_sha,
        branch,
        principal_did,
    )
    .await;
    seed_workspace_placement(
        node,
        workspace_id,
        owner,
        placement_path,
        repository_id,
        seal_hash.unwrap_or(""),
    )
    .await;
}

async fn fetch_child_workspace(node: &EmbeddedNode, child_request_id: &str) -> ChildWorkspaceRow {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                workspace_id
                workspace_authority
                workspace_owner_agent_did
                workspace_seal_hash
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_isolated_workspace(node: &EmbeddedNode, workspace_id: &str) -> IsolatedWorkspaceRow {
    let escaped = escape_graphql_string(workspace_id);
    let query = format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                workspace_id
                lifecycle_state
                path_capability
                repository_id
                branch
                seal_hash
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "IsolatedWorkspace")
}

async fn fetch_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
) -> WorkspacePlacementRow {
    let escaped = escape_graphql_string(workspace_id);
    let query = format!(
        r#"{{
            WorkspacePlacement(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                workspace_id
                host_path
                owner_agent_did
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "WorkspacePlacement")
}

async fn spawn_background_child_result(
    fixture: &SpawnFixture,
    tool_call_id: &str,
    workspace: Option<Value>,
) -> Value {
    spawn_background_child_turn(fixture, tool_call_id, workspace)
        .await
        .0
}

async fn spawn_background_child_turn(
    fixture: &SpawnFixture,
    tool_call_id: &str,
    workspace: Option<Value>,
) -> (Value, AcceptedTurnRuntime) {
    let mut args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "workspace child prompt",
        "await_mode": "background",
    });
    if let Some(workspace) = workspace {
        args["workspace"] = workspace;
    }
    let args = args.to_string();
    let runtime = run_canonical_spawn_turn(fixture, tool_call_id, &args).await;
    let tool = fetch_tool_call(&fixture.db.node, &fixture.session_id, tool_call_id).await;
    (persisted_tool_result_json(&tool), runtime)
}

async fn spawn_background_child(
    fixture: &SpawnFixture,
    tool_call_id: &str,
    workspace: Option<Value>,
) -> ChildWorkspaceRow {
    // The runtime's subagent source materializes the child after the parent's
    // background receipt; the runtime must outlive that write.
    let (result, runtime) = spawn_background_child_turn(fixture, tool_call_id, workspace).await;
    assert_eq!(result["ok"], true, "{result}");
    let child = wait_for_child_request_for_tool(
        fixture.db.node.as_ref(),
        &fixture.session_id,
        tool_call_id,
    )
    .await;
    drop(runtime);
    fetch_child_workspace(fixture.db.node.as_ref(), &child.request_id).await
}

/// Without a WorkspaceWrite sandbox, admission refuses a ReadWrite-bound parent
/// before any provider turn, tool call or child workspace exists.
async fn assert_readwrite_parent_refused_without_sandbox(
    fixture: &SpawnFixture,
    tool_call_id: &str,
    workspace: Value,
    parent_workspace_id: &str,
) {
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "workspace child prompt",
        "await_mode": "background",
        "workspace": workspace,
    })
    .to_string();
    let runtime = boot_canonical_spawn_turn(
        fixture,
        tool_call_id,
        &args,
        "child held for fixture observation",
        1,
    )
    .await;
    let reason = wait_for_canonical_parent_terminal(fixture)
        .await
        .expect_err("ReadWrite parent must be refused on a host without a WorkspaceWrite sandbox");
    assert!(
        reason.contains("requires an enforceable WorkspaceWrite sandbox on this host"),
        "expected explicit unsupported-sandbox refusal, got {reason:?}"
    );
    assert_eq!(
        runtime.backend.observed_completion_requests(),
        0,
        "refused parent must not reach the provider"
    );
    let session = escape_graphql_string(&fixture.session_id);
    let response = fixture
        .db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ tool_call_id }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.as_ref().unwrap()["AgentToolCall"],
        json!([]),
        "refused parent must not dispatch a tool call"
    );
    let owner = escape_graphql_string(&fixture.agent_did);
    let response = fixture
        .db
        .node
        .execute(&format!(
            r#"{{ IsolatedWorkspace(filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }}) {{ workspace_id }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.as_ref().unwrap()["IsolatedWorkspace"],
        json!([{ "workspace_id": parent_workspace_id }]),
        "refused parent must not provision a child workspace"
    );
    runtime.shutdown().await;
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

fn init_git_repo() -> (TempDir, PathBuf, String) {
    let root = TempDir::new().expect("tempdir");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "ws@example.com"]);
    git(&repo, &["config", "user.name", "Workspace Test"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    (root, repo, sha)
}

fn placement_dir(label: &str) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("placement tempdir");
    let path = root.path().join(label);
    std::fs::create_dir_all(&path).unwrap();
    (root, path)
}

async fn provision_parent_workspace(
    fixture: &SpawnFixture,
    workspace_id: &str,
    repository_id: &str,
    repo: &Path,
    base_sha: &str,
) -> PathBuf {
    use gents::workspace::{
        emit_create_workspace_plan, execute_create_workspace_plan, CreateWorkspaceAction,
        CreationPolicy, HostExecutorContext, MemoryWorkspaceDocuments, RepositoryPlacementRef,
        WorkspaceAdapterKind, WorkspacePathCapability, CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
    };
    let mut documents = MemoryWorkspaceDocuments::default();
    let mut context = HostExecutorContext {
        owner_agent_did: fixture.agent_did.clone(),
        repository: RepositoryPlacementRef {
            repository_id: repository_id.into(),
            owner_agent_did: fixture.agent_did.clone(),
            host_path: repo.into(),
            enabled: true,
        },
        ceiling: Some(repo),
        capabilities: [CAP_CREATE_WORKSPACE.into(), CAP_OBSERVE_DIRTY_BASE.into()]
            .into_iter()
            .collect(),
        writer_principal: fixture.agent_did.clone(),
        integrator_principal: fixture.agent_did.clone(),
        caused_by_invocation_id: format!("{workspace_id}-inv"),
        caused_by_correlation: format!("{workspace_id}-corr"),
        documents: &mut documents,
    };
    let created = execute_create_workspace_plan(
        &emit_create_workspace_plan(CreateWorkspaceAction {
            path_capability: WorkspacePathCapability::exact_paths(vec!["README.md".into()])
                .unwrap(),
            workspace_id: workspace_id.into(),
            work_unit_id: format!("{workspace_id}-unit"),
            repository_id: repository_id.into(),
            base_sha: base_sha.into(),
            branch: "topic".into(),
            creation_policy: CreationPolicy::GitWorktreeDiff,
            adapter: WorkspaceAdapterKind::GitWorktree,
            clone_artifacts: None,
        }),
        &mut Vec::new(),
        &mut context,
    )
    .expect("provision admitted parent workspace including host identity marker");
    let response = fixture
        .db
        .node
        .execute(&isolated_workspace_upsert_mutation(&created.workspace))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let now = chrono::Utc::now().to_rfc3339();
    let response = fixture
        .db
        .node
        .execute(&workspace_placement_upsert_mutation(
            &created.placement,
            &now,
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    PathBuf::from(created.placement.host_path)
}

fn parent_workspace_fields(workspace_id: &str, owner: &str, authority: &str) -> String {
    format!(
        r#", workspace_id: "{id}"
                , workspace_authority: "{authority}"
                , workspace_owner_agent_did: "{owner}""#,
        id = escape_graphql_string(workspace_id),
        authority = escape_graphql_string(authority),
        owner = escape_graphql_string(owner),
    )
}

#[tokio::test]
async fn spawn_subagent_inherit_uses_parent_authority_infimum() {
    let workspace_id = "ws-inherit-infimum";
    let owner = "did:test:workspace-inherit";
    let extra = parent_workspace_fields(workspace_id, owner, "readOnly");
    let mut fixture = setup_spawn_fixture_with_parent_fields(
        "spawn_ws_inherit",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        &extra,
    )
    .await;
    let (placement_root, placement) = placement_dir("inherit");
    seed_local_workspace(
        fixture.db.node.as_ref(),
        workspace_id,
        &fixture.agent_did,
        "ready",
        None,
        "repo-inherit",
        "abc123",
        "topic",
        &placement,
        &fixture.agent_did,
    )
    .await;

    let child =
        spawn_background_child(&fixture, "internal-spawn-inherit", Some(json!("inherit"))).await;
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(
        child.workspace_authority.as_deref(),
        Some("readOnly"),
        "inherit must infimum Ready/ReadWrite default with parent ReadOnly"
    );
    assert_eq!(
        child.workspace_owner_agent_did.as_deref(),
        Some(fixture.agent_did.as_str())
    );
    assert!(child
        .workspace_seal_hash
        .as_deref()
        .is_none_or(|value| value.is_empty()));

    // A second invocation is a new admitted request, not a replacement of the
    // physical request already named by the workspace binding.
    fixture.request_id.push_str("-default");
    fixture.session_id.push_str("-default");
    let omitted = spawn_background_child(&fixture, "internal-spawn-inherit-default", None).await;
    assert_eq!(omitted.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(
        omitted.workspace_authority.as_deref(),
        Some("readOnly"),
        "omitted workspace must default to inherit with parent authority infimum"
    );
    let _keep = placement_root;
}

#[tokio::test]
async fn spawn_subagent_inherit_sealed_copies_seal_hash() {
    let workspace_id = "ws-inherit-sealed";
    let owner = "did:test:workspace-inherit-sealed";
    let (placement_root, placement, base_sha) = init_git_repo();
    let seal_hash = git(&placement, &["rev-parse", "HEAD^{tree}"]);
    let extra = format!(
        "{}, workspace_seal_hash: \"{}\"",
        parent_workspace_fields(workspace_id, owner, "readOnly"),
        escape_graphql_string(&seal_hash)
    );
    let fixture = setup_spawn_fixture_with_parent_fields(
        "spawn_ws_inherit_sealed",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        &extra,
    )
    .await;
    // The canonical owned loop reads the sealed workspace's Git tree before
    // dispatch; an empty directory is not an admitted workspace.
    seed_local_workspace(
        fixture.db.node.as_ref(),
        workspace_id,
        &fixture.agent_did,
        "sealed",
        Some(&seal_hash),
        "repo-inherit-sealed",
        &base_sha,
        "main",
        &placement,
        &fixture.agent_did,
    )
    .await;

    let child = spawn_background_child(
        &fixture,
        "internal-spawn-inherit-sealed",
        Some(json!("inherit")),
    )
    .await;
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(
        child.workspace_seal_hash.as_deref(),
        Some(seal_hash.as_str())
    );
    let _keep = placement_root;
}

#[tokio::test]
async fn spawn_subagent_bind_id_stamps_existing_workspace() {
    let workspace_id = "ws-bind-ready";
    let fixture = setup_spawn_fixture("spawn_ws_bind", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let owner = fixture.agent_did.clone();
    let (placement_root, placement) = placement_dir("bind");
    seed_local_workspace(
        fixture.db.node.as_ref(),
        workspace_id,
        &owner,
        "ready",
        None,
        "repo-bind",
        "abc123",
        "topic",
        &placement,
        &fixture.agent_did,
    )
    .await;

    let child = spawn_background_child(
        &fixture,
        "internal-spawn-bind",
        Some(json!({ "id": workspace_id })),
    )
    .await;
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(child.workspace_authority.as_deref(), Some("readWrite"));
    assert_eq!(
        child.workspace_owner_agent_did.as_deref(),
        Some(owner.as_str())
    );
    let _keep = placement_root;
}

#[tokio::test]
async fn spawn_subagent_bind_id_infimums_parent_readonly() {
    let workspace_id = "ws-bind-readonly-parent";
    let owner = "did:test:workspace-bind-ro";
    let extra = parent_workspace_fields(workspace_id, owner, "readOnly");
    let fixture = setup_spawn_fixture_with_parent_fields(
        "spawn_ws_bind_ro",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        &extra,
    )
    .await;
    let (placement_root, placement) = placement_dir("bind-ro");
    seed_local_workspace(
        fixture.db.node.as_ref(),
        workspace_id,
        &fixture.agent_did,
        "ready",
        None,
        "repo-bind-ro",
        "abc123",
        "topic",
        &placement,
        &fixture.agent_did,
    )
    .await;

    let child = spawn_background_child(
        &fixture,
        "internal-spawn-bind-ro",
        Some(json!({ "id": workspace_id })),
    )
    .await;
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(
        child.workspace_authority.as_deref(),
        Some("readOnly"),
        "bind-id must infimum Ready/ReadWrite default with parent ReadOnly"
    );
    let _keep = placement_root;
}

#[tokio::test]
async fn spawn_subagent_bind_id_sealed_copies_seal_hash() {
    let workspace_id = "ws-bind-sealed";
    let fixture =
        setup_spawn_fixture("spawn_ws_bind_sealed", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let owner = fixture.agent_did.clone();
    let (placement_root, placement) = placement_dir("bind-sealed");
    seed_local_workspace(
        fixture.db.node.as_ref(),
        workspace_id,
        &owner,
        "sealed",
        Some("seal-bind"),
        "repo-bind-sealed",
        "abc123",
        "topic",
        &placement,
        &fixture.agent_did,
    )
    .await;

    let child = spawn_background_child(
        &fixture,
        "internal-spawn-bind-sealed",
        Some(json!({ "id": workspace_id })),
    )
    .await;
    assert_eq!(child.workspace_id.as_deref(), Some(workspace_id));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-bind"));
    let _keep = placement_root;
}

#[tokio::test]
async fn spawn_subagent_provision_creates_isolated_workspace() {
    let parent_workspace_id = "ws-provision-parent";
    let owner = "did:test:workspace-provision";
    let (root, repo, sha) = init_git_repo();
    let extra = parent_workspace_fields(parent_workspace_id, owner, "readWrite");
    let mut fixture = setup_spawn_fixture_with_parent_fields(
        "spawn_ws_provision",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        &extra,
    )
    .await;
    provision_parent_workspace(&fixture, parent_workspace_id, "repo-provision", &repo, &sha).await;
    seed_repository_placement(
        fixture.db.node.as_ref(),
        "repo-provision",
        &fixture.agent_did,
        &repo,
    )
    .await;
    seed_workspace_root(
        fixture.db.node.as_ref(),
        &std::fs::canonicalize(root.path()).unwrap(),
    )
    .await;
    if !gents::toolset::workspace_write_sandbox_enforced() {
        let before = git(&repo, &["worktree", "list"]);
        assert_readwrite_parent_refused_without_sandbox(
            &fixture,
            "internal-spawn-provision",
            json!({ "provision": { "policy": "git_worktree_diff" } }),
            parent_workspace_id,
        )
        .await;
        assert_eq!(
            git(&repo, &["worktree", "list"]),
            before,
            "refused spawn must not add a worktree"
        );
        return;
    }

    let first = spawn_background_child(
        &fixture,
        "internal-spawn-provision",
        Some(json!({ "provision": { "policy": "git_worktree_diff" } })),
    )
    .await;
    let first_id = first
        .workspace_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .expect("provision must stamp workspace_id")
        .to_string();
    assert_ne!(first_id, parent_workspace_id);
    assert_eq!(first.workspace_authority.as_deref(), Some("readWrite"));
    assert_eq!(
        first.workspace_owner_agent_did.as_deref(),
        Some(fixture.agent_did.as_str())
    );

    let created = fetch_isolated_workspace(fixture.db.node.as_ref(), &first_id).await;
    assert_eq!(created.workspace_id, first_id);
    assert_eq!(created.lifecycle_state.as_deref(), Some("ready"));
    assert_eq!(created.repository_id.as_deref(), Some("repo-provision"));
    let capability: gents::workspace::WorkspacePathCapability =
        serde_json::from_str(&created.path_capability).unwrap();
    assert_eq!(
        capability,
        gents::workspace::WorkspacePathCapability::exact_paths(vec!["README.md".into()]).unwrap(),
        "fresh child must inherit its parent's exact file scope"
    );
    assert_ne!(created.branch.as_deref(), Some("topic"));
    assert!(
        created
            .branch
            .as_deref()
            .is_some_and(|branch| branch.contains("topic-ws-")),
        "child branch should be unique, got {:?}",
        created.branch
    );

    let placement = fetch_workspace_placement(fixture.db.node.as_ref(), &first_id).await;
    assert_eq!(placement.workspace_id, first_id);
    assert_eq!(
        placement.owner_agent_did.as_deref(),
        Some(fixture.agent_did.as_str())
    );
    let host_path = placement
        .host_path
        .as_deref()
        .filter(|value| !value.is_empty())
        .expect("provision must persist WorkspacePlacement.host_path");
    let dest = PathBuf::from(host_path);
    assert!(dest.is_dir(), "placement dest missing: {}", dest.display());
    assert!(
        dest.starts_with(std::fs::canonicalize(root.path()).unwrap()),
        "placement {} must sit under operator WorkspaceRoot {}",
        dest.display(),
        root.path().display()
    );
    let listed = git(&repo, &["worktree", "list"]);
    assert!(
        listed.contains(&dest.to_string_lossy().into_owned())
            || listed.contains(&dest.canonicalize().unwrap().to_string_lossy().into_owned()),
        "git worktree list missing dest {dest:?}: {listed}"
    );

    fixture.request_id.push_str("-second");
    fixture.session_id.push_str("-second");
    // The first writer seals its parent workspace on completion. A later
    // request must bind that immutable tree as a reader, not reopen a writer.
    let parent = fetch_isolated_workspace(fixture.db.node.as_ref(), parent_workspace_id).await;
    let seal_hash = parent
        .seal_hash
        .expect("completed parent writer sealed its workspace");
    fixture.extra_parent_fields = format!(
        "{}, workspace_seal_hash: \"{}\"",
        parent_workspace_fields(parent_workspace_id, &fixture.agent_did, "readOnly"),
        escape_graphql_string(&seal_hash)
    );
    let second = spawn_background_child(
        &fixture,
        "internal-spawn-provision-2",
        Some(json!({ "provision": { "policy": "git_worktree_diff" } })),
    )
    .await;
    let second_id = second
        .workspace_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .expect("second provision must stamp workspace_id")
        .to_string();
    assert_ne!(second_id, first_id);
    assert_ne!(second_id, parent_workspace_id);
    let second_created = fetch_isolated_workspace(fixture.db.node.as_ref(), &second_id).await;
    assert_ne!(second_created.branch.as_deref(), Some("topic"));
    assert_ne!(second_created.branch, created.branch);
    let second_placement = fetch_workspace_placement(fixture.db.node.as_ref(), &second_id).await;
    let second_dest = PathBuf::from(
        second_placement
            .host_path
            .as_deref()
            .expect("second placement host_path"),
    );
    assert!(second_dest.is_dir());
    let listed = git(&repo, &["worktree", "list"]);
    assert!(
        listed.contains(&second_dest.to_string_lossy().into_owned())
            || listed.contains(
                &second_dest
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            ),
        "git worktree list missing second dest {second_dest:?}: {listed}"
    );
    let _keep = root;
}

#[tokio::test]
async fn spawn_subagent_provision_fails_closed_when_dest_escapes_operator_tool_root() {
    // The operator ceiling is process-wide. Exercise a different ceiling in
    // its own process so parallel runtime fixtures cannot overwrite it.
    const ISOLATED: &str = "GENTS_SPAWN_WORKSPACE_CEILING_TEST";
    if std::env::var_os(ISOLATED).is_none() {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("r4_subagent_tools::spawn_workspace::spawn_subagent_provision_fails_closed_when_dest_escapes_operator_tool_root")
            .arg("--nocapture")
            .env(ISOLATED, "1")
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(60), command.output())
            .await
            .expect("isolated operator-ceiling test timed out")
            .expect("run isolated operator-ceiling test");
        assert!(
            output.status.success(),
            "isolated operator-ceiling test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let parent_workspace_id = "ws-provision-ceiling";
    let owner = "did:test:workspace-provision-ceiling";
    let (root, repo, sha) = init_git_repo();
    // The ceiling guard precedes authority stamping; a ReadOnly parent keeps
    // this premise admissible on hosts without a WorkspaceWrite sandbox.
    let extra = parent_workspace_fields(parent_workspace_id, owner, "readOnly");
    let mut fixture = setup_spawn_fixture_with_parent_fields(
        "spawn_ws_provision_ceiling",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        &extra,
    )
    .await;
    let parent_ws = provision_parent_workspace(
        &fixture,
        parent_workspace_id,
        "repo-provision-ceiling",
        &repo,
        &sha,
    )
    .await;
    seed_repository_placement(
        fixture.db.node.as_ref(),
        "repo-provision-ceiling",
        &fixture.agent_did,
        &repo,
    )
    .await;
    seed_workspace_root(
        fixture.db.node.as_ref(),
        &std::fs::canonicalize(root.path()).unwrap(),
    )
    .await;
    // Admit the parent itself, but exclude the sibling destination chosen for
    // the new child workspace. A ceiling excluding the parent tests admission,
    // not the spawn provisioning guard.
    fixture.operator_tool_root = Some(std::fs::canonicalize(&parent_ws).unwrap());

    let tool_call_id = "internal-spawn-provision-ceiling";
    let result = spawn_background_child_result(
        &fixture,
        tool_call_id,
        Some(json!({ "provision": { "policy": "git_worktree_diff" } })),
    )
    .await;
    assert_eq!(result["ok"], false, "{result}");
    let message = result["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ceiling") || message.contains("escapes") || message.contains("tool root"),
        "expected operator ceiling denial, got {result}"
    );

    // Check the actual principal's entire fixture scope, not a guessed
    // workspace ID derived from a provider ID rather than the physical call.
    let escaped = escape_graphql_string(&fixture.agent_did);
    let query = format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ owner_agent_did: {{ _eq: "{escaped}" }} }}
            ) {{ workspace_id }}
        }}"#
    );
    let response = fixture.db.node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["IsolatedWorkspace"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "denied spawn must leave only the parent workspace: {rows:?}"
    );
    assert_eq!(rows[0]["workspace_id"], parent_workspace_id);
    let _keep = root;
}
