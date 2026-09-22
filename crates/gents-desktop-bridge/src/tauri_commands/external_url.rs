//! Opening a link from the interface in the person's own browser.
//!
//! The generic opener inherits this process's environment. Inside a packaged
//! build that environment points at the package's own libraries, data
//! directories and display backend, so the link either opens a browser that
//! cannot load or opens nothing at all. Links go through the bridge instead,
//! which strips the package's contributions first.

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
    host_browser::open_url(url).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Could not open a browser for this link: {error}. Copy the address into your browser instead."),
        )
    })
}
