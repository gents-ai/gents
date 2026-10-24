use std::collections::BTreeMap;
use std::time::Duration;
use std::{io, io::IsTerminal as _, io::Write as _};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graph_package::{
    default_graph_package_install_bindings, install_loaded_graph_package,
    load_archive_graph_package_with_environment, load_installed_package_plan, prepare_entry_run,
    select_run_plan, EntryRunRequest, GraphInstallRecord, GraphPackageInstallBindings,
    GraphRunSelector,
};
use gents::graph_pipeline::{
    activate_graph_revision_with_access, load_active_graph_plan_with_access,
    load_graph_run_result_view_with_access, load_graph_run_view_with_access,
    request_graph_run_cancellation_with_access, set_graph_enabled_with_access,
    start_graph_run_with_access, GraphRunView,
};
use gents::plugin::executor::PluginExecutor;
use gents::run_timeline::{RunActivityRows, TimelineInferenceCallRow, TimelineToolCallRow};
use gents::run_timeline_fetch::load_run_activity_rows;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::{
    GraphCancelArgs, GraphCommand, GraphResultArgs, GraphRunArgs, GraphToggleArgs, GraphWatchArgs,
    PackInstallArgs, ScopeArgs,
};
use crate::{print_json, print_ndjson, resolve_config_access, resolve_node_did};

pub(crate) async fn dispatch(command: GraphCommand) -> Result<()> {
    match command {
        GraphCommand::Run(args) => run(args).await,
        GraphCommand::Watch(args) => watch(args).await,
        GraphCommand::Result(args) => result(args).await,
        GraphCommand::Cancel(args) => cancel(args).await,
        GraphCommand::Disable(args) => toggle(args, false).await,
        GraphCommand::Enable(args) => toggle(args, true).await,
    }
}

pub(crate) async fn install(
    source: &super::pack::PackSource,
    args: PackInstallArgs,
    emit_report: bool,
) -> Result<()> {
    let (access, owner_did) = access_and_actor(&args.scope).await?;
    install_with_access(&access, &owner_did, source, args, emit_report).await
}

pub(crate) async fn install_with_access(
    access: &ConfigAccess,
    owner_did: &str,
    source: &super::pack::PackSource,
    args: PackInstallArgs,
    emit_report: bool,
) -> Result<()> {
    let requested = super::pack::parse_inference_slot_bindings(&args.inference_slots)?;
    let scope = gents::pack::PackInstallOptions {
        node_did: owner_did.to_owned(),
    };
    let package = load_archive_graph_package_with_environment(source.archive(), &scope, &|name| {
        std::env::var(name).ok()
    })?;
    let mut bindings = if let Some(path) = args.bindings.as_deref() {
        let bindings: GraphPackageInstallBindings = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("reading graph package bindings {}", path.display()))?,
        )
        .with_context(|| format!("parsing graph package bindings {}", path.display()))?;
        if bindings.node_did != owner_did {
            anyhow::bail!(
                "binding owner {} does not match selected package owner {}",
                bindings.node_did,
                owner_did
            );
        }
        bindings
    } else {
        default_graph_package_install_bindings(access, &package, owner_did, &requested).await?
    };
    for (slot, profile_id) in requested {
        if let Some(existing) = bindings
            .inference_slots
            .insert(slot.clone(), profile_id.clone())
        {
            anyhow::ensure!(
                existing == profile_id,
                "inference slot {slot:?} has conflicting bindings"
            );
        }
    }
    let preview = gents::pack::preview_pack_inference_bindings(
        access,
        &package.manifest,
        owner_did,
        &bindings.inference_slots,
    )
    .await?;
    bindings.inference_slots = preview.bindings.clone();
    if args.preview {
        return Ok(print_json(&json!({
            "pack": package.manifest.name,
            "owner": owner_did,
            "inference": preview,
            "origin_tag": gents::pack::pack_origin_tag(&package.manifest.name)?,
            "would_write": false,
        }))?);
    }
    let plugin_home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let installed_plugins = if package.manifest.metadata.plugins.is_empty() {
        Vec::new()
    } else {
        anyhow::ensure!(
            args.scope.graphql.is_none(),
            "{} ships plugins, which install on the node's own host; run the install there with --home",
            args.package
        );
        gents::plugin::install::install_pack_plugins_with_bindings(
            &plugin_home,
            &package.manifest,
            &package.package_digest,
            |path| source.asset(path),
            args.grant_authority,
            Some(owner_did),
            &bindings.inference_slots,
            |_| Ok(()),
        )?
    };
    let record = GraphInstallRecord {
        plugins: installed_plugins
            .iter()
            .map(|plugin| gents::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
        explicit: args.explicit,
    };
    let receipt =
        install_loaded_graph_package(access, owner_did, &package, &bindings, None, &record)
            .await
            .context(
                "graph installation was not confirmed; locally published plugins remain installed",
            )?;
    let previous = load_active_graph_plan_with_access(access, owner_did, &receipt.graph_id)
        .await
        .context("graph installation completed, but activation lookup failed; locally published plugins remain installed")?
        .map(|plan| plan.digest);
    let activation = activate_graph_revision_with_access(
        access,
        owner_did,
        &receipt.graph_id,
        &receipt.revision_digest,
        previous.as_deref(),
    )
    .await
    .context("graph installation completed, but activation failed; locally published plugins remain installed")?;
    if !emit_report {
        return Ok(());
    }
    match args.output {
        OutputFormat::Json => Ok(print_json(&json!({
            "install": receipt,
            "activation": activation,
            "bindings": bindings,
            "external_dependencies": package.manifest.external_dependencies,
        }))?),
        OutputFormat::Text => {
            let mut out = io::stdout().lock();
            writeln!(
                out,
                "Installed and activated {} {}",
                receipt.package_name, receipt.package_version
            )?;
            for dependency in &package.manifest.external_dependencies {
                writeln!(out, "Requires: {}", dependency.service_id)?;
                writeln!(out, "  {}", dependency.description)?;
                writeln!(out, "  Repository: {}", dependency.repository_url)?;
                writeln!(out, "  After cloning: {}", dependency.install_command)?;
            }
            writeln!(out, "Run: gents graph run {}", receipt.package_name)?;
            Ok(())
        }
        _ => unreachable!("validated output format"),
    }
}

