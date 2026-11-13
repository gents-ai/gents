//! A UX plugin's page in a window of its own. The window loads the app
//! with a start address the UI reads, and gets only the `plugin-window`
//! capability: the client's snapshot and lifecycle, the node list, the
//! page's own plugin reads and calls, and the notifications its plugin
//! raises while this window speaks for it.
use std::fmt::Write;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub(super) const LABEL_PREFIX: &str = "gents-plugin-";
const MAX_ID: usize = 256;
const MAX_NODE: usize = 512;
const MAX_TITLE: usize = 120;

/// Sent to the main view when a plugin's own window opens or closes, so the
/// page there stands aside while the window shows it.
pub const PLUGIN_WINDOW_EVENT: &str = "desktop://plugin-window";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PluginWindowChange<'a> {
    plugin_id: &'a str,
    open: bool,
}

/// Sent to the main view with the place a plugin's page, in its own window,
/// asked to go to: that window shows the page and nothing else.
pub const PLUGIN_NAVIGATE_EVENT: &str = "desktop://plugin-navigate";
const MAX_PLACE: usize = 2048;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct PluginNavigation<'a> {
    path: &'a str,
}

/// Whether `label` is a plugin's own window.
pub(super) fn is_page_window(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

fn checked_place(from: &str, path: &str) -> Result<(), String> {
    if !is_page_window(from) {
        return Err("only a plugin's own window sends a place to the main view".into());
    }
    if !path.starts_with('/') || path.len() > MAX_PLACE {
        return Err("a place in the app is a path of at most 2048 bytes".into());
    }
    Ok(())
}

/// Opens a place in the main view, for a plugin's page in a window of its
/// own, and brings the main view forward. The path only names a place; the
/// main view reads it as it reads any of its own.
#[tauri::command]
pub fn desktop_plugin_window_navigate<R>(
    window: tauri::WebviewWindow<R>,
    path: String,
) -> Result<(), String>
where
    R: Runtime,
{
    checked_place(window.label(), &path)?;
    let app = window.app_handle();
    super::show_main_window(app);
    app.emit_to(
        "main",
        PLUGIN_NAVIGATE_EVENT,
        PluginNavigation { path: &path },
    )
    .map_err(|error| error.to_string())
}

/// A plugin's question is answered in the main view. While a plugin's own
/// window has focus the person would not see it, so the main view asks to
/// come forward; it stays where it is otherwise. Whether it came forward.
#[tauri::command]
pub fn desktop_plugin_prompt_to_main<R>(window: tauri::WebviewWindow<R>) -> Result<bool, String>
where
    R: Runtime,
{
    if window.label() != "main" {
        return Err("only the main view comes forward for a plugin's question".into());
    }
    let app = window.app_handle();
    let page_focused = app
        .webview_windows()
        .iter()
        .any(|(label, page)| is_page_window(label) && page.is_focused().unwrap_or(false));
    if page_focused {
        super::show_main_window(app);
    }
    Ok(page_focused)
}

fn tell_main<R>(app: &AppHandle<R>, plugin_id: &str, open: bool)
where
    R: Runtime,
{
    let change = PluginWindowChange { plugin_id, open };
    if let Err(error) = app.emit_to("main", PLUGIN_WINDOW_EVENT, change) {
        tracing::warn!(%error, plugin = %plugin_id, open, "could not tell the main view about a plugin window");
    }
}

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
    let place = placement_of(&app, &plugin_id).await;
    open(
        &app,
        &plugin_id,
        node_did.as_deref(),
        title.as_deref(),
        place,
    )
    .map_err(|error| {
        tracing::warn!(%error, plugin = %plugin_id, "could not open plugin window");
        error.to_string()
    })
}

