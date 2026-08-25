use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graph_package::{
    bundled_graph_id, default_bundled_graph_package_install_bindings, graph_package_catalog,
    install_bundled_graph_package, load_bundled_graph_package, GraphPackageInstallBindings,
};
use gents::graph_pipeline::{
    activate_graph_revision_with_access, load_graph_run_view_with_access,
    request_graph_run_cancellation_with_access, revision_gate_decision,
    start_graph_run_with_access, GraphPlan, GraphRunView,
};
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::{
    GraphCancelArgs, GraphCatalogArgs, GraphCommand, GraphInstallArgs, GraphPublishArgs,
    GraphResultArgs, GraphRunArgs, GraphScopeArgs, GraphToggleArgs, GraphWatchArgs,
};
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: GraphCommand) -> Result<()> {
    match command {
        GraphCommand::Catalog(args) => catalog(args),
        GraphCommand::Install(args) => install(args).await,
        GraphCommand::Publish(args) => publish(args).await,
        GraphCommand::Run(args) => run(args).await,
        GraphCommand::Watch(args) => watch(args).await,
        GraphCommand::Result(args) => result(args).await,
        GraphCommand::Cancel(args) => cancel(args).await,
        GraphCommand::Disable(args) => toggle(args, false).await,
        GraphCommand::Enable(args) => toggle(args, true).await,
    }
}

fn catalog(args: GraphCatalogArgs) -> Result<()> {
    let mut entries = graph_package_catalog()?;
    if let Some(package) = args.package.as_deref() {
        entries.retain(|entry| entry.name == package);
        if entries.is_empty() {
            anyhow::bail!("unknown bundled graph package {package:?}");
        }
    }
    print_json(&json!({ "packages": entries }))
}

async fn install(args: GraphInstallArgs) -> Result<()> {
    let package = load_bundled_graph_package(&args.package)?;
    if args
        .version
        .as_deref()
        .is_some_and(|version| version != package.manifest.version)
    {
        anyhow::bail!(
            "bundled package {} has version {}, not {}",
            args.package,
            package.manifest.version,
            args.version.as_deref().unwrap_or_default()
        );
    }
    let owner_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) = resolve_config_access(args.home.as_deref(), None).await?;
    let ConfigAccess::Local(node) = access else {
        anyhow::bail!(
            "graph install currently requires direct home access; stop the running server and retry with --home"
        );
    };
    let bindings = if let Some(path) = args.bindings.as_deref() {
        let bindings: GraphPackageInstallBindings = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("reading graph package bindings {}", path.display()))?,
        )
        .with_context(|| format!("parsing graph package bindings {}", path.display()))?;
        if bindings.owner_did != owner_did {
            anyhow::bail!(
                "binding owner {} does not match selected package owner {}",
                bindings.owner_did,
                owner_did
            );
        }
        bindings
    } else {
        default_bundled_graph_package_install_bindings(&node, &args.package, &owner_did).await?
    };
    let receipt =
        install_bundled_graph_package(&node, None, &owner_did, &args.package, &bindings).await?;
    print_json(&json!({
        "install": receipt,
        "bindings": bindings,
        "next": format!(
            "gents graph publish {} --revision {} --confirm-revision {}",
            args.package, receipt.revision_digest, receipt.revision_digest
        ),
    }))
}

async fn access_and_actor(scope: &GraphScopeArgs) -> Result<(ConfigAccess, String)> {
    let actor = resolve_agent_did(scope.home.as_deref(), scope.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
    Ok((access, actor))
}

fn rows<'a>(response: &'a Value, collection: &str) -> &'a [Value] {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

async fn load_revision_plan(
    access: &ConfigAccess,
    digest: &str,
    package_name: &str,
    actor_did: &str,
) -> Result<GraphPlan> {
    let response = access
        .execute(&format!(
            r#"{{ GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 2) {{ graph_id owner_did plan_json }} }}"#,
            escape_graphql_string(digest),
        ))
        .await?;
    let found = rows(&response, "GraphRevision");
    if found.len() != 1 {
        anyhow::bail!("revision {digest:?} is missing or ambiguous");
    }
    if found[0].get("owner_did").and_then(Value::as_str) != Some(actor_did) {
        anyhow::bail!("actor does not own revision {digest:?}");
    }
    let plan: GraphPlan = serde_json::from_str(
        found[0]
            .get("plan_json")
            .and_then(Value::as_str)
            .context("revision is missing plan_json")?,
    )?;
    if plan.package.as_ref().map(|package| package.name.as_str()) != Some(package_name) {
        anyhow::bail!("revision does not belong to bundled package {package_name:?}");
    }
    Ok(plan)
}

