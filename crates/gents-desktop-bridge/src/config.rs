use std::path::PathBuf;

use crate::snapshot::projection::SnapshotGrants;

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub home: HomePolicy,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
    pub managed_server: ManagedServerPolicy,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            home: HomePolicy::Default,
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                agent_home: AgentHomePolicy::Default,
            },
            app_meta: AppMeta {
                app_name: "gents-desktop".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            snapshot_grants: SnapshotGrants::core_only(),
            managed_server: ManagedServerPolicy::Disabled,
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
    LocalRuntimeAllowed { agent_home: AgentHomePolicy },
    PairedRemoteOnly,
}

#[derive(Debug, Clone)]
pub enum AgentHomePolicy {
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
    pub log_path: PathBuf,
    pub filter: Option<String>,
    pub console: bool,
}