fn open<R>(
    app: &AppHandle<R>,
    plugin_id: &str,
    node_did: Option<&str>,
    title: Option<&str>,
    place: Option<Placement>,
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
    // Shown once it is kept out of window tabs, so it never joins one.
    config.visible = false;
    if let Some(place) = place {
        config.width = place.width;
        config.height = place.height;
        if place.on_screen(&screens(app)) {
            config.x = Some(place.x);
            config.y = Some(place.y);
        }
    }
    let window = gents_desktop_bridge::navigation::restrict_window(
        WebviewWindowBuilder::from_config(app, &config)?,
    )
    .build()?;
    let handle = app.clone();
    let id = plugin_id.to_owned();
    let placed = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { .. } => {
            if let Some(place) = Placement::of(&placed) {
                remember(&handle, id.clone(), place);
            }
        }
        WindowEvent::Destroyed => tell_main(&handle, &id, false),
        _ => {}
    });
    tell_main(app, plugin_id, true);
    show_untabbed(&window)
}

/// Where a plugin's window last was and how large, in logical pixels, so
/// it opens there again. Kept per plugin in the app's config folder.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Placement {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

const PLACES_FILE: &str = "plugin-windows.json";
/// How much of a window must lie on a screen for its place to be kept: a
/// window last on a screen that is gone opens where the system puts it.
const VISIBLE: f64 = 80.0;

/// The places, read once and written in order: one writer at a time holds
/// the lock through its write.
static PLACES: tokio::sync::Mutex<Option<BTreeMap<String, Placement>>> =
    tokio::sync::Mutex::const_new(None);

impl Placement {
    fn of<R>(window: &tauri::WebviewWindow<R>) -> Option<Self>
    where
        R: Runtime,
    {
        let scale = window.scale_factor().ok()?;
        let at = window.outer_position().ok()?.to_logical::<f64>(scale);
        let size = window.inner_size().ok()?.to_logical::<f64>(scale);
        Some(Self {
            x: at.x,
            y: at.y,
            width: size.width,
            height: size.height,
        })
    }

    /// Whether enough of it lies on one of `screens` (x, y, width, height).
    fn on_screen(&self, screens: &[(f64, f64, f64, f64)]) -> bool {
        screens.iter().any(|&(x, y, w, h)| {
            let across = (self.x + self.width).min(x + w) - self.x.max(x);
            let down = (self.y + self.height).min(y + h) - self.y.max(y);
            across >= VISIBLE && down >= VISIBLE
        })
    }
}

fn screens<R>(app: &AppHandle<R>) -> Vec<(f64, f64, f64, f64)>
where
    R: Runtime,
{
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let scale = monitor.scale_factor();
            let at = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            (at.x, at.y, size.width, size.height)
        })
        .collect()
}

fn places_path<R>(app: &AppHandle<R>) -> Option<std::path::PathBuf>
where
    R: Runtime,
{
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(PLACES_FILE))
}

async fn loaded<R>(
    app: &AppHandle<R>,
    places: &mut Option<BTreeMap<String, Placement>>,
) -> std::path::PathBuf
where
    R: Runtime,
{
    let path = places_path(app).unwrap_or_default();
    if places.is_none() {
        let read = tokio::fs::read(&path).await.ok();
        *places = Some(
            read.and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
        );
    }
    path
}

async fn placement_of<R>(app: &AppHandle<R>, plugin_id: &str) -> Option<Placement>
where
    R: Runtime,
{
    let mut places = PLACES.lock().await;
    loaded(app, &mut places).await;
    places.as_ref()?.get(plugin_id).copied()
}

fn remember<R>(app: &AppHandle<R>, plugin_id: String, place: Placement)
where
    R: Runtime,
{
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut places = PLACES.lock().await;
        let path = loaded(&app, &mut places).await;
        let Some(map) = places.as_mut() else { return };
        map.insert(plugin_id, place);
        let Ok(bytes) = serde_json::to_vec_pretty(map) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        if let Err(error) = tokio::fs::write(&path, bytes).await {
            tracing::warn!(%error, "could not remember a plugin window's place");
        }
    });
}