async fn active_digest(
    access: &ConfigAccess,
    graph_id: &str,
    actor_did: &str,
) -> Result<Option<String>> {
    let response = access
        .execute(&format!(
            r#"{{ GraphDefinition(filter: {{ graph_id: {{ _eq: "{}" }} }}, limit: 2) {{ owner_did active_revision_digest }} }}"#,
            escape_graphql_string(graph_id),
        ))
        .await?;
    let found = rows(&response, "GraphDefinition");
    if found.len() != 1 {
        anyhow::bail!("graph {graph_id:?} is missing or ambiguous");
    }
    if found[0].get("owner_did").and_then(Value::as_str) != Some(actor_did) {
        anyhow::bail!("actor does not own graph {graph_id:?}");
    }
    Ok(found[0]
        .get("active_revision_digest")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

async fn publish(args: GraphPublishArgs) -> Result<()> {
    if args.revision != args.confirm_revision {
        anyhow::bail!("--confirm-revision must exactly match --revision");
    }
    let (access, actor) = access_and_actor(&args.scope).await?;
    let plan = load_revision_plan(&access, &args.revision, &args.package, &actor).await?;
    let previous = active_digest(&access, &plan.graph_id, &actor).await?;
    let receipt = activate_graph_revision_with_access(
        &access,
        &actor,
        &plan.graph_id,
        &args.revision,
        previous.as_deref(),
    )
    .await?;
    print_json(&serde_json::to_value(receipt)?)
}

fn git_output(repo: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .with_context(|| format!("running git in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn resolve_repository(
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<(std::path::PathBuf, String, String)> {
    let canonical = std::fs::canonicalize(repo)
        .with_context(|| format!("canonicalizing repository {}", repo.display()))?;
    if git_output(&canonical, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        anyhow::bail!("{} is not a Git work tree", canonical.display());
    }
    let base_sha = git_output(
        &canonical,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )?;
    let head_sha = git_output(
        &canonical,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    Ok((canonical, base_sha, head_sha))
}

async fn run(args: GraphRunArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let ConfigAccess::Graphql(endpoint) = &access else {
        anyhow::bail!(
            "graph run requires the local Gents server to be running so workspace and request recovery remain active"
        );
    };
    let endpoint_url = url::Url::parse(endpoint).context("parsing graph GraphQL endpoint")?;
    let local_endpoint = endpoint_url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if !local_endpoint {
        anyhow::bail!(
            "the local-repository quickstart requires a loopback GraphQL endpoint; remote repository placement is not inferred from a client path"
        );
    }
    let graph_id = bundled_graph_id(&args.package, &actor)?;
    let digest = active_digest(&access, &graph_id, &actor)
        .await?
        .context("graph has no active revision; install and publish it first")?;
    let plan = load_revision_plan(&access, &digest, &args.package, &actor).await?;
    let deployments = plan
        .package
        .as_ref()
        .context("active revision has no package attribution")?
        .roles
        .values()
        .map(|role| role.deployment_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if deployments.len() != 1 {
        anyhow::bail!("the local code-review quickstart requires all roles on one deployment");
    }
    let deployment_id = deployments.into_iter().next().expect("one deployment");
    let (repository_path, base_ref, head_ref) =
        resolve_repository(&args.repo, &args.base, &args.head)?;
    let workspace = gents::workspace::provision_read_only_workspace(
        &access,
        &repository_path,
        &head_ref,
        deployment_id,
        &actor,
    )
    .await?;
    let receipt = start_graph_run_with_access(
        &access,
        &actor,
        &graph_id,
        "review",
        json!({
            "repository_path": ".",
            "base_ref": base_ref,
            "head_ref": head_ref,
            "workspace_id": workspace.workspace.workspace_id,
            "workspace_authority": "readOnly",
            "workspace_owner_deployment_id": workspace.workspace.owner_deployment_id,
            "lens_count": "4",
            "lens_min": "4",
            "lens_max": "4",
            "pr_number": "",
            "focus": args.focus.unwrap_or_else(|| "Review the diff for material correctness, safety, durability, and maintainability defects.".to_owned()),
        }),
    )
    .await?;
    if args.watch {
        watch_run(&access, &actor, &receipt.run_id, Duration::from_secs(1)).await
    } else {
        print_json(&serde_json::to_value(receipt)?)
    }
}

fn progress(view: &GraphRunView) -> Value {
    json!({
        "run_id": view.run_id,
        "status": view.status,
        "revision_digest": view.revision_digest,
        "active_requests": view.active_request_count,
        "terminal_requests": view.terminal_request_count,
        "stages": view.stages,
        "groups": view.groups,
        "results": view.results,
        "error": view.error,
    })
}

async fn watch_run(
    access: &ConfigAccess,
    actor: &str,
    run_id: &str,
    interval: Duration,
) -> Result<()> {
    let mut last = Value::Null;
    loop {
        let view = load_graph_run_view_with_access(access, actor, run_id).await?;
        let current = progress(&view);
        if current != last {
            print_json(&current)?;
            last = current;
        }
        if view.is_terminal() {
            if view.status == "succeeded" {
                return Ok(());
            }
            anyhow::bail!("graph run {} ended {}", view.run_id, view.status);
        }
        tokio::time::sleep(interval).await;
    }
}

async fn watch(args: GraphWatchArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    watch_run(
        &access,
        &actor,
        &args.run_id,
        Duration::from_millis(args.interval_ms.max(100)),
    )
    .await
}

async fn result(args: GraphResultArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let view = load_graph_run_view_with_access(&access, &actor, &args.run_id).await?;
    print_json(&json!({
        "run_id": view.run_id,
        "status": view.status,
        "revision_digest": view.revision_digest,
        "results": view.results,
        "result_refs": view.persisted_result_refs,
        "error": view.error,
    }))
}

async fn cancel(args: GraphCancelArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let view = request_graph_run_cancellation_with_access(
        &access,
        &actor,
        &args.run_id,
        args.reason.as_deref(),
    )
    .await?;
    print_json(&progress(&view))
}

async fn toggle(args: GraphToggleArgs, enabled: bool) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let graph_id = bundled_graph_id(&args.package, &actor)?;
    let response = access
        .execute(&format!(
            r#"{{ GraphDefinition(filter: {{ graph_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID owner_did active_revision_digest }} }}"#,
            escape_graphql_string(&graph_id),
        ))
        .await?;
    let found = rows(&response, "GraphDefinition");
    if found.len() != 1 || found[0].get("owner_did").and_then(Value::as_str) != Some(&actor) {
        anyhow::bail!("graph is missing, ambiguous, or owned by another principal");
    }
    if enabled {
        let digest = found[0]
            .get("active_revision_digest")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("cannot enable a graph without an active revision")?;
        let revision = access
            .execute(&format!(
                r#"{{ GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 1) {{ status artifacts_complete }} }}"#,
                escape_graphql_string(digest),
            ))
            .await?;
        let row = rows(&revision, "GraphRevision")
            .first()
            .context("active revision is missing")?;
        if !revision_gate_decision(
            row.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            row.get("artifacts_complete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            false,
            true,
        )
        .may_start
        {
            anyhow::bail!("active revision is not runnable");
        }
    }
    let doc_id = found[0]
        .get("_docID")
        .and_then(Value::as_str)
        .context("GraphDefinition is missing _docID")?;
    access
        .execute_committed(&format!(
            r#"mutation {{ update_GraphDefinition(docID: "{}", input: {{ enabled: {}, updated_at: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(doc_id),
            enabled,
            escape_graphql_string(&chrono::Utc::now().to_rfc3339()),
        ))
        .await?;
    print_json(&json!({ "graph_id": graph_id, "enabled": enabled }))
}
