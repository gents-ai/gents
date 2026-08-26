use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::{io, io::IsTerminal as _, io::Write as _};

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
use gents::run_timeline::{RunActivityRows, TimelineToolCallRow};
use gents::run_timeline_fetch::load_run_activity_rows;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::{
    GraphCancelArgs, GraphCatalogArgs, GraphCommand, GraphInstallArgs, GraphResultArgs,
    GraphRunArgs, GraphScopeArgs, GraphToggleArgs, GraphWatchArgs,
};
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: GraphCommand) -> Result<()> {
    match command {
        GraphCommand::Catalog(args) => catalog(args),
        GraphCommand::Install(args) => install(args).await,
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
    let (access, owner_did) = access_and_actor(&args.scope).await?;
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
        default_bundled_graph_package_install_bindings(&access, &args.package, &owner_did).await?
    };
    let receipt =
        install_bundled_graph_package(&access, &owner_did, &args.package, &bindings).await?;
    let previous = active_digest(&access, &receipt.graph_id, &owner_did).await?;
    let activation = activate_graph_revision_with_access(
        &access,
        &owner_did,
        &receipt.graph_id,
        &receipt.revision_digest,
        previous.as_deref(),
    )
    .await?;
    match args
        .output
        .ensure_supported("graph install", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => print_json(&json!({
            "install": receipt,
            "activation": activation,
            "bindings": bindings,
        })),
        OutputFormat::Text => {
            let mut out = io::stdout().lock();
            writeln!(
                out,
                "Installed and activated {} {}",
                receipt.package_name, receipt.package_version
            )?;
            writeln!(out, "Backend: inherited from your default behavior")?;
            writeln!(out, "Run: gents graph run {}", receipt.package_name)?;
            Ok(())
        }
        _ => unreachable!("validated output format"),
    }
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
        .context("graph is not installed; run `gents graph install code-review` first")?;
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
        watch_run(
            &access,
            &actor,
            &receipt.run_id,
            Duration::from_secs(1),
            args.output,
        )
        .await
    } else {
        match args
            .output
            .ensure_supported("graph run", &[OutputFormat::Text, OutputFormat::Json])?
        {
            OutputFormat::Json => print_json(&serde_json::to_value(receipt)?),
            OutputFormat::Text => {
                let mut out = io::stdout().lock();
                writeln!(out, "Started {}", receipt.run_id)?;
                writeln!(out, "Watch: gents graph watch {}", receipt.run_id)?;
                Ok(())
            }
            _ => unreachable!("validated output format"),
        }
    }
}

fn progress(view: &GraphRunView) -> Value {
    json!({
        "run_id": view.run_id,
        "status": view.status,
        "revision_digest": view.revision_digest,
        "deadline_at": view.deadline_at,
        "active_requests": view.active_request_count,
        "terminal_requests": view.terminal_request_count,
        "stages": view.stages,
        "groups": view.groups,
        "results": view.results,
        "error": view.error,
    })
}

