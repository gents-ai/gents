use std::path::PathBuf;

use fixture_domain_plugin::DomainConfig;
use gents_desktop_bridge::{
    init as init_bridge, init_tracing as install_tracing, install_runtime, AppMeta,
    BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, SnapshotGrants, TracingConfig,
};

fn bridge_config() -> BridgeConfig {
    BridgeConfig {
        home: HomePolicy::AppDataDir {
            subdirectory: "gents-fixture-host/client",
        },
        bootstrap: BootstrapPolicy::PairedRemoteOnly,
        app_meta: AppMeta {
            app_name: "Gents Fixture Host".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
        snapshot_grants: SnapshotGrants {
            session_read: true,
            fleet_read: true,
            config_read: false,
            operations_read: true,
            runtime_admin: false,
        },
        managed_server: ManagedServerPolicy::Disabled,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host_root = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("gents-fixture-host");
    let log_home = host_root.join("logs");
    let domain_home = std::env::var_os("FIXTURE_DOMAIN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host_root.join("domain"));
    let _ = std::fs::create_dir_all(&log_home);
    let _ = std::fs::create_dir_all(&domain_home);

    install_tracing(TracingConfig {
        log_path: log_home.join("fixture-host.log"),
        filter: None,
        console: true,
    });
    install_runtime();

    tauri::Builder::default()
        .plugin(init_bridge(bridge_config()))
        .plugin(fixture_domain_plugin::init(DomainConfig {
            home: domain_home,
        }))
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running fixture host");
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPABILITIES: &str = include_str!("../capabilities/default.json");

    fn granted_projection_bits() -> SnapshotGrants {
        let caps: serde_json::Value =
            serde_json::from_str(CAPABILITIES).expect("capabilities/default.json parses");
        let perms = caps["permissions"]
            .as_array()
            .expect("capabilities permissions array");
        let mut bits = SnapshotGrants::core_only();
        for perm in perms {
            let Some(name) = perm.as_str() else { continue };
            let set = name.strip_prefix("gents-desktop-bridge:").unwrap_or(name);
            match set {
                "session-read" => bits.session_read = true,
                "fleet-read" => bits.fleet_read = true,
                "config-read" => bits.config_read = true,
                "operations-read" => bits.operations_read = true,
                "runtime-admin" => bits.runtime_admin = true,
                "full" => bits = SnapshotGrants::all(),
                _ => {}
            }
        }
        bits
    }

    #[test]
    fn capability_grants_match_declared_snapshot_grants() {
        let declared = bridge_config().snapshot_grants;
        let granted = granted_projection_bits();
        assert_eq!(
            granted, declared,
            "capabilities/default.json and BridgeConfig.snapshot_grants drifted; \
             update both together (v1 single-profile contract)"
        );
    }
}
