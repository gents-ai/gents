//! A UX plugin's page in a window of its own. The window loads the app
//! with a start address the UI reads, and gets only the `plugin-window`
//! capability: the client's snapshot and lifecycle, the node list and the
//! page's own plugin reads and calls.
use std::fmt::Write;

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub(super) const LABEL_PREFIX: &str = "gents-plugin-";
const MAX_ID: usize = 256;
const MAX_NODE: usize = 512;
const MAX_TITLE: usize = 120;

/// Window labels allow only `a-zA-Z0-9-/:_`, and the capability's window
/// glob must match every label, so the id is spelled in hex: one window per
/// plugin, with no two ids sharing a label.
pub(super) fn label_for(plugin_id: &str) -> String {
    let mut label = String::from(LABEL_PREFIX);
    for byte in plugin_id.bytes() {
        let _ = write!(label, "{byte:02x}");
    }
    label
}

/// What the UI reads to open on the page: `view=plugin`, the plugin and
/// the node its data calls go to.
pub(super) fn start_path(plugin_id: &str, node_did: Option<&str>) -> String {
    let mut url = tauri::Url::parse("tauri://localhost/index.html").expect("static start url");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("view", "plugin");
        query.append_pair("plugin", plugin_id);
        if let Some(node) = node_did {
            query.append_pair("node", node);
        }
    }
    format!("index.html?{}", url.query().unwrap_or_default())
}

fn checked(plugin_id: &str, node_did: Option<&str>, title: Option<&str>) -> Result<(), String> {
    if plugin_id.is_empty() || plugin_id.len() > MAX_ID {
        return Err("a plugin window needs the plugin's id".into());
    }
    if node_did.is_some_and(|node| node.is_empty() || node.len() > MAX_NODE) {
        return Err("the node for a plugin window is not a node id".into());
    }
    if title.is_some_and(|title| title.len() > MAX_TITLE) {
        return Err("the plugin window's title is too long".into());
    }
    Ok(())
}

// Async: building a window inside a synchronous command deadlocks on Windows.
#[tauri::command]
pub async fn desktop_open_plugin_window<R>(
    app: AppHandle<R>,
    plugin_id: String,
    node_did: Option<String>,
    title: Option<String>,
) -> Result<(), String>
where
    R: Runtime,
{
    checked(&plugin_id, node_did.as_deref(), title.as_deref())?;
    open(&app, &plugin_id, node_did.as_deref(), title.as_deref()).map_err(|error| {
        tracing::warn!(%error, plugin = %plugin_id, "could not open plugin window");
        error.to_string()
    })
}

fn open<R>(
    app: &AppHandle<R>,
    plugin_id: &str,
    node_did: Option<&str>,
    title: Option<&str>,
) -> tauri::Result<()>
where
    R: Runtime,
{
    let label = label_for(plugin_id);
    if let Some(window) = app.get_webview_window(&label) {
        window.unminimize()?;
        window.show()?;
        return window.set_focus();
    }
    let mut config = app
        .config()
        .app
        .windows
        .first()
        .cloned()
        .unwrap_or_default();
    config.label = label;
    config.url = WebviewUrl::App(start_path(plugin_id, node_did).into());
    config.title = title.unwrap_or("Gents").to_owned();
    config.width = 1100.0;
    config.height = 800.0;
    config.min_width = Some(480.0);
    config.min_height = Some(400.0);
    // The page has no window bar of its own: the system draws the title
    // and the controls, on every platform.
    config.decorations = true;
    config.hidden_title = false;
    config.tabbing_identifier = None;
    config.create = false;
    config.visible = true;
    let window = gents_desktop_bridge::navigation::restrict_window(
        WebviewWindowBuilder::from_config(app, &config)?,
    )
    .build()?;
    window.set_focus()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_plugin_gets_its_own_label_inside_the_capability_glob() {
        let a = label_for("acme/board/page");
        let b = label_for("acme/board-page");
        assert_ne!(a, b);
        for label in [&a, &b] {
            assert!(label.starts_with(LABEL_PREFIX));
            assert!(label[LABEL_PREFIX.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()));
        }
        assert_eq!(label_for("a/b"), "gents-plugin-612f62");
    }

    #[test]
    fn start_path_carries_the_plugin_and_node_encoded() {
        assert_eq!(
            start_path("acme/board & co", Some("did:key:z6Mk")),
            "index.html?view=plugin&plugin=acme%2Fboard+%26+co&node=did%3Akey%3Az6Mk"
        );
        assert_eq!(start_path("x", None), "index.html?view=plugin&plugin=x");
    }

    #[test]
    fn requests_are_bounded() {
        assert!(checked("", None, None).is_err());
        assert!(checked(&"x".repeat(MAX_ID + 1), None, None).is_err());
        assert!(checked("x", Some(""), None).is_err());
        assert!(checked("x", None, Some(&"t".repeat(MAX_TITLE + 1))).is_err());
        assert!(checked("x", Some("did:key:z"), Some("Board")).is_ok());
    }

    #[test]
    fn plugin_windows_get_only_the_page_surface() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../../capabilities/plugin-window.json")).unwrap();
        assert_eq!(capability["windows"], serde_json::json!(["gents-plugin-*"]));
        let granted: Vec<&str> = capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        for needed in [
            "gents-desktop-bridge:default",
            "gents-desktop-bridge:ux-plugins-read",
            "gents-desktop-bridge:ux-plugins-call",
        ] {
            assert!(granted.contains(&needed), "{needed}");
        }
        for withheld in [
            "gents-desktop-bridge:full",
            "gents-desktop-bridge:runtime-admin",
            "gents-desktop-bridge:config-write",
            "gents-desktop-bridge:chat-write",
            "gents-desktop-bridge:packs-admin",
            "gents-desktop-bridge:allowed-dirs-admin",
            "gents-desktop-bridge:fleet-admin",
        ] {
            assert!(!granted.contains(&withheld), "{withheld}");
        }
        for other in ["default", "desktop-runtime-admin", "native-windows"] {
            let path = format!("{}/capabilities/{other}.json", env!("CARGO_MANIFEST_DIR"));
            let wider: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert!(!wider["windows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|window| window.as_str().unwrap().starts_with("gents-plugin")));
        }
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert!(config["app"]["security"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("plugin-window")));
    }

    /* the app has permissions of its own, so a command none of them allows
    is refused at runtime ("not allowed. Command not found") */
    #[test]
    fn the_main_view_may_open_a_plugin_window() {
        let permission = include_str!("../../permissions/plugin-window.toml");
        assert!(permission.contains("identifier = \"open-plugin-window\""));
        assert!(permission.contains("\"desktop_open_plugin_window\""));
        let main: serde_json::Value =
            serde_json::from_str(include_str!("../../capabilities/default.json")).unwrap();
        assert!(main["windows"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("main")));
        assert!(main["permissions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("open-plugin-window")));
    }
}
