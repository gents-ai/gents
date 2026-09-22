//! Developer option: open the managed runtime's DB Explorer window.
//!
//! `gents serve` mounts the vendored DefraDB Explorer at `/explorer/` on the
//! same listener as its DefraDB HTTP API, so the window is same-origin with
//! the database it browses and needs no CORS or IPC access.

use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindowBuilder};

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;

const DB_EXPLORER_WINDOW_LABEL: &str = "db-explorer";

#[tauri::command]
pub async fn desktop_open_db_explorer<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<String, BridgeError> {
    let status = super::managed_server::managed_server_status_for(&app, &state).await?;
    let graphql = status
        .graphql
        .as_deref()
        .map(str::trim)
        .filter(|graphql| !graphql.is_empty())
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "the managed runtime is not running, so its DB explorer has no address",
            )
        })?;
    let url = explorer_url_from_graphql(graphql)?;
    open_db_explorer_window(&app, &url)?;
    Ok(url)
}

/// `http://host:port/api/v0/graphql` → `http://host:port/explorer/`.
fn explorer_url_from_graphql(graphql: &str) -> Result<String, BridgeError> {
    let parsed = reqwest::Url::parse(graphql).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("managed runtime GraphQL endpoint is not a valid URL: {error}"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!(
                "managed runtime GraphQL endpoint has unsupported scheme {}",
                parsed.scheme()
            ),
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "managed runtime GraphQL endpoint has no host",
        )
    })?;
    let mut url = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        url.push_str(&format!(":{port}"));
    }
    url.push_str("/explorer/");
    Ok(url)
}

fn open_db_explorer_window<R: Runtime>(app: &AppHandle<R>, url: &str) -> Result<(), BridgeError> {
    let parsed: tauri::Url = url
        .parse()
        .map_err(|error| BridgeError::new(BridgeErrorCode::InvalidArgument, format!("{error}")))?;
    if let Some(existing) = app.get_webview_window(DB_EXPLORER_WINDOW_LABEL) {
        // The runtime address can change between runs; re-point the window.
        existing
            .navigate(parsed)
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error}")))?;
        existing
            .set_focus()
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error}")))?;
        return Ok(());
    }
    WebviewWindowBuilder::new(app, DB_EXPLORER_WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("DB Explorer")
        .inner_size(1280.0, 860.0)
        .build()
        .map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::Backend,
                format!("opening the DB explorer window failed: {error}"),
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::explorer_url_from_graphql;

    #[test]
    fn derives_explorer_url_from_graphql_endpoint() {
        assert_eq!(
            explorer_url_from_graphql("http://127.0.0.1:9181/api/v0/graphql").unwrap(),
            "http://127.0.0.1:9181/explorer/"
        );
        assert_eq!(
            explorer_url_from_graphql("http://100.73.235.38:9291/api/v0/graphql").unwrap(),
            "http://100.73.235.38:9291/explorer/"
        );
        assert_eq!(
            explorer_url_from_graphql("https://runtime.example/api/v0/graphql").unwrap(),
            "https://runtime.example/explorer/"
        );
    }

    #[test]
    fn rejects_non_http_and_invalid_endpoints() {
        assert!(explorer_url_from_graphql("ftp://127.0.0.1/api/v0/graphql").is_err());
        assert!(explorer_url_from_graphql("not a url").is_err());
        assert!(explorer_url_from_graphql("file:///etc/passwd").is_err());
    }
}