fn compact_count(value: i64) -> String {
    match value.max(0) {
        value if value >= 1_000_000 => format!("{:.1}m", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

fn tool_state(tool: &TimelineToolCallRow) -> (&'static str, &str) {
    let state = tool
        .lifecycle_state
        .as_deref()
        .unwrap_or(tool.status.as_str());
    let marker = match state {
        "completed" => "✓",
        "failed" | "timedOut" | "cancelled" => "×",
        _ => "●",
    };
    (marker, state)
}

fn print_progress_text(
    view: &GraphRunView,
    activity: &RunActivityRows,
    redraw: bool,
) -> Result<()> {
    let input_tokens = activity
        .inference_calls
        .iter()
        .filter_map(|call| call.prompt_tokens)
        .sum::<i64>();
    let output_tokens = activity
        .inference_calls
        .iter()
        .filter_map(|call| call.completion_tokens)
        .sum::<i64>();
    let cached_tokens = activity
        .inference_calls
        .iter()
        .filter_map(|call| call.cached_input_tokens)
        .sum::<i64>();
    let active_models = activity
        .inference_calls
        .iter()
        .filter(|call| {
            !matches!(
                call.call_state.as_str(),
                "completed" | "failed" | "cancelled"
            )
        })
        .count();
    let mut tool_counts = BTreeMap::new();
    let mut completed_tools = 0;
    let mut failed_tools = 0;
    for tool in &activity.tool_calls {
        *tool_counts.entry(tool.tool_name.as_str()).or_insert(0usize) += 1;
        match tool_state(tool).1 {
            "completed" => completed_tools += 1,
            "failed" | "timedOut" | "cancelled" => failed_tools += 1,
            _ => {}
        }
    }
    let active_tools = activity
        .tool_calls
        .len()
        .saturating_sub(completed_tools + failed_tools);
    let mut out = io::stdout().lock();
    if redraw {
        write!(out, "\x1b[2J\x1b[H")?;
    }
    writeln!(out, "Graph run {} · {}", view.run_id, view.status)?;
    writeln!(
        out,
        "Models  {} calls ({} active) · {} input · {} cached · {} output",
        activity.inference_calls.len(),
        active_models,
        compact_count(input_tokens),
        compact_count(cached_tokens),
        compact_count(output_tokens),
    )?;
    writeln!(
        out,
        "Tools   {} calls · {} completed · {} active · {} failed",
        activity.tool_calls.len(),
        completed_tools,
        active_tools,
        failed_tools,
    )?;
    writeln!(out)?;
    writeln!(out, "Stages")?;
    for stage in &view.stages {
        let completed = stage.succeeded + stage.failed;
        let marker = if stage.failed > 0 {
            "×"
        } else if stage.active > 0 {
            "●"
        } else if stage.total > 0 && completed == stage.total {
            "✓"
        } else {
            "○"
        };
        writeln!(
            out,
            "  {marker} {:<12} {completed}/{} · {} active · {} failed",
            stage.node_id, stage.total, stage.active, stage.failed
        )?;
    }
    writeln!(out)?;
    writeln!(out, "Agent sessions")?;
    if view.requests.is_empty() {
        writeln!(out, "  Waiting for the entry request")?;
    }
    for request in &view.requests {
        let session = request.session_id.as_deref().and_then(|session_id| {
            activity
                .sessions
                .iter()
                .find(|session| session.session_id == session_id)
        });
        let session_id = request.session_id.as_deref().unwrap_or("pending");
        let status = request
            .lifecycle_state
            .as_deref()
            .or_else(|| session.and_then(|session| session.status.as_deref()))
            .unwrap_or(request.status.as_str());
        let node = request.node_id.as_deref().unwrap_or("unknown");
        let calls = activity
            .inference_calls
            .iter()
            .filter(|call| call.request_id == request.request_id)
            .count();
        let tools = activity
            .tool_calls
            .iter()
            .filter(|tool| tool.request_id.as_deref() == Some(request.request_id.as_str()))
            .count();
        writeln!(
            out,
            "  {node:<12} {status:<12} · {calls} model calls · {tools} tools"
        )?;
        writeln!(
            out,
            "    session {session_id} · request {}",
            request.request_id
        )?;
    }
    if !tool_counts.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "Tool usage  {}",
            tool_counts
                .into_iter()
                .map(|(name, count)| format!("{name} {count}"))
                .collect::<Vec<_>>()
                .join(" · ")
        )?;
    }
    if !activity.tool_calls.is_empty() {
        writeln!(out)?;
        writeln!(out, "Recent tool calls")?;
        for tool in activity.tool_calls.iter().rev().take(8).rev() {
            let (marker, state) = tool_state(tool);
            let latency = tool
                .latency_ms
                .map(|value| format!(" · {value}ms"))
                .unwrap_or_default();
            let failure = tool
                .tool_failure_class
                .as_deref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            writeln!(
                out,
                "  {marker} {:<24} {state}{latency}{failure}",
                tool.tool_name
            )?;
        }
    }
    if activity.truncated {
        writeln!(
            out,
            "\nWarning: activity counts exceeded the bounded observation window"
        )?;
    }
    if let Some(error) = view.error.as_ref().or(view.failure_evidence.as_ref()) {
        writeln!(out, "\nError: {}", serde_json::to_string(error)?)?;
    }
    out.flush()?;
    Ok(())
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

fn print_result_text(view: &GraphRunView) -> Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "Run: {}", view.run_id)?;
    writeln!(out, "Status: {}", view.status)?;
    if let Some(error) = view.error.as_ref().or(view.failure_evidence.as_ref()) {
        writeln!(out, "Error: {}", serde_json::to_string_pretty(error)?)?;
    }
    let outputs = view
        .results
        .iter()
        .filter(|result| result.terminal)
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        writeln!(out, "Outputs: none declared")?;
        return Ok(());
    }
    for result in outputs {
        writeln!(out)?;
        writeln!(out, "{} ({})", result.name, result.documents.len())?;
        if let Some(violation) = result.violation.as_deref() {
            writeln!(out, "  Contract violation: {violation}")?;
        }
        for (index, document) in result.documents.iter().enumerate() {
            if result.documents.len() > 1 {
                writeln!(out, "  #{}", index + 1)?;
            }
            let Some(fields) = document.as_object() else {
                writeln!(out, "  {}", display_value(document))?;
                continue;
            };
            for (field, value) in fields {
                if field.starts_with('_') || field == "run_id" {
                    continue;
                }
                let rendered = display_value(value);
                if rendered.contains('\n') {
                    writeln!(out, "  {field}:")?;
                    for line in rendered.lines() {
                        writeln!(out, "    {line}")?;
                    }
                } else {
                    writeln!(out, "  {field}: {rendered}")?;
                }
            }
        }
    }
    Ok(())
}

async fn watch_run(
    access: &ConfigAccess,
    actor: &str,
    run_id: &str,
    interval: Duration,
    output: OutputFormat,
) -> Result<()> {
    let output =
        output.ensure_supported("graph watch", &[OutputFormat::Text, OutputFormat::Json])?;
    let mut last = Value::Null;
    let redraw = output == OutputFormat::Text && io::stdout().is_terminal();
    loop {
        let view = load_graph_run_view_with_access(access, actor, run_id).await?;
        let request_ids = view
            .requests
            .iter()
            .map(|request| request.request_id.clone())
            .collect::<Vec<_>>();
        let session_ids = view
            .requests
            .iter()
            .filter_map(|request| request.session_id.clone())
            .collect::<Vec<_>>();
        let activity = load_run_activity_rows(access, &request_ids, &session_ids).await?;
        let current = json!({ "run": progress(&view), "activity": activity });
        if current != last {
            match output {
                OutputFormat::Json => print_json(&current)?,
                OutputFormat::Text => print_progress_text(&view, &activity, redraw)?,
                _ => unreachable!("validated output format"),
            }
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
        args.output,
    )
    .await
}

async fn result(args: GraphResultArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let view = load_graph_run_view_with_access(&access, &actor, &args.run_id).await?;
    match args
        .output
        .ensure_supported("graph result", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => print_json(&json!({
            "run_id": view.run_id,
            "status": view.status,
            "revision_digest": view.revision_digest,
            "results": view.results,
            "result_refs": view.persisted_result_refs,
            "error": view.error,
        })),
        OutputFormat::Text => print_result_text(&view),
        _ => unreachable!("validated output format"),
    }
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