async fn access_and_actor(
    scope: &ScopeArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
    let actor = resolve_node_did(scope.home.as_deref(), scope.node_did.as_deref())?;
    let (access, _) =
        resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
    Ok((access, actor))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GraphCommandError {
    #[error("reading --input file {path}")]
    ReadInputFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing --input as JSON")]
    ParseInputJson(#[source] serde_json::Error),
    #[error("--input must be a JSON object")]
    InputNotObject,
    #[error("--field {field:?} must be NAME=VALUE")]
    FieldNotPair { field: String },
    #[error("--field {field:?} must name a field before '='")]
    FieldNameEmpty { field: String },
    #[error("graph progress has no {format} rendering")]
    ProgressFormat { format: &'static str },
    #[error(transparent)]
    SerializeProgress(serde_json::Error),
    #[error(transparent)]
    WriteProgressLine(std::io::Error),
    #[error("writing graph progress to stdout")]
    FlushProgress(#[source] std::io::Error),
}

fn parse_input_arg(raw: Option<&str>) -> Result<Value, GraphCommandError> {
    let Some(raw) = raw else {
        return Ok(json!({}));
    };
    let text = match raw.strip_prefix('@') {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|source| GraphCommandError::ReadInputFile {
                path: path.to_owned(),
                source,
            })?
        }
        None => raw.to_owned(),
    };
    let value: Value = serde_json::from_str(&text).map_err(GraphCommandError::ParseInputJson)?;
    if !value.is_object() {
        return Err(GraphCommandError::InputNotObject);
    }
    Ok(value)
}

fn apply_input_fields(mut input: Value, fields: &[String]) -> Result<Value, GraphCommandError> {
    let object = input
        .as_object_mut()
        .ok_or(GraphCommandError::InputNotObject)?;
    for field in fields {
        let (name, value) =
            field
                .split_once('=')
                .ok_or_else(|| GraphCommandError::FieldNotPair {
                    field: field.clone(),
                })?;
        if name.is_empty() {
            return Err(GraphCommandError::FieldNameEmpty {
                field: field.clone(),
            });
        }
        object.insert(name.to_owned(), Value::String(value.to_owned()));
    }
    Ok(input)
}

async fn run(args: GraphRunArgs) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let ConfigAccess::Graphql(_) = &access else {
        anyhow::bail!(
            "graph run requires the local Gents server to be running so workspace and request recovery remain active"
        );
    };
    let package = args.package.as_deref().map(super::pack::split_namespace);
    let coordinate = package.map(|(namespace, name)| format!("{namespace}/{name}"));
    let selector = match (package, args.graph_id.as_deref(), args.digest.as_deref()) {
        (Some((_, name)), None, None) => GraphRunSelector::Package {
            name,
            coordinate: coordinate.as_deref(),
        },
        (None, Some(graph_id), Some(digest)) => GraphRunSelector::Pinned { graph_id, digest },
        _ => anyhow::bail!("graph run takes PACKAGE, or --graph-id ID with --digest sha256:..."),
    };
    let plan = select_run_plan(&access, &actor, &selector)
        .await?
        .with_context(|| match selector {
            GraphRunSelector::Package { .. } => format!(
                "graph is not installed; run `gents pack install {}` first",
                args.package.as_deref().unwrap_or_default()
            ),
            GraphRunSelector::Pinned { graph_id, .. } => {
                format!("graph {graph_id:?} has no active revision for {actor}")
            }
        })?;
    let graph_id = plan.graph_id.clone();
    let digest = plan.digest.clone();
    let input = apply_input_fields(parse_input_arg(args.input.as_deref())?, &args.field)?;
    let plugin_home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let plugins = PluginExecutor::new(Some(plugin_home));
    let prepared = prepare_entry_run(
        &access,
        &actor,
        EntryRunRequest {
            plan: &plan,
            entry: args.entry.as_deref(),
            input,
            host_root: None,
            plugins: &plugins,
        },
    )
    .await?;
    let receipt = start_graph_run_with_access(
        &access,
        &actor,
        &graph_id,
        Some(&digest),
        &prepared.entry_name,
        prepared.input,
        prepared.origin,
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
        match args.output.ensure_supported(
            "graph run",
            &[OutputFormat::Text, OutputFormat::Json, OutputFormat::Ndjson],
        )? {
            OutputFormat::Json => Ok(print_json(&serde_json::to_value(receipt)?)?),
            OutputFormat::Ndjson => Ok(print_ndjson(&serde_json::to_value(receipt)?)?),
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

fn effective_error(view: &GraphRunView) -> Option<&Value> {
    view.error.as_ref().or(view.failure_evidence.as_ref())
}

fn progress(view: &GraphRunView) -> Value {
    json!({
        "run_id": view.run_id,
        "status": view.status,
        "revision_digest": view.revision_digest,
        "deadline_at": view.deadline_at,
        "active_requests": view.active_request_count,
        "outstanding_invocations": view.outstanding_invocation_count,
        "terminal_requests": view.terminal_request_count,
        "stages": view.stages,
        "groups": view.groups,
        "results": view.results,
        "error": effective_error(view),
    })
}

fn compact_count(value: i64) -> String {
    match value.max(0) {
        value if value >= 1_000_000 => format!("{:.1}m", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
struct RunUsageSummary {
    reported_input_tokens: i64,
    reported_cached_input_tokens: i64,
    reported_output_tokens: i64,
    estimated_input_tokens: i64,
    unreported_completed_calls: usize,
}

fn context_input_estimate(call: &TimelineInferenceCallRow) -> Option<i64> {
    let value: Value = serde_json::from_str(call.context_accounting_json.as_deref()?).ok()?;
    let estimate = value.get("estimated_input_tokens")?.as_u64()?;
    Some(i64::try_from(estimate).unwrap_or(i64::MAX))
}

fn run_usage_summary(calls: &[TimelineInferenceCallRow]) -> RunUsageSummary {
    let mut summary = RunUsageSummary::default();
    for call in calls {
        let input = call.prompt_tokens.unwrap_or_default().max(0);
        let cached = call.cached_input_tokens.unwrap_or_default().max(0);
        let output = call.completion_tokens.unwrap_or_default().max(0);
        if input > 0 || cached > 0 || output > 0 {
            summary.reported_input_tokens = summary.reported_input_tokens.saturating_add(input);
            summary.reported_cached_input_tokens =
                summary.reported_cached_input_tokens.saturating_add(cached);
            summary.reported_output_tokens = summary.reported_output_tokens.saturating_add(output);
        } else if call.call_state == "completed" {
            summary.unreported_completed_calls += 1;
            summary.estimated_input_tokens = summary
                .estimated_input_tokens
                .saturating_add(context_input_estimate(call).unwrap_or_default());
        }
    }
    summary
}

fn usage_detail(summary: &RunUsageSummary) -> String {
    if summary.unreported_completed_calls == 0 {
        return format!(
            "{} input · {} cached · {} output",
            compact_count(summary.reported_input_tokens),
            compact_count(summary.reported_cached_input_tokens),
            compact_count(summary.reported_output_tokens),
        );
    }
    let suffix = if summary.unreported_completed_calls == 1 {
        "1 completed call unreported".to_owned()
    } else {
        format!(
            "{} completed calls unreported",
            summary.unreported_completed_calls
        )
    };
    let estimate = if summary.estimated_input_tokens > 0 {
        format!(
            "~{} input estimated",
            compact_count(summary.estimated_input_tokens)
        )
    } else {
        "input unavailable".to_owned()
    };
    if summary.reported_input_tokens > 0
        || summary.reported_cached_input_tokens > 0
        || summary.reported_output_tokens > 0
    {
        format!(
            "{} input reported · {} cached · {} output · {estimate} · {suffix}",
            compact_count(summary.reported_input_tokens),
            compact_count(summary.reported_cached_input_tokens),
            compact_count(summary.reported_output_tokens),
        )
    } else {
        format!("{estimate} · output unavailable · {suffix}")
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
    let usage = run_usage_summary(&activity.inference_calls);
    let active_models = activity
        .inference_calls
        .iter()
        .filter(|call| {
            !matches!(
                call.call_state.as_str(),
                "failed" | "completed" | "cancelled"
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
        "Models  {} calls ({} active) · {}",
        activity.inference_calls.len(),
        active_models,
        usage_detail(&usage),
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
        let session_id = request.session_id.as_deref().unwrap_or("pending");
        let status = request.lifecycle_state.as_deref().unwrap_or("unknown");
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
    if let Some(error) = effective_error(view) {
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

fn write_result_text(out: &mut impl io::Write, view: &GraphRunView) -> Result<()> {
    writeln!(out, "Run: {}", view.run_id)?;
    writeln!(out, "Status: {}", view.status)?;
    if let Some(error) = effective_error(view) {
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

fn print_result_text(view: &GraphRunView) -> Result<()> {
    write_result_text(&mut io::stdout().lock(), view)
}

fn emit_progress(
    out: &mut impl io::Write,
    format: OutputFormat,
    value: &Value,
) -> Result<(), GraphCommandError> {
    let line = match format {
        OutputFormat::Json => serde_json::to_string_pretty(value),
        OutputFormat::Ndjson => serde_json::to_string(value),
        other => {
            return Err(GraphCommandError::ProgressFormat {
                format: other.as_str(),
            })
        }
    }
    .map_err(GraphCommandError::SerializeProgress)?;
    writeln!(out, "{line}").map_err(GraphCommandError::WriteProgressLine)?;
    out.flush().map_err(GraphCommandError::FlushProgress)
}

async fn watch_run(
    access: &ConfigAccess,
    actor: &str,
    run_id: &str,
    interval: Duration,
    output: OutputFormat,
) -> Result<()> {
    let output = output.ensure_supported(
        "graph watch",
        &[OutputFormat::Text, OutputFormat::Json, OutputFormat::Ndjson],
    )?;
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
        let usage = run_usage_summary(&activity.inference_calls);
        let current = json!({ "run": progress(&view), "activity": activity, "usage": usage });
        if current != last {
            match output {
                OutputFormat::Json | OutputFormat::Ndjson => {
                    emit_progress(&mut io::stdout().lock(), output, &current)?
                }
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
    let view = load_graph_run_result_view_with_access(&access, &actor, &args.run_id).await?;
    match args
        .output
        .ensure_supported("graph result", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => Ok(print_json(&json!({
            "run_id": view.run_id,
            "status": view.status,
            "revision_digest": view.revision_digest,
            "results": view.results,
            "result_refs": view.persisted_result_refs,
            "error": effective_error(&view),
        }))?),
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
    Ok(print_json(&progress(&view))?)
}

async fn toggle(args: GraphToggleArgs, enabled: bool) -> Result<()> {
    let (access, actor) = access_and_actor(&args.scope).await?;
    let (_, name) = super::pack::split_namespace(&args.package);
    let graph_id = load_installed_package_plan(&access, name, &actor)
        .await?
        .context("package graph is not installed")?
        .graph_id;
    set_graph_enabled_with_access(&access, &actor, &graph_id, enabled).await?;
    Ok(print_json(
        &json!({ "graph_id": graph_id, "enabled": enabled }),
    )?)
}
