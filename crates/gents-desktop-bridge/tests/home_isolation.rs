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
                app_version: env!("CARGO_PKG_VERSION").into(),
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

    // Keep two real ClientCore/DefraDB stores live concurrently under distinct
    // roots. This catches process-global or path-resolution collisions that a
    // sequential marker-file assertion cannot.
    let bridge_core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&bridge_root),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("bridge client starts under FixedRoot"),
    );
    let domain_core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&domain_root),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("second store starts concurrently under domain root"),
    );

    // Marker file only under domain root.
    std::fs::write(domain_root.join("domain-marker"), b"domain").unwrap();
    assert!(
        !bridge_root.join("domain-marker").exists(),
        "domain marker must not appear under bridge home"
    );
    assert!(domain_root.join("domain-marker").exists());

    domain_core.shutdown().await.expect("domain store shutdown");
    bridge_core.shutdown().await.expect("bridge store shutdown");
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
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            snapshot_grants: SnapshotGrants::all(),
        },
        None,
    )
    .expect("resolve");

    assert_eq!(policy.agent_home.as_deref(), Some(agent_root.as_path()));
    assert_eq!(policy.desktop_paths.root(), bridge_root.as_path());
}

// The capability-vs-snapshot-grants consistency fence lives in the fixture
// crate (gents-fixture-host `capability_grants_match_declared_snapshot_grants`),
// where it parses the real capabilities/default.json against the real
// BridgeConfig instead of restating expected values.

#[test]
fn bridge_config_default_is_fail_closed_core_only() {
    let cfg = BridgeConfig::default();
    assert_eq!(cfg.snapshot_grants, SnapshotGrants::core_only());
    assert!(!cfg.snapshot_grants.session_read);
    assert!(!cfg.snapshot_grants.config_read);
    assert!(!cfg.snapshot_grants.runtime_admin);
}
