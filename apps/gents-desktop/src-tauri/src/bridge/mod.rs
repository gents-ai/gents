#[cfg(target_os = "macos")]
mod browser;
#[cfg(desktop)]
mod links;
#[cfg(target_os = "macos")]
mod notifications;
#[cfg(desktop)]
mod plugin_window;
mod product_telemetry;
#[cfg(target_os = "macos")]
mod swipe;
#[cfg(desktop)]
mod window_close;
#[cfg(target_os = "macos")]
mod windows;

#[cfg(desktop)]
use gents_desktop_bridge::contract::{
    MANAGED_SERVER_TRAY_RESTART_EVENT, MANAGED_SERVER_TRAY_START_EVENT,
    MANAGED_SERVER_TRAY_STOP_EVENT, MANAGED_SERVER_UPDATED_EVENT,
};
#[cfg(all(feature = "native-e2e", debug_assertions))]
use gents_desktop_bridge::tauri_commands::e2e::NativeE2eLaunch;
#[cfg(any(
    all(feature = "native-e2e", debug_assertions, desktop),
    all(desktop, any(target_os = "macos", target_os = "linux"))
))]
use gents_desktop_bridge::NodeHomePolicy;
use gents_desktop_bridge::{
    init, init_tracing as install_tracing, install_runtime, prefer_host_tools, AppMeta,
    BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, SnapshotGrants, TracingConfig,
};
#[cfg(desktop)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(desktop)]
use std::sync::Arc;
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem};
#[cfg(desktop)]
use tauri::tray::TrayIconBuilder;
use tauri::Manager;
#[cfg(desktop)]
use tauri::{Emitter, Listener};

/// On a phone the store's runtimes start before anything starts them on
/// first use, set for a device that mostly idles: the async runtime without
/// its handoff monitor, which wakes every tick to look for a blocked poll (a
/// task that blocks then holds up only the other tasks of its worker), and
/// the default worker pool on the phone's settings, returned for the app to
/// hold.
#[cfg(mobile)]
fn start_phone_runtimes() -> Option<workers::Pool> {
    if let Err(error) = nuclei::runtime::Builder::new()
        .handoff_after(None)
        .build_global()
    {
        tracing::warn!(%error, "the store's async runtime kept its handoff monitor");
    }
    gents_desktop_core::client::phone::start_default_pool()
        .inspect_err(|error| tracing::warn!(%error, "the default worker pool starts on first use"))
        .ok()
}

/// The app's Tauri context. It expands once per crate, since it embeds the
/// macOS Info.plist under a fixed symbol.
pub(crate) fn context() -> tauri::Context<tauri::Wry> {
    tauri::generate_context!()
}

