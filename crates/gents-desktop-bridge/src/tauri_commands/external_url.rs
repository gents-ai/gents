//! Opening a link in the person's own browser. Links go through the bridge
//! rather than a generic opener because a packaged build's environment cannot
//! open a working browser; the bridge strips the package's contributions first.

use crate::error::{BridgeError, BridgeErrorCode};
use crate::host_browser;

#[tauri::command]
pub async fn desktop_open_external_url(url: String) -> Result<(), BridgeError> {
    let url = url.trim();
    if !host_browser::is_openable(url) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Only web and mail links can be opened.",
        ));
    }
    host_browser::open_url(url).await.map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Could not open a browser for this link: {error}. Copy the address into your browser instead."),
        )
    })
}
