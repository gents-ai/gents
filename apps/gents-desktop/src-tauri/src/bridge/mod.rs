//! Gents Desktop host shell: owns Builder, bundle identity, and capability grants.
//! Bridge logic lives in `gents_desktop_bridge`.

use gents_desktop_bridge::logging::init_tracing;
use gents_desktop_bridge::SnapshotGrants;
use gents_desktop_bridge::{
    init, install_runtime, AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy,
};

pub fn run() {
    init_tracing();
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
            // Matches capabilities/default.json granting gents-desktop-bridge:full.
            snapshot_grants: SnapshotGrants::all(),
        }))
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
