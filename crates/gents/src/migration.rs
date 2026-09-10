//! Schema migration entry points — thin re-export of `gents-migration`.
//!
//! The lens-first engine owns baseline registration, chain replay, lineage
//! verification, and (when upstream supports it) materialization.

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
///
/// Registers the canonical baseline SDL, applies configured migration steps,
/// and verifies collection lineages. The current registry has no historical steps.
pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_migrations(node.as_ref())
        .await
        .context("ensure_migrations")?;
    Ok(())
}