pub fn run() {
    install_tracing(TracingConfig {
        filter: None,
        console: std::env::args_os()
            .skip(1)
            .any(|argument| argument == "--console-log"),
    });
    // Before anything spawns a helper: a packaged launch puts its own copies
    // of the host's tools first on PATH, and an older bundled `xdg-open` can
    // open no browser at all while still reporting success.
    prefer_host_tools();
    install_runtime();
    #[cfg(target_os = "ios")]
    crate::backup_exclusion::exclude_app_data();
    #[cfg(mobile)]
    let phone_pool = start_phone_runtimes();

    #[cfg(all(feature = "native-e2e", debug_assertions))]
    let native_e2e = native_e2e_launch();
    #[cfg(all(feature = "native-e2e", debug_assertions, target_os = "macos"))]
    let webview_store_id = native_e2e.as_ref().map(|launch| {
        launch
            .webview_store_id
            .clone()
            .expect("a macOS --native-e2e launch names its isolated webviewStoreId")
    });
    #[cfg(all(
        not(all(feature = "native-e2e", debug_assertions)),
        target_os = "macos"
    ))]
    let webview_store_id: Option<String> = None;

    let context = context();
    #[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
    let context = native_local_e2e_context(context, native_e2e.as_ref());
    let mut context = context;
    let initial_windows: Vec<_> = context
        .config()
        .app
        .windows
        .iter()
        .filter(|window| window.create)
        .cloned()
        .collect();
    // Automatic creation bypasses the native new-window handler. Keep every
    // platform's configured initial windows, but build them through that owner.
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        .plugin(single_instance())
        .plugin(tauri_plugin_deep_link::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(browser_store_lock(webview_store_id));
    let bridge_config = platform_bridge_config();
    #[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
    let bridge_config = native_local_e2e_config(bridge_config, native_e2e.as_ref());
    let builder = builder
        .setup(move |app| {
            #[cfg(all(feature = "native-e2e", debug_assertions))]
            if let Some(launch) = native_e2e {
                app.manage(launch);
            }
            #[cfg(mobile)]
            if let Some(pool) = phone_pool {
                app.manage(pool);
            }
            let telemetry_path = app
                .path()
                .app_config_dir()?
                .join("product-telemetry-consent");
            app.manage(product_telemetry::ProductTelemetry::new(telemetry_path));
            for config in &initial_windows {
                gents_desktop_bridge::navigation::restrict_window(
                    tauri::WebviewWindowBuilder::from_config(app.handle(), config)?,
                )
                .build()?;
            }
            #[cfg(desktop)]
            {
                links::setup(app)?;
                plugin_window::setup(app);
                setup_tray(app)?;
                #[cfg(target_os = "macos")]
                windows::setup(app)?;
                #[cfg(target_os = "macos")]
                swipe::setup(app.handle().clone());
            }
            Ok(())
        })
        .plugin(tauri_plugin_fs::init())
        .plugin(init(bridge_config))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init());
    #[cfg(target_os = "macos")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        windows::desktop_window_setup_complete,
        swipe::swipe_scroll_edges,
        links::desktop_take_links,
        plugin_window::desktop_open_plugin_window,
        plugin_window::desktop_plugin_window_navigate,
        plugin_window::desktop_plugin_prompt_to_main,
        notifications::desktop_post_notification,
        browser::browser_open,
        browser::browser_select,
        browser::browser_place,
        browser::browser_hide,
        browser::browser_go,
        browser::browser_close_tab,
        browser::browser_close,
        product_telemetry::product_telemetry_app_opened,
        product_telemetry::product_telemetry_status,
        product_telemetry::product_telemetry_set_enabled
    ]);
    #[cfg(all(desktop, not(target_os = "macos")))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        links::desktop_take_links,
        plugin_window::desktop_open_plugin_window,
        plugin_window::desktop_plugin_window_navigate,
        plugin_window::desktop_plugin_prompt_to_main,
        product_telemetry::product_telemetry_app_opened,
        product_telemetry::product_telemetry_status,
        product_telemetry::product_telemetry_set_enabled
    ]);
    #[cfg(not(desktop))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        product_telemetry::product_telemetry_app_opened,
        product_telemetry::product_telemetry_status,
        product_telemetry::product_telemetry_set_enabled
    ]);
    #[cfg(desktop)]
    let builder = builder.on_window_event(|window, event| {
        if matches!(event, tauri::WindowEvent::Focused(true))
            && window.label() == "main"
            && matches!(window.is_visible(), Ok(true))
        {
            if let Some(state) = window.app_handle().try_state::<TrayRuntimeState>() {
                state.main_hidden_by_close.store(false, Ordering::SeqCst);
            }
        }
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            let active = window
                .app_handle()
                .try_state::<TrayRuntimeState>()
                .is_some_and(|state| state.active.load(Ordering::SeqCst));
            let app = window.app_handle();
            let main = app.get_webview_window("main");
            let other_views = app
                .webview_windows()
                .keys()
                .any(|label| label != "main" && label != window.label());
            let main_hidden = main
                .as_ref()
                .is_some_and(|view| main_closed_out_of_sight(app, view));
            let action = window_close::close_action(
                cfg!(target_os = "macos"),
                window.label() == "main",
                plugin_window::is_page_window(window.label()),
                active,
                other_views,
                main_hidden,
            );
            tracing::debug!(
                label = window.label(),
                active,
                other_views,
                main_hidden,
                ?action,
                "desktop window close requested"
            );
            match action {
                window_close::CloseAction::Hide => {
                    api.prevent_close();
                    #[cfg(target_os = "macos")]
                    if let Some(view) = window.app_handle().get_webview_window(window.label()) {
                        let _ = windows::detach(&view);
                    }
                    if window.hide().is_ok() && window.label() == "main" {
                        if let Some(state) = app.try_state::<TrayRuntimeState>() {
                            state.main_hidden_by_close.store(true, Ordering::SeqCst);
                        }
                    }
                }
                window_close::CloseAction::CloseWithHiddenMain => {
                    if let Some(main) = main {
                        if let Err(error) = main.destroy() {
                            tracing::warn!(%error, "could not close hidden main view");
                        }
                    }
                }
                window_close::CloseAction::Close => {}
            }
        }
    });
    builder
        .build(context)
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = event
            {
                show_main_window(app);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

/// Decodes a native-e2e build's `--native-e2e <json>` launch argument; a
/// malformed one stops the launch rather than opening user data.
#[cfg(all(feature = "native-e2e", debug_assertions))]
fn native_e2e_launch() -> Option<NativeE2eLaunch> {
    launch_argument("--native-e2e").map(|raw| {
        let raw = raw
            .into_string()
            .expect("--native-e2e is a UTF-8 JSON value");
        NativeE2eLaunch::parse(&raw).expect("valid --native-e2e launch configuration")
    })
}

#[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
fn native_local_e2e_context(
    mut context: tauri::Context<tauri::Wry>,
    launch: Option<&NativeE2eLaunch>,
) -> tauri::Context<tauri::Wry> {
    if let Some(launch) = launch.filter(|launch| launch.local.is_some()) {
        let identifier =
            crate::browser_data::isolated_store_identifier(launch.webview_store_id.as_deref())
                .expect("valid isolated WebView store identifier")
                .expect("native E2E requires an isolated WebView store");
        for window in &mut context.config_mut().app.windows {
            window.data_store_identifier = Some(identifier);
        }
    }
    context
}

/// Held for the process lifetime from before any webview opens the shared
/// WebKit store, so no reset clears it under this desktop.
#[cfg(target_os = "macos")]
fn browser_store_lock<R>(webview_store_id: Option<String>) -> tauri::plugin::TauriPlugin<R>
where
    R: tauri::Runtime,
{
    tauri::plugin::Builder::new("gents-browser-store")
        .setup(move |app, _api| {
            app.manage(crate::browser_data::lock_store(
                webview_store_id.as_deref(),
            )?);
            Ok(())
        })
        .build()
}

#[cfg(desktop)]
fn single_instance<R>() -> tauri::plugin::TauriPlugin<R>
where
    R: tauri::Runtime,
{
    tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app))
}

