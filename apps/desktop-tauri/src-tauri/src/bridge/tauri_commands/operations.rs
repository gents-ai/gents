//! Tauri command stubs for operator-surfaces panels. Each command returns
//! an `Err` describing the panel issue that will replace it; returning an
//! error rather than `unimplemented!()` keeps the desktop backend from
//! panicking if these are accidentally invoked before the real bodies
//! land. Panel PRs replace the body with the real implementation.

use std::time::Duration;

use reqwest::Url;
use tauri::State;

use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    CascadeCancelPreview, DesktopInterruptRequest, DesktopListSubagentTreeRequest,
    DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult, SubagentTreeView,
};

const SUBAGENT_TREE_TIMEOUT: Duration = Duration::from_secs(10);

#[tauri::command]
pub(crate) async fn desktop_operations_snapshot(
    _state: State<'_, DesktopAppState>,
    _request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, String> {
    Err(
        "desktop_operations_snapshot not implemented yet; landing via panel #277 \
         (backgrounded tools / operations projection)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_list_subagent_tree(
    state: State<'_, DesktopAppState>,
    request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, String> {
    let root_request_id = request.root_request_id.trim();
    if root_request_id.is_empty() {
        return Err("rootRequestId is required".to_string());
    }
    let core = current_core(&state)
        .ok_or_else(|| "desktop bridge has not finished bootstrapping".to_string())?;

    let agent_did = match request.agent_did.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => core
            .selected_agent_did()
            .ok_or_else(|| "no agent selected; pass agentDid explicitly".to_string())?,
    };

    let graphql = core
        .graphql_for_agent(&agent_did)
        .await
        .ok_or_else(|| format!("no graphql URL configured for agent {agent_did}"))?;
    let url = subagent_tree_url(&graphql, root_request_id, &request)?;

    let client = reqwest::Client::builder()
        .timeout(SUBAGENT_TREE_TIMEOUT)
        .build()
        .map_err(|error| format!("build subagent tree http client: {error}"))?;

    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| format!("subagent tree fetch failed: {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "subagent tree fetch returned {status}: {}",
            body.trim()
        ));
    }

    response
        .json::<SubagentTreeView>()
        .await
        .map_err(|error| format!("decode subagent tree response: {error}"))
}

/// Translate the agent's GraphQL URL into the runtime's `/subagents/tree`
/// endpoint URL. Mirrors the path-stripping logic in
/// `defra_agent_desktop_core::local_runtime::runtime_status_url` but targets
/// the R5 subagent-lineage handler.
fn subagent_tree_url(
    graphql: &str,
    root_request_id: &str,
    request: &DesktopListSubagentTreeRequest,
) -> Result<Url, String> {
    let trimmed = graphql.trim();
    if trimmed.is_empty() {
        return Err("agent graphql URL is empty".to_string());
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|error| format!("agent graphql URL is not a valid URL: {error}"))?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() || path == "/api/v0" || path == "/api/v0/graphql" {
        url.set_path("/subagents/tree");
    } else if !path.ends_with("/subagents/tree") {
        url.set_path(&format!("{path}/subagents/tree"));
    }
    url.set_query(None);
    url.set_fragment(None);

    let mut pairs = url.query_pairs_mut();
    pairs.append_pair("root_request_id", root_request_id);
    if let Some(include_terminal) = request.include_terminal {
        pairs.append_pair("include_terminal", &include_terminal.to_string());
    }
    if let Some(max_depth) = request.max_depth {
        pairs.append_pair("max_depth", &max_depth.to_string());
    }
    drop(pairs);
    Ok(url)
}

#[cfg(test)]
mod subagent_tree_url_tests {
    use super::*;

    fn request(include_terminal: Option<bool>, max_depth: Option<u32>) -> DesktopListSubagentTreeRequest {
        DesktopListSubagentTreeRequest {
            root_request_id: "req-root".to_string(),
            agent_did: None,
            include_terminal,
            max_depth,
        }
    }

    #[test]
    fn strips_graphql_path_and_appends_subagents_tree() {
        let url = subagent_tree_url(
            "http://127.0.0.1:9181/api/v0/graphql",
            "req-root",
            &request(None, None),
        )
        .unwrap();
        assert_eq!(url.path(), "/subagents/tree");
        assert!(url.query().unwrap().contains("root_request_id=req-root"));
    }

    #[test]
    fn accepts_bare_host_and_defaults_scheme() {
        let url = subagent_tree_url("127.0.0.1:9181", "req-root", &request(Some(true), Some(4)))
            .unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.path(), "/subagents/tree");
        let query = url.query().unwrap();
        assert!(query.contains("include_terminal=true"));
        assert!(query.contains("max_depth=4"));
    }

    #[test]
    fn preserves_remote_host_and_port() {
        let url = subagent_tree_url(
            "https://runtime.example.com:8443/api/v0/graphql",
            "req-root",
            &request(None, None),
        )
        .unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("runtime.example.com"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.path(), "/subagents/tree");
    }

    #[test]
    fn rejects_empty_graphql_url() {
        let err = subagent_tree_url("   ", "req-root", &request(None, None)).unwrap_err();
        assert!(err.contains("empty"));
    }
}

#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    _state: State<'_, DesktopAppState>,
    _request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    Err(
        "desktop_preview_interrupt_cascade not implemented yet; landing via panel #286 \
         (cascade cancel UX)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    _state: State<'_, DesktopAppState>,
    _request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    Err(
        "desktop_interrupt_request not implemented yet; landing via panel #283 \
         (interrupt button)"
            .to_string(),
    )
}
