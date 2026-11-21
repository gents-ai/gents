use std::path::PathBuf;

use gents_store::client_journal::paths::DesktopPaths;

use crate::snapshot::projection::SnapshotGrants;

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub home: HomePolicy,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
    pub managed_server: ManagedServerPolicy,
    /// Allows local frontends to use the chat client through private IPC.
    /// This is independent of the embedding webview's projection grants.
    pub client_host: bool,
    pub store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice,
    /// The `gents` executable the desktop installs as its service, named at
    /// launch; `None` discovers it beside the app, in its resources, then on
    /// PATH.
    pub service_executable: Option<PathBuf>,
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
            client_host: false,
            store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::default(),
            service_executable: None,
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

#[derive(Debug, Clone)]
pub struct ResolvedBridgePolicy {
    pub desktop_paths: DesktopPaths,
    pub node_home: Option<PathBuf>,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
    pub managed_server: ManagedServerPolicy,
    pub client_host: bool,
    pub store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice,
    pub service_executable: Option<PathBuf>,
}
