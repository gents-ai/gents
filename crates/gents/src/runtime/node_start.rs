use std::path::Path;
use std::sync::Arc;

use crate::plugin::store::{
    list_records, projected_plugins, try_lock_store_shared, PublishedPluginReadError, StoreGuard,
};
use crate::store::identity::NodeIdentity;
use crate::store::local::backend_protection::{
    BackendAdmission, BackendProtectionError, PreflightProtectedBackendStore,
};
use crate::store::local::store_key::StoreKey;
use crate::tools::surface::ToolCeiling;

type Result<T> = std::result::Result<T, BackendProtectionError>;

/// Starts the served node with the host's tool ceiling. The installed plugins
/// it projects are read under the plugin store's shared lock, which is held
/// until the node has started.
pub async fn start_node(
    preflight: PreflightProtectedBackendStore<'_>,
    key: &StoreKey,
    runtime: Arc<dyn NodeIdentity>,
    http: Option<defra_node::HttpConfig>,
    p2p: Option<defra_node::P2PConfig>,
    ceiling: ToolCeiling,
    plugin_home: &Path,
) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
    let plugin_home = plugin_home.to_owned();
    let (plugins, _plugin_guard) = tokio::task::spawn_blocking(move || {
        let guard = try_lock_store_shared(&plugin_home)?;
        let plugins = list_records(&plugin_home).map_err(PublishedPluginReadError::Read)?;
        Ok::<_, PublishedPluginReadError>((plugins, guard))
    })
    .await?
    .map_err(|error| BackendProtectionError::Schema(error.into()))?;
    preflight
        .start_node_with_projection(
            key,
            runtime,
            http,
            p2p,
            Arc::new(ceiling),
            projected_plugins(&plugins),
        )
        .await
}

/// Starts the node for a caller that already holds the home's exclusive
/// plugin store lock: the installed plugins it projects are read under that
/// guard, because a shared lock taken here would refuse against the caller's
/// own exclusive one.
pub async fn start_node_under_plugin_store(
    preflight: PreflightProtectedBackendStore<'_>,
    key: &StoreKey,
    runtime: Arc<dyn NodeIdentity>,
    http: Option<defra_node::HttpConfig>,
    p2p: Option<defra_node::P2PConfig>,
    ceiling: ToolCeiling,
    guard: &StoreGuard,
) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
    let plugin_home = guard.home().to_owned();
    let plugins = tokio::task::spawn_blocking(move || list_records(&plugin_home))
        .await?
        .map_err(|error| {
            BackendProtectionError::Schema(PublishedPluginReadError::Read(error).into())
        })?;
    preflight
        .start_node_with_projection(
            key,
            runtime,
            http,
            p2p,
            Arc::new(ceiling),
            projected_plugins(&plugins),
        )
        .await
}
