pub mod backend_protection;
#[cfg(test)]
mod backend_protection_tests;
pub mod file_lock;
pub mod home;
#[cfg(test)]
mod node_release_tests;
pub(crate) mod plugin_delete_source;
mod runtime_document_projection;
pub mod storage_backend;
pub mod store_key;
mod tool_permission_projection;

/// Build the single native document projector for a held local peer identity.
/// The caller supplies the host's tool ceiling; transport publication remains
/// scoped to the native Node administrator of this store.
pub fn document_projector(
    node: std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
    ceiling: crate::tools::surface::ToolCeiling,
) -> std::result::Result<std::sync::Arc<dyn db::DocumentMutationProjector>, identity::Error> {
    Ok(std::sync::Arc::new(
        runtime_document_projection::RuntimeDocumentProjector::new(node, ceiling)?,
    ))
}

pub(crate) fn document_projector_with_installed_plugins(
    node: std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
    ceiling: crate::tools::surface::ToolCeiling,
    plugins: Vec<crate::plugin::store::InstalledPlugin>,
) -> std::result::Result<std::sync::Arc<dyn db::DocumentMutationProjector>, identity::Error> {
    Ok(std::sync::Arc::new(
        runtime_document_projection::RuntimeDocumentProjector::new(node, ceiling)?
            .with_startup_plugins(plugins),
    ))
}
