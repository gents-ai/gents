//! Fixture host shell: owns Builder, bundle identity, and co-resident plugins.

use std::path::PathBuf;

use fixture_domain_plugin::DomainConfig;
use gents_desktop_bridge::{
    install_runtime, init as init_bridge, AppMeta, BootstrapPolicy, BridgeConfig,
    HomePolicy, SnapshotGrants,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    gents_desktop_bridge::logging::init_tracing();
    install_runtime();

    // Distinct AppDataDir subdirectory so this host never collides with Gents Desktop.
    let domain_home = std::env::var_os("FIXTURE_DOMAIN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("gents-fixture-host")
                .join("domain")
        });

    tauri::Builder::default()
        .plugin(init_bridge(BridgeConfig {
            home: HomePolicy::AppDataDir {
                subdirectory: "gents-fixture-host/client",
            },
            // Paired-remote only: no local runtime provisioning from the webview.
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            app_meta: AppMeta {
                app_name: "Gents Fixture Host".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            // Chat + fleet grants only (matches capabilities/default.json).
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
