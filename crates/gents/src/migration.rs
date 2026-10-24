use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

pub use gents_migration::{
    ensure_migrations, ensure_migrations_arc, ensure_migrations_dynamic,
    ensure_migrations_with_registry, fixture_lens_wasm, predict_transform_id, BaselineCollection,
    BaselineCollectionOwned, CollectionExpectation, DynamicRegistry, Error as MigrationError,
    LensSpec, LensSpecOwned, MaterializationStats, MigrationReport, MigrationStep,
    MigrationStepOwned, Registry, DEFAULT_BASELINE, DEFAULT_REGISTRY, DEFAULT_STEPS,
};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeMigrationError {
    #[error("ensure_migrations")]
    EnsureMigrations(#[source] MigrationError),
}

pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    crate::config_client::ConfigAccess::local(node)
        .run_local_owned(|node| async move {
            ensure_migrations_borrowed(node.as_ref())
                .await
                .map_err(anyhow::Error::from)
        })
        .await?;
    Ok(())
}

pub(crate) async fn ensure_migrations_borrowed(
    node: &EmbeddedNode,
) -> Result<(), RuntimeMigrationError> {
    ensure_migrations(node)
        .await
        .map(|_| ())
        .map_err(RuntimeMigrationError::EnsureMigrations)
}
