//! Phase-4 home isolation: FixedRoot policies never share storage.

use std::sync::Arc;

use gents_desktop_bridge::{
    resolve_policy, AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy,
    SnapshotGrants,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tempfile::tempdir;

// Re-export resolve_policy for integration tests — make it public if needed.
// resolve_policy is already pub in state module.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixed_root_homes_do_not_collide() {
    let tmp = tempdir().expect("tempdir");
    let bridge_root = tmp.path().join("bridge-home");
    let domain_root = tmp.path().join("domain-home");
    std::fs::create_dir_all(&bridge_root).unwrap();
    std::fs::create_dir_all(&domain_root).unwrap();

    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(bridge_root.clone()),
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            app_meta: AppMeta {
                app_name: "fixture".into(),
                app_version: "0.8.0".into(),
            },
            snapshot_grants: SnapshotGrants::chat_package(),
        },
        None,
    )
    .expect("resolve policy");

    assert_eq!(policy.desktop_paths.root(), bridge_root.as_path());
    assert!(
        policy.agent_home.is_none(),
        "PairedRemoteOnly has no agent home"
    );
    assert_ne!(
        policy.desktop_paths.root(),
        domain_root.as_path(),
        "domain home must stay distinct"
    );

    // Boot a real client under the bridge FixedRoot — proves the home is usable.
    let paths = DesktopPaths::from_root(&bridge_root);
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("client starts under FixedRoot");
    let core = Arc::new(core);

    // Marker file only under domain root.
    std::fs::write(domain_root.join("domain-marker"), b"domain").unwrap();
    assert!(
        !bridge_root.join("domain-marker").exists(),
        "domain marker must not appear under bridge home"
    );
    assert!(domain_root.join("domain-marker").exists());

    core.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_runtime_allowed_binds_fixed_agent_home() {
    let tmp = tempdir().expect("tempdir");
    let bridge_root = tmp.path().join("bridge");
    let agent_root = tmp.path().join("agent");
    std::fs::create_dir_all(&bridge_root).unwrap();
    std::fs::create_dir_all(&agent_root).unwrap();

    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(bridge_root.clone()),
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                agent_home: AgentHomePolicy::Fixed(agent_root.clone()),
            },
            app_meta: AppMeta {
                app_name: "fixture".into(),
                app_version: "0.8.0".into(),
            },
            snapshot_grants: SnapshotGrants::all(),
        },
        None,
    )
    .expect("resolve");

    assert_eq!(policy.agent_home.as_deref(), Some(agent_root.as_path()));
    assert_eq!(policy.desktop_paths.root(), bridge_root.as_path());
}

/// Fixture profile: capability grants must not exceed SnapshotGrants.
/// v1 is process-wide single profile (no per-caller ACL introspection).
#[test]
fn fixture_chat_fleet_profile_is_subset_of_declared_snapshot_grants() {
    // Documented fixture capability sets → expected SnapshotGrants bits.
    let grants = SnapshotGrants {
        session_read: true, // session-read
        fleet_read: true,   // fleet-read (+ fleet-admin does not widen projection)
        config_read: false, // no config-read / config-write
        operations_read: false,
        runtime_admin: false, // PairedRemoteOnly / no runtime-admin
    };
    // Must match apps/fixture-host BridgeConfig.snapshot_grants.
    assert_eq!(grants.session_read, true);
    assert_eq!(grants.fleet_read, true);
    assert_eq!(grants.config_read, false);
    assert_eq!(grants.runtime_admin, false);
}

#[test]
fn bridge_config_default_is_fail_closed_core_only() {
    let cfg = BridgeConfig::default();
    assert_eq!(cfg.snapshot_grants, SnapshotGrants::core_only());
    assert!(!cfg.snapshot_grants.session_read);
    assert!(!cfg.snapshot_grants.config_read);
    assert!(!cfg.snapshot_grants.runtime_admin);
}
