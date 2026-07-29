//! Lens-first schema migration for Gents.
//!
//! The DefraDB version DAG is the only source of truth. This crate adds a
//! declarative step registry, an engine that derives crash position from
//! observable database state, and a thin materialization driver.
//!
//! # Entry point
//!
//! ```ignore
//! gents_migration::ensure_migrations(&node).await?;
//! ```
//!
//! That single call registers the frozen baseline SDL, replays pending steps,
//! verifies every managed collection's lineage, and (when upstream supports
//! it) eagerly materializes documents. There is no public "register schemas
//! only" path — any bypass forks the version lineage.
//!
//! See `docs/superpowers/specs/2026-07-28-lens-first-migration-design.md`.

mod engine;
mod error;
mod expectation;
mod lens;
mod materialize;
mod registry;
mod report;
mod upgrade;

pub use engine::{
    ensure_migrations, ensure_migrations_arc, ensure_migrations_dynamic,
    ensure_migrations_with_registry,
};
pub use error::{Error, Result};
pub use expectation::{descriptor_digest, normalize_descriptor, CollectionExpectation};
pub use lens::{lens_config, predict_transform_id};
pub use registry::{
    fixture_lens_wasm, BaselineCollection, BaselineCollectionOwned, DynamicRegistry, LensSpec,
    LensSpecOwned, MigrationStep, MigrationStepOwned, Registry, DEFAULT_BASELINE, DEFAULT_REGISTRY,
    DEFAULT_STEPS,
};
pub use report::{MaterializationStats, MigrationReport};
pub use upgrade::{is_unknown_version_read_error, ROLLING_UPGRADE_GUIDANCE};
