use std::path::PathBuf;

use crate::snapshot::projection::SnapshotGrants;

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub home: HomePolicy,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
    pub managed_server: ManagedServerPolicy,
    /// Custody of new store keys (the client store and a provisioned managed
    /// home). Tests request file keys so no login-keychain item is written.
    pub store_key_custody: gents::store_key::StoreKeyCustodyChoice,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            home: HomePolicy::Default,
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                node_home: NodeHomePolicy::Default,
            },
            app_meta: AppMeta {
                app_name: "gents-desktop".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            snapshot_grants: SnapshotGrants::core_only(),
            managed_server: ManagedServerPolicy::Disabled,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedServerPolicy {
    Disabled,
    Allowed,
}

#[derive(Debug, Clone)]
pub enum HomePolicy {
    Default,
    AppDataDir { subdirectory: &'static str },
    FixedRoot(PathBuf),
}

#[derive(Debug, Clone)]
pub enum BootstrapPolicy {
    LocalRuntimeAllowed { node_home: NodeHomePolicy },
    PairedRemoteOnly,
}

#[derive(Debug, Clone)]
pub enum NodeHomePolicy {
    Default,
    Fixed(PathBuf),
}

#[derive(Debug, Clone)]
pub struct AppMeta {
    pub app_name: String,
    pub app_version: String,
}

#[derive(Debug, Clone)]
pub struct TracingConfig {
    pub filter: Option<String>,
    pub console: bool,
}