/// Shows the window as a window of its own. The app lets its views join
/// window tabs (the main window's New Tab), and macOS can put any new window
/// into a tab when the person prefers tabs; a plugin's page is never a tab.
#[cfg(target_os = "macos")]
fn show_untabbed<R>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()>
where
    R: Runtime,
{
    use objc2_app_kit::{NSWindow, NSWindowTabbingMode};
    let target = window.clone();
    window.run_on_main_thread(move || {
        if let Ok(pointer) = target.ns_window() {
            // SAFETY: Tauri keeps the window's NSWindow alive while the
            // window exists, and this runs on the main thread.
            let native = unsafe { &*pointer.cast::<NSWindow>() };
            native.setTabbingMode(NSWindowTabbingMode::Disallowed);
        }
        if let Err(error) = target.show().and_then(|()| target.set_focus()) {
            tracing::warn!(%error, "could not show a plugin window");
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn show_untabbed<R>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()>
where
    R: Runtime,
{
    window.show()?;
    window.set_focus()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_place_only_while_enough_of_it_is_on_a_screen() {
        let place = |x, y| Placement {
            x,
            y,
            width: 800.0,
            height: 600.0,
        };
        let laptop = (0.0, 0.0, 1512.0, 982.0);
        let beside = (1512.0, 0.0, 1920.0, 1080.0);
        assert!(place(100.0, 100.0).on_screen(&[laptop]));
        /* on a screen that is gone */
        assert!(!place(2000.0, 100.0).on_screen(&[laptop]));
        assert!(place(2000.0, 100.0).on_screen(&[laptop, beside]));
        /* only a sliver left on the screen */
        assert!(!place(1480.0, 100.0).on_screen(&[laptop]));
        assert!(!place(100.0, 100.0).on_screen(&[]));
    }

    #[test]
    fn reads_and_writes_places_as_plain_json() {
        let mut places = BTreeMap::new();
        places.insert(
            "acme/board/page".to_owned(),
            Placement {
                x: 10.0,
                y: 20.0,
                width: 1100.0,
                height: 800.0,
            },
        );
        let bytes = serde_json::to_vec(&places).unwrap();
        let back: BTreeMap<String, Placement> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, places);
    }

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
        assert!(is_page_window(&a));
        assert!(!is_page_window("main"));
        assert!(!is_page_window("gents-view-1"));
    }

    #[test]
    fn a_window_change_reaches_the_main_view_in_its_shape() {
        assert_eq!(
            serde_json::to_value(PluginWindowChange {
                plugin_id: "acme/board",
                open: true,
            })
            .unwrap(),
            serde_json::json!({ "pluginId": "acme/board", "open": true })
        );
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
    fn only_a_plugin_window_sends_a_bounded_place_to_the_main_view() {
        let page = label_for("acme/board");
        assert!(checked_place(&page, "/sessions/s1").is_ok());
        assert!(checked_place("main", "/sessions/s1").is_err());
        assert!(checked_place("gents-view-2", "/sessions/s1").is_err());
        assert!(checked_place(&page, "sessions").is_err());
        assert!(checked_place(&page, "").is_err());
        assert!(checked_place(&page, &format!("/{}", "x".repeat(MAX_PLACE))).is_err());
        assert_eq!(
            serde_json::to_value(PluginNavigation { path: "/packs" }).unwrap(),
            serde_json::json!({ "path": "/packs" })
        );
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
            "notification:default",
        ] {
            assert!(granted.contains(&needed), "{needed}");
        }
        let notifications: serde_json::Value =
            serde_json::from_str(include_str!("../../capabilities/notifications.json")).unwrap();
        assert!(notifications["windows"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("gents-plugin-*")));
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

    #[test]
    fn a_place_goes_to_the_main_view_and_a_question_brings_it_forward() {
        let permission = include_str!("../../permissions/plugin-window.toml");
        for command in [
            "desktop_plugin_window_navigate",
            "desktop_plugin_prompt_to_main",
        ] {
            assert!(permission.contains(&format!("\"{command}\"")), "{command}");
        }
        let grants = |file: &str| -> Vec<String> {
            let path = format!("{}/capabilities/{file}.json", env!("CARGO_MANIFEST_DIR"));
            let capability: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_owned())
                .collect()
        };
        let page = grants("plugin-window");
        let main = grants("default");
        assert!(page.contains(&"plugin-window-navigate".to_owned()));
        assert!(!page.contains(&"plugin-prompt-to-main".to_owned()));
        assert!(main.contains(&"plugin-prompt-to-main".to_owned()));
        assert!(!main.contains(&"plugin-window-navigate".to_owned()));
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
