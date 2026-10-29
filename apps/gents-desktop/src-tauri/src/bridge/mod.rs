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

pub fn run() {
    install_tracing(TracingConfig {
        filter: None,
        console: std::env::var("GENTS_DESKTOP_CONSOLE_LOG")
            .ok()
            .is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            }),
    });
    // Before anything spawns a helper: a packaged launch puts its own copies
    // of the host's tools first on PATH, and an older bundled `xdg-open` can
    // open no browser at all while still reporting success.
    prefer_host_tools();
    install_runtime();

    let context = tauri::generate_context!();
    #[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
    let context = native_local_e2e_context(context);
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
    let builder = builder.plugin(single_instance());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(browser_store_lock());
    let builder = builder
        .setup(move |app| {
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
                setup_tray(app)?;
                #[cfg(target_os = "macos")]
                windows::setup(app)?;
                #[cfg(target_os = "macos")]
                swipe::setup(app.handle().clone());
            }
            Ok(())
        })
        .plugin(tauri_plugin_fs::init())
        .plugin(init(platform_bridge_config()))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());
    #[cfg(target_os = "macos")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        windows::desktop_window_setup_complete,
        swipe::swipe_scroll_edges,
        product_telemetry::product_telemetry_app_opened,
        product_telemetry::product_telemetry_status,
        product_telemetry::product_telemetry_set_enabled
    ]);
    #[cfg(not(target_os = "macos"))]
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
                .is_some_and(|view| matches!(view.is_visible(), Ok(false)))
                && app
                    .try_state::<TrayRuntimeState>()
                    .is_some_and(|state| state.main_hidden_by_close.load(Ordering::SeqCst));
            let action = window_close::close_action(
                cfg!(target_os = "macos"),
                window.label() == "main",
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

#[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
fn native_local_e2e_context(mut context: tauri::Context<tauri::Wry>) -> tauri::Context<tauri::Wry> {
    if std::env::var("GENTS_NATIVE_E2E").ok().as_deref() == Some("1")
        && std::env::var("GENTS_E2E_LOCAL_MODEL").is_ok()
    {
        let identifier = crate::browser_data::isolated_store_identifier()
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
fn browser_store_lock<R>() -> tauri::plugin::TauriPlugin<R>
where
    R: tauri::Runtime,
{
    tauri::plugin::Builder::new("gents-browser-store")
        .setup(|app, _api| {
            app.manage(crate::browser_data::lock_store()?);
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

fn platform_bridge_config() -> BridgeConfig {
    let config = BridgeConfig {
        home: HomePolicy::Default,
        bootstrap: platform_bootstrap_policy(),
        app_meta: AppMeta {
            app_name: "Gents".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
        snapshot_grants: SnapshotGrants::all(),
        managed_server: platform_managed_server_policy(),
        client_host: cfg!(all(desktop, any(unix, windows))),
        store_key_custody: gents::store_key::StoreKeyCustodyChoice::default(),
    };
    #[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
    let config = native_local_e2e_config(config);
    config
}

#[cfg(all(feature = "native-e2e", debug_assertions, desktop))]
fn native_local_e2e_config(config: BridgeConfig) -> BridgeConfig {
    if std::env::var("GENTS_NATIVE_E2E").ok().as_deref() != Some("1")
        || std::env::var("GENTS_E2E_LOCAL_MODEL").is_err()
    {
        return config;
    }
    let path = |name| {
        let value = std::env::var_os(name)
            .unwrap_or_else(|| panic!("{name} is required for local native E2E"));
        let path = std::path::PathBuf::from(value);
        assert!(path.is_absolute(), "{name} must be absolute");
        path
    };
    BridgeConfig {
        home: HomePolicy::FixedRoot(path("GENTS_E2E_DESKTOP_HOME")),
        bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
            node_home: NodeHomePolicy::Fixed(path("GENTS_E2E_NODE_HOME")),
        },
        store_key_custody: if std::env::var("GENTS_E2E_FILE_STORE_KEYS").ok().as_deref()
            == Some("1")
        {
            gents::store_key::StoreKeyCustodyChoice::File
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
