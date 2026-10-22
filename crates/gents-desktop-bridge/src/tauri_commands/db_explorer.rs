//! The window is same-origin with the DefraDB HTTP API, so it needs no CORS or IPC access.

use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindowBuilder};

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;

const DB_EXPLORER_WINDOW_LABEL: &str = "db-explorer";

#[tauri::command]
pub async fn desktop_open_db_explorer<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<String, BridgeError>
where
    R: Runtime,
{
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

fn open_db_explorer_window<R>(app: &AppHandle<R>, url: &str) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let parsed: tauri::Url = url
        .parse()
        .map_err(|error| BridgeError::new(BridgeErrorCode::InvalidArgument, format!("{error}")))?;
    if let Some(existing) = app.get_webview_window(DB_EXPLORER_WINDOW_LABEL) {
        existing
            .navigate(parsed)
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error}")))?;
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        existing
            .set_focus()
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error}")))?;
        return Ok(());
    }
    let builder =
        WebviewWindowBuilder::new(app, DB_EXPLORER_WINDOW_LABEL, WebviewUrl::External(parsed));
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    let builder = builder.title("DB Explorer").inner_size(1280.0, 860.0);
    builder.build().map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("opening the DB explorer window failed: {error}"),
        )
    })?;
    Ok(())
}
