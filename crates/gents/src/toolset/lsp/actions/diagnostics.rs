use anyhow::Context as _;
use serde_json::{json, Value};

use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::shared::{ToolContext, ToolError};

use super::super::auth::LspAction;
use super::super::catalog::{family_eligible, marker_matches, CatalogServer};
use super::super::client::LspClient;
use super::format::{format_result_with_limit, truncate_model_output};
use super::{arg_invalid, language_id, ActionRequest, MAX_GLOB_TARGETS};

pub(super) async fn glob_action(
    context: &ToolContext,
    client: &LspClient,
    config: &super::super::LspToolConfig,
    servers: &[CatalogServer],
    req: &ActionRequest,
    pattern: &str,
    timeout: std::time::Duration,
) -> Result<String, ToolError> {
    let action = req.action;
    if !matches!(action, LspAction::Diagnostics) {
        return Err(arg_invalid("globs are only valid for diagnostics"));
    }
    let runner = crate::toolset::native_runner::NativeFsRunner::new(context);
    let paths = runner.glob_paths(pattern, MAX_GLOB_TARGETS).await?;
    if paths.is_empty() {
        return Ok("no files matched the diagnostic glob".into());
    }
    let mut sections = Vec::new();
    let selected_server = servers
        .iter()
        .find(|server| server.name == client.server_name);
    for path in paths
        .into_iter()
        .filter(|path| {
            selected_server
                .is_none_or(|server| super::super::catalog::file_type_matches(server, path))
        })
        .take(MAX_GLOB_TARGETS)
    {
        let display = context.display_path(&path);
        let uri = super::super::uri::path_to_file_uri(&path);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading matched diagnostic file {}", path.display()))?;
        client
            .ensure_open(&uri, &language_id(servers, &path), &text)
            .await
            .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
        let result = client
            .request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
            )
            .await
            .unwrap_or(Value::Null);
        let mut section = format!(
            "{display}:\n{}",
            format_result_with_limit(context, action, result, req.limit)
        );
        if let Some(linter) = run_linter_diagnostics(config, servers, &path, timeout).await {
            section = format!("{section}\n{linter}");
        }
        sections.push(section);
    }
    if sections.is_empty() {
        return Ok("no files matched the selected language server".into());
    }
    Ok(truncate_model_output(sections.join("\n")))
}

pub(crate) async fn run_linter_diagnostics(
    config: &super::super::LspToolConfig,
    servers: &[CatalogServer],
    path: &std::path::Path,
    timeout: std::time::Duration,
) -> Option<String> {
    if super::super::artifact_scope_denied() {
        return Some(super::super::ARTIFACT_SCOPE_DENIAL.into());
    }
    let workspace = super::super::overlay_workspace_or(&config.workspace);
    let constraints = super::super::overlay_lsp_constraints(&config.constraints);
    let linter = servers.iter().find(|server| {
        server.is_linter
            && marker_matches(&workspace, &server.root_markers)
            && family_eligible(server, &workspace)
            && super::super::catalog::file_type_matches(server, path)
    })?;
    let argv = match linter.name.as_str() {
        "biome" => vec![
            linter.command.clone(),
            "check".into(),
            "--reporter=json".into(),
            path.to_string_lossy().into_owned(),
        ],
        "swiftlint" => vec![
            linter.command.clone(),
            "lint".into(),
            "--quiet".into(),
            path.to_string_lossy().into_owned(),
        ],
        _ => return None,
    };
    let command =
        crate::toolset::prepare_managed_command(&workspace, &argv[0], &argv[1..], &constraints)
            .await
            .ok()?;
    let mut full = vec![command.program.to_string_lossy().into_owned()];
    full.extend(command.args);
    // Best-effort supplementary probe; deliberately does not thread
    // live_output, which would interleave per-file linter noise into the
    // enclosing tool call's live output.
    let bounds = crate::tool_call_lifecycle::runtime::tool_execution_bounds(timeout);
    let outcome = crate::managed_exec::run_managed_exec(crate::managed_exec::ManagedExecRequest {
        argv: full,
        cwd: workspace,
        deadline_at: bounds.deadline_at,
        cancellation_token: bounds.cancellation_token,
        max_output_bytes: 64 * 1024,
        stdin: Vec::new(),
        environment: Some(command.environment),
        tool_name: Some("lsp".into()),
        live_output: None,
    })
    .await;
    match outcome {
        crate::managed_exec::ManagedExecOutcome::Exited { stdout, .. } => {
            let text = String::from_utf8_lossy(&stdout);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!("{}: {trimmed}", linter.name))
            }
        }
        _ => None,
    }
}
