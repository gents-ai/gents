use super::*;
use super::{
    authority::*, launch::*, pairing::*, port::*, preference::*, readiness::*, reset::*,
    start_wait::*, status::*,
};
use crate::client_runtime::ClientRuntime;
use crate::state::ManagedServerState as ManagedServerRuntimeState;
use crate::types::{ManagedServerState, ManagedServerToolCeiling};
use gents_desktop_core::home_reset::*;
use gents_desktop_core::preference::MANAGED_SERVER_CONFIG;
use gents_service::repair::crash_loop::{CrashLoopWatch, JobSample};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod authority;
mod crash_loop;
mod node_port;
mod orchestration;
mod pairing;
mod readiness;
mod repair;
mod reset;
mod reset_store;
mod startup_readiness;
mod status;
mod unfinished_setup;

fn not_the_user_home() -> &'static Path {
    Path::new("/nonexistent-user-home")
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn lineage_refusal(
    data_path: PathBuf,
) -> gents_protocol::operations::install_repair::IncompatibleStore {
    gents_protocol::operations::install_repair::IncompatibleStore {
        kind: gents_protocol::operations::install_repair::IncompatibleStoreKind::UnknownLineage,
        data_path,
    }
}

fn dirty_home(temp: &Path) -> (PathBuf, gents_store::client_journal::paths::DesktopPaths) {
    let home = temp.join(".gents");
    write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
    write(
        &home.join("init.json"),
        r#"{"node_did":"did:key:old","node_name":"local"}"#,
    );
    write(&home.join("keys/local.key"), "identity");
    write(&home.join("runtime.json"), "{}");
    write(&home.join("p2p-secret-key"), "p2p");
    write(
        &home.join("grok-port-home/init.json"),
        r#"{"node_did":"did:key:other"}"#,
    );
    write(&home.join("grok-port-home/data/MANIFEST"), "REGOMAN other");
    let desktop = gents_store::client_journal::paths::DesktopPaths::from_root(temp.join("desktop"));
    write(
        &desktop.node_data_dir().join("MANIFEST"),
        "REGOMAN old client",
    );
    write(desktop.runtime_directory_path(), "{}");
    write(
        desktop.principal_metadata_path(),
        r#"{"did":"did:key:z6MkClient","public_key_bytes":[1,2,3]}"#,
    );
    write(desktop.identity_key_path(), "principal");
    write(&desktop.root().join(MANAGED_SERVER_CONFIG), "{}");
    write(&desktop.root().join("runtime/gents"), "packaged runtime");
    (std::fs::canonicalize(&home).unwrap(), desktop)
}

fn ready_status(node_did: &str) -> ManagedServerStatus {
    ManagedServerStatus {
        state: ManagedServerState::External,
        auto_start: false,
        node_name: Some("local".to_string()),
        node_did: Some(node_did.to_string()),
        graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
        effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
        effective_tool_root: None,
        suggested_tool_root: None,
        pairing_ready: false,
        approval_required: false,
        runtime_booting: false,
        error: None,
        error_code: None,
        deleted_home: None,
    }
}

fn service_exit(restarts: u64) -> gents_service::native_service::ServiceExit {
    gents_service::native_service::ServiceExit {
        reason: "exited with code 78".to_string(),
        restarts,
        clean: false,
        code: Some(78),
    }
}

fn orchestration_state() -> (tempfile::TempDir, DesktopAppState) {
    use crate::config::{
        AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
    };
    use crate::snapshot::projection::SnapshotGrants;
    use crate::state::resolve_policy;

    let temp = tempfile::tempdir().expect("temporary bridge roots");
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                node_home: NodeHomePolicy::Fixed(temp.path().join("agent")),
            },
            app_meta: AppMeta {
                app_name: "managed-start-test".to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            snapshot_grants: SnapshotGrants::core_only(),
            managed_server: ManagedServerPolicy::Allowed,
            store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
            service_executable: None,
            registry: None,
            client_host: false,
        },
        None,
    )
    .expect("fixed bridge policy");
    (temp, DesktopAppState::new(policy))
}

use crate::test_support::FakeLaunch;

/// A restart that fails after the starter packs were enabled still says
/// what happened to them.
#[test]
fn a_failed_restart_reports_the_packs_enabled_before_it() {
    let report = serde_json::json!({
        "enabled": [{"pack": "gents/web"}, {"pack": "gents/charts"}],
        "failed": [
            {"pack": "gents/script", "error": "simulated", "installed": true},
            {"pack": "gents/data_tables", "error": "simulated", "installed": false},
            {"pack": "gents/pack_builder", "error": "simulated"},
        ],
    });
    assert_eq!(
        installed_before_restart(&report),
        "Before the local node failed to start again, these starter packs were turned on: gents/web, gents/charts. These are installed but not finished: gents/script. Turn them on again once the node starts. These were not turned on: gents/data_tables, gents/pack_builder."
    );
    // A pack that is on the node is never reported under "no starter pack
    // was turned on".
    assert_eq!(
        installed_before_restart(&serde_json::json!({
            "enabled": [],
            "failed": [{"pack": "gents/script", "error": "simulated", "installed": true}],
        })),
        "Before the local node failed to start again, these are installed but not finished: gents/script. Turn them on again once the node starts."
    );
    assert_eq!(
        installed_before_restart(&serde_json::json!({
            "enabled": [],
            "failed": [{"pack": "gents/web", "error": "simulated", "installed": false}],
        })),
        "Before the local node failed to start again, no starter pack was turned on. These were not turned on: gents/web."
    );
    assert_eq!(
        installed_before_restart(&serde_json::json!({"enabled": [], "failed": []})),
        "Before the local node failed to start again, no starter pack was turned on."
    );
}
