use gents_desktop_bridge::{
    init, init_tracing as install_tracing, install_runtime, AgentHomePolicy, AppMeta,
    BootstrapPolicy, BridgeConfig, HomePolicy, SnapshotGrants, TracingConfig,
};
use gents_desktop_core::client::DesktopPaths;

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

    tauri::Builder::default()
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
        }))
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
