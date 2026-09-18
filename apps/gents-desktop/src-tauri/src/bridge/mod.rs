#[cfg(desktop)]
mod window_close;
#[cfg(target_os = "macos")]
mod windows;

#[cfg(desktop)]
use gents_desktop_bridge::contract::{
    MANAGED_SERVER_TRAY_RESTART_EVENT, MANAGED_SERVER_TRAY_START_EVENT,
    MANAGED_SERVER_TRAY_STOP_EVENT, MANAGED_SERVER_UPDATED_EVENT,
};
use gents_desktop_bridge::{
    init, init_tracing as install_tracing, install_runtime, AgentHomePolicy, AppMeta,
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
#[cfg(desktop)]
use tauri::{Emitter, Listener, Manager};

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
    install_runtime();

    let builder = tauri::Builder::default()
        .plugin(init(platform_bridge_config()))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());
    #[cfg(target_os = "macos")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        windows::desktop_window_setup_complete
    ]);
    #[cfg(desktop)]
    let builder = builder
        .setup(|app| {
            setup_tray(app)?;
            #[cfg(target_os = "macos")]
            windows::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
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
                        // The last visible view is closing and no runtime needs
                        // the hidden coordinator. Destroy it so last-window exit
                        // works exactly as it did before native tabs.
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
        .build(tauri::generate_context!())
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

fn platform_bridge_config() -> BridgeConfig {
    BridgeConfig {
        home: HomePolicy::Default,
        bootstrap: platform_bootstrap_policy(),
        app_meta: AppMeta {
            app_name: "Gents".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
        snapshot_grants: SnapshotGrants::all(),
        managed_server: platform_managed_server_policy(),
    }
}

#[cfg(all(desktop, any(target_os = "macos", target_os = "linux")))]
fn platform_bootstrap_policy() -> BootstrapPolicy {
    BootstrapPolicy::LocalRuntimeAllowed {
        agent_home: AgentHomePolicy::Default,
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

    #[test]
    fn tray_does_not_claim_process_readiness_while_starting() {
        assert_eq!(
            managed_agent_tooltip(Some("starting")),
            "Gents agent service — starting"
        );
        assert_eq!(
            managed_agent_tooltip(Some("failed")),
            "Gents agent service — status unavailable"
        );
    }
}

#[cfg(desktop)]
struct TrayRuntimeState {
    // The desktop is tray-resident independently of agent process state. This
    // stays true so closing the main window preserves the menu-bar frontend.
    active: Arc<AtomicBool>,
    // Native non-selected tabs can also report invisible. Only a deliberate
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
    let start = MenuItem::with_id(app, "start", "Start Agent", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop Agent", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Restart Agent", true, None::<&str>)?;
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
        .tooltip("Gents agent service — independent of the desktop")
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
        Some("running") => "Gents agent service — running",
        Some("starting") => "Gents agent service — starting",
        Some("stopped") | Some("disabled") => "Gents agent service — stopped",
        Some("external") => "Gents agent — running outside the managed service",
        Some("failed") | None => "Gents agent service — status unavailable",
        Some(_) => "Gents agent service — status unavailable",
    }
}

#[cfg(desktop)]
fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        if window.show().is_ok() {
            if let Some(state) = app.try_state::<TrayRuntimeState>() {
                state.main_hidden_by_close.store(false, Ordering::SeqCst);
            }
        }
        let _ = window.set_focus();
    }
}
