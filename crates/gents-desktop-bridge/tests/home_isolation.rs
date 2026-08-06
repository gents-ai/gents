use std::sync::Arc;

use gents_desktop_bridge::{
    resolve_policy, AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy,
    ManagedServerPolicy, SnapshotGrants,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tempfile::tempdir;

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
            managed_server: ManagedServerPolicy::Disabled,
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
            managed_server: ManagedServerPolicy::Allowed,
        },
        None,
    )
    .expect("resolve");

    assert_eq!(policy.agent_home.as_deref(), Some(agent_root.as_path()));
    assert_eq!(policy.desktop_paths.root(), bridge_root.as_path());
}

#[test]
fn bridge_config_default_is_fail_closed_core_only() {
    let cfg = BridgeConfig::default();
    assert_eq!(cfg.snapshot_grants, SnapshotGrants::core_only());
    assert!(!cfg.snapshot_grants.session_read);
    assert!(!cfg.snapshot_grants.config_read);
    assert!(!cfg.snapshot_grants.runtime_admin);
    assert_eq!(cfg.managed_server, ManagedServerPolicy::Disabled);
}
