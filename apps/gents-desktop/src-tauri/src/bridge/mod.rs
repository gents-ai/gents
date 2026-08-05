#[cfg(desktop)]
use gents_desktop_bridge::contract::{
    MANAGED_SERVER_TRAY_STOP_EVENT, MANAGED_SERVER_UPDATED_EVENT,
};
use gents_desktop_bridge::{
    init, init_tracing as install_tracing, install_runtime, AgentHomePolicy, AppMeta,
    BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, SnapshotGrants, TracingConfig,
};
use gents_desktop_core::client::DesktopPaths;
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
    let log_path = DesktopPaths::discover()
        .map(|paths| paths.log_file_path())
        .unwrap_or_else(|_| std::env::temp_dir().join("gents-desktop.log"));
    install_tracing(TracingConfig {
        log_path,
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
        .plugin(init(BridgeConfig {
            home: HomePolicy::Default,
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                agent_home: AgentHomePolicy::Default,
            },
            app_meta: AppMeta {
                app_name: "Gents".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            snapshot_grants: SnapshotGrants::all(),
            managed_server: ManagedServerPolicy::Allowed,
        }))
        .plugin(tauri_plugin_opener::init());
    #[cfg(desktop)]
    let builder = builder.setup(setup_tray).on_window_event(|window, event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            let active = window
                .app_handle()
                .try_state::<TrayRuntimeState>()
                .is_some_and(|state| state.active.load(Ordering::SeqCst));
            if active {
                api.prevent_close();
                let _ = window.hide();
            }
        }
    });
    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(desktop)]
struct TrayRuntimeState {
    active: Arc<AtomicBool>,
}

#[cfg(desktop)]
fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "Open Gents", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop Local Agent", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Gents", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &stop, &quit])?;
    let active = Arc::new(AtomicBool::new(false));
    app.manage(TrayRuntimeState {
        active: Arc::clone(&active),
    });
    let tray = TrayIconBuilder::with_id("gents-managed-server")
        .menu(&menu)
        .tooltip("Gents local agent")
        .icon(
            app.default_window_icon()
                .cloned()
                .ok_or("missing application icon")?,
        )
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "stop" => {
                let _ = app.emit(MANAGED_SERVER_TRAY_STOP_EVENT, ());
                show_main_window(app);
            }
            "quit" => shutdown_and_quit(app),
            _ => {}
        })
        .build(app)?;
    tray.set_visible(false)?;

    let tray_id = tray.id().clone();
    let app_handle = app.handle().clone();
    app.listen(MANAGED_SERVER_UPDATED_EVENT, move |event| {
        let running = serde_json::from_str::<serde_json::Value>(event.payload())
            .ok()
            .and_then(|value| {
                value
                    .get("state")
                    .and_then(|state| state.as_str())
                    .map(str::to_string)
            })
            .is_some_and(|state| state == "running" || state == "starting");
        active.store(running, Ordering::SeqCst);
        if let Some(tray) = app_handle.tray_by_id(&tray_id) {
            let _ = tray.set_visible(running);
        }
    });
    Ok(())
}

#[cfg(desktop)]
fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
fn shutdown_and_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app.try_state::<gents_desktop_bridge::state::DesktopAppState>() {
            let server = state.managed_server.lock().await.server.take();
            if let Some(server) = server {
                match tokio::time::timeout(std::time::Duration::from_secs(5), server.shutdown())
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "managed local server shutdown failed during quit");
                    }
                    Err(_) => {
                        tracing::warn!("managed local server shutdown timed out during quit");
                    }
                }
            }
        }
        app.exit(0);
    });
}
