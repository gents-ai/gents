//! Schema migration entry points — thin re-export of `gents-migration`.
//!
//! The lens-first engine owns baseline registration, chain replay, lineage
//! verification, and (when upstream supports it) materialization. See
//! `docs/superpowers/specs/2026-07-28-lens-first-migration-design.md`.

use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

pub use gents_migration::{
    ensure_migrations, ensure_migrations_arc, ensure_migrations_dynamic,
    ensure_migrations_with_registry, fixture_lens_wasm, is_unknown_version_read_error,
    predict_transform_id, BaselineCollection, BaselineCollectionOwned, CollectionExpectation,
    DynamicRegistry, Error as MigrationError, LensSpec, LensSpecOwned, MaterializationStats,
    MigrationReport, MigrationStep, MigrationStepOwned, Registry, DEFAULT_BASELINE,
    DEFAULT_REGISTRY, DEFAULT_STEPS, ROLLING_UPGRADE_GUIDANCE,
};

/// Production bootstrap entry used by CLI, desktop, and runtime startup.
///
/// Registers the frozen baseline SDL, applies the (currently empty) step
/// chain, and rejects multi-version pre-cutover lineages with a clear error.
pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_migrations(node.as_ref())
        .await
        .context("ensure_migrations")?;
    Ok(())
}

/// Historical name retained for call sites that only needed behavior-related
/// schema presence. Routes through the full engine — partial registration
/// forks the version lineage.
pub async fn ensure_agent_behavior_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_all_runtime_migrations(node).await
}
