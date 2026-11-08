pub mod backend_protection;
#[cfg(test)]
mod backend_protection_tests;
pub mod file_lock;
pub mod home;
#[cfg(test)]
mod node_release_tests;
pub(crate) mod plugin_delete_source;
mod projection_policy;
mod runtime_document_projection;
pub mod storage_backend;
pub mod store_key;
mod tool_permission_projection;

pub use projection_policy::{
    ProjectedOperationSource, ProjectedPlugin, ProjectedPluginRef, ToolSourcePolicy,
};

/// Build the single native document projector for a held local peer identity.
/// The caller supplies the host's tool policy; transport publication remains
/// scoped to the native Node administrator of this store.
pub fn document_projector(
    node: std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
    policy: std::sync::Arc<dyn ToolSourcePolicy>,
) -> std::result::Result<std::sync::Arc<dyn db::DocumentMutationProjector>, identity::Error> {
    Ok(std::sync::Arc::new(
        runtime_document_projection::RuntimeDocumentProjector::new(node, policy)?,
    ))
}

pub(crate) fn document_projector_with_installed_plugins(
    node: std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
    policy: std::sync::Arc<dyn ToolSourcePolicy>,
    plugins: Vec<ProjectedPlugin>,
) -> std::result::Result<std::sync::Arc<dyn db::DocumentMutationProjector>, identity::Error> {
    Ok(std::sync::Arc::new(
        runtime_document_projection::RuntimeDocumentProjector::new(node, policy)?
            .with_startup_plugins(plugins),
    ))
}
