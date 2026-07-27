//! Gents Desktop host shell: owns Builder, bundle identity, and capability grants.
//! Bridge logic lives in `gents_desktop_bridge`.

use gents_desktop_bridge::{
    install_runtime, init, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, AgentHomePolicy,
};
use gents_desktop_bridge::logging::init_tracing;

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
        }))
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
