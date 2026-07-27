//! Fixture host shell: owns Builder, bundle identity, and co-resident plugins.

use std::path::PathBuf;

use fixture_domain_plugin::DomainConfig;
use gents_desktop_bridge::{
    init as init_bridge, init_tracing as install_tracing, install_runtime, AppMeta,
    BootstrapPolicy, BridgeConfig, HomePolicy, SnapshotGrants, TracingConfig,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Resolve domain + client homes first so tracing never uses Gents defaults.
    let host_root = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("gents-fixture-host");
    let client_home = host_root.join("client");
    let domain_home = std::env::var_os("FIXTURE_DOMAIN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host_root.join("domain"));
    let _ = std::fs::create_dir_all(&client_home);
    let _ = std::fs::create_dir_all(&domain_home);

    install_tracing(TracingConfig {
        log_path: client_home.join("fixture-host.log"),
        filter: None,
        console: true,
    });
    install_runtime();

    tauri::Builder::default()
        .plugin(init_bridge(BridgeConfig {
            // FixedRoot under host_root for deterministic isolation tests; production
            // shape is AppDataDir — both keep storage under gents-fixture-host/.
            home: HomePolicy::AppDataDir {
                subdirectory: "gents-fixture-host/client",
            },
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            app_meta: AppMeta {
                app_name: "Gents Fixture Host".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            // Process-wide profile matching capabilities/default.json (chat+fleet).
            snapshot_grants: SnapshotGrants {
                session_read: true,
                fleet_read: true,
                config_read: false,
                operations_read: false,
                runtime_admin: false,
            },
        }))
        .plugin(fixture_domain_plugin::init(DomainConfig {
            home: domain_home,
        }))
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running fixture host");
}
