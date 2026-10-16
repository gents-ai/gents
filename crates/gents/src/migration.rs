//! Schema migration entry points — thin re-export of `gents-migration`.

use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

pub use gents_migration::{
    ensure_migrations, ensure_migrations_arc, ensure_migrations_dynamic,
    ensure_migrations_with_registry, fixture_lens_wasm, predict_transform_id, BaselineCollection,
    BaselineCollectionOwned, CollectionExpectation, DynamicRegistry, Error as MigrationError,
    LensSpec, LensSpecOwned, MaterializationStats, MigrationReport, MigrationStep,
    MigrationStepOwned, Registry, DEFAULT_BASELINE, DEFAULT_REGISTRY, DEFAULT_STEPS,
};

/// Production bootstrap entry used by CLI, desktop, and runtime startup.
pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    crate::config_client::ConfigAccess::local(node)
        .run_local_owned(|node| async move { ensure_migrations_borrowed(node.as_ref()).await })
        .await?;
    Ok(())
}

pub(crate) async fn ensure_migrations_borrowed(node: &EmbeddedNode) -> Result<()> {
    ensure_migrations(node)
        .await
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!(error))
        .context("ensure_migrations")
}