/// The value after `flag` on the launch command line.
fn launch_argument(flag: &str) -> Option<std::ffi::OsString> {
    std::env::args_os()
        .skip(1)
        .skip_while(|arg| arg != flag)
        .nth(1)
}

/// `--desktop-home <PATH>` keeps the desktop client state in PATH instead of
/// the platform's application data folder.
fn launch_home_policy() -> HomePolicy {
    launch_argument("--desktop-home").map_or(HomePolicy::Default, |root| {
        HomePolicy::FixedRoot(root.into())
    })
}

fn platform_bridge_config() -> BridgeConfig {
    BridgeConfig {
        home: launch_home_policy(),
        bootstrap: platform_bootstrap_policy(),
        app_meta: AppMeta {
            app_name: "Gents".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
        snapshot_grants: SnapshotGrants::all(),
        managed_server: platform_managed_server_policy(),
        client_host: cfg!(all(desktop, any(unix, windows))),
        store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::default(),
        service_executable: launch_argument("--gents-bin").map(Into::into),
    }
}

#[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
fn native_local_e2e_config(config: BridgeConfig, launch: Option<&NativeE2eLaunch>) -> BridgeConfig {
    let Some(local) = launch.and_then(|launch| launch.local.as_ref()) else {
        return config;
    };
    BridgeConfig {
        bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
            node_home: NodeHomePolicy::Fixed(local.node_home.clone()),
        },
        store_key_custody: if local.file_store_keys {
            gents_store::local::store_key::StoreKeyCustodyChoice::File
        } else {
            config.store_key_custody
        },
        ..config
    }
}

#[cfg(all(desktop, any(target_os = "macos", target_os = "linux")))]
fn platform_bootstrap_policy() -> BootstrapPolicy {
    BootstrapPolicy::LocalRuntimeAllowed {
        node_home: NodeHomePolicy::Default,
    }
}

#[cfg(any(
    mobile,
    all(desktop, not(any(target_os = "macos", target_os = "linux")))
))]
fn platform_bootstrap_policy() -> BootstrapPolicy {
    BootstrapPolicy::PairedRemoteOnly
}

#[cfg(all(desktop, any(target_os = "macos", target_os = "linux")))]
fn platform_managed_server_policy() -> ManagedServerPolicy {
    ManagedServerPolicy::Allowed
}

#[cfg(any(
    mobile,
    all(desktop, not(any(target_os = "macos", target_os = "linux")))
))]
fn platform_managed_server_policy() -> ManagedServerPolicy {
    ManagedServerPolicy::Disabled
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;

    #[test]
    fn native_tabs_reserve_chrome_outside_the_webview() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let main = &config["app"]["windows"][0];
        // In Tauri, Transparent disables FullSizeContentView, unlike Overlay
        // (and even Visible). AppKit then owns the content viewport on resize,
        // tab attach/detach, and fullscreen transitions.
        assert_eq!(main["titleBarStyle"], "Transparent");
        assert!(main.get("trafficLightPosition").is_none());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn supported_desktop_build_explicitly_owns_local_runtime_authority() {
        let config = platform_bridge_config();
        assert!(matches!(
            config.bootstrap,
            BootstrapPolicy::LocalRuntimeAllowed { .. }
        ));
        assert_eq!(config.managed_server, ManagedServerPolicy::Allowed);
    }

    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn unsupported_desktop_build_is_remote_only() {
        let config = platform_bridge_config();
        assert!(matches!(
            config.bootstrap,
            BootstrapPolicy::PairedRemoteOnly
        ));
        assert_eq!(config.managed_server, ManagedServerPolicy::Disabled);
    }
}

#[cfg(desktop)]
struct TrayRuntimeState {
    active: Arc<AtomicBool>,
    // Native non-selected tabs can also report invisible; only a deliberate
    // close makes the original view eligible for last-sibling cleanup.
    main_hidden_by_close: AtomicBool,
}

#[cfg(desktop)]
fn tray_icon<'a>(
    app: &'a tauri::App,
) -> Result<tauri::image::Image<'a>, Box<dyn std::error::Error>> {
    if cfg!(target_os = "macos") {
        let bytes = include_bytes!("../../icons/gents-app-icon-tray@2x.png");
        return Ok(tauri::image::Image::from_bytes(bytes)?);
    }
    Ok(app
        .default_window_icon()
        .cloned()
        .ok_or("missing application icon")?)
}

#[cfg(desktop)]
fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "Open Gents", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "Start Node", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop Node", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Restart Node", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Desktop", true, None::<&str>)?;
    let menu = if cfg!(any(target_os = "macos", target_os = "linux")) {
        Menu::with_items(app, &[&show, &start, &stop, &restart, &quit])?
    } else {
        Menu::with_items(app, &[&show, &quit])?
    };
    app.manage(TrayRuntimeState {
        active: Arc::new(AtomicBool::new(true)),
        main_hidden_by_close: AtomicBool::new(false),
    });
    let tray = TrayIconBuilder::with_id("gents-managed-server")
        .menu(&menu)
        .tooltip("Gents local node")
        .icon(tray_icon(app)?)
        .icon_as_template(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "start" => {
                let _ = app.emit_to("main", MANAGED_SERVER_TRAY_START_EVENT, ());
            }
            "stop" => {
                let _ = app.emit_to("main", MANAGED_SERVER_TRAY_STOP_EVENT, ());
            }
            "restart" => {
                let _ = app.emit_to("main", MANAGED_SERVER_TRAY_RESTART_EVENT, ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    tray.set_visible(true)?;

    let tray_id = tray.id().clone();
    let app_handle = app.handle().clone();
    app.listen(MANAGED_SERVER_UPDATED_EVENT, move |event| {
        let state = serde_json::from_str::<serde_json::Value>(event.payload())
            .ok()
            .and_then(|value| {
                value
                    .get("state")
                    .and_then(|state| state.as_str())
                    .map(str::to_string)
            });
        if let Some(tray) = app_handle.tray_by_id(&tray_id) {
            let _ = tray.set_tooltip(Some(managed_agent_tooltip(state.as_deref())));
        }
    });
    Ok(())
}

#[cfg(desktop)]
fn managed_agent_tooltip(state: Option<&str>) -> &'static str {
    match state {
        Some("running") => "Gents node service — running",
        Some("starting") => "Gents node service — starting",
        Some("stopped") | Some("disabled") => "Gents node service — stopped",
        Some("external") => "Gents node — running outside the managed service",
        Some("failed") | None => "Gents node service — status unavailable",
        Some(_) => "Gents node service — status unavailable",
    }
}

#[cfg(desktop)]
fn main_closed_out_of_sight<R>(app: &tauri::AppHandle<R>, main: &tauri::WebviewWindow<R>) -> bool
where
    R: tauri::Runtime,
{
    matches!(main.is_visible(), Ok(false))
        && app
            .try_state::<TrayRuntimeState>()
            .is_some_and(|state| state.main_hidden_by_close.load(Ordering::SeqCst))
}

#[cfg(desktop)]
fn show_main_window<R>(app: &tauri::AppHandle<R>)
where
    R: tauri::Runtime,
{
    if let Some(window) = app.get_webview_window("main") {
        if window.show().is_ok() {
            if let Some(state) = app.try_state::<TrayRuntimeState>() {
                state.main_hidden_by_close.store(false, Ordering::SeqCst);
            }
        }
        let _ = window.set_focus();
    }
}
