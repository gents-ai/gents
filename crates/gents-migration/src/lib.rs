mod engine;
mod error;
mod expectation;
mod lens;
mod materialize;
mod registry;
mod report;
mod upgrades;

pub use engine::{
    canonical_schema_plan, canonical_versions, check_migration_compatibility, ensure_migrations,
    ensure_migrations_arc, ensure_migrations_dynamic, ensure_migrations_with_registry,
    non_canonical_lineages, non_canonical_store_lineages, CanonicalSchemaPlan, LineageMismatch,
};
pub use error::{Error, Result};
pub use expectation::CollectionExpectation;
pub use lens::{lens_config, predict_transform_id};
pub use registry::{
    fixture_lens_wasm, BaselineCollection, BaselineCollectionOwned, DynamicRegistry, LensSpec,
    LensSpecOwned, MigrationStep, MigrationStepOwned, OrdinaryIndex, OrdinaryIndexField, Registry,
    CLIENT_AUTHORED_COLLECTIONS, DEFAULT_BASELINE, DEFAULT_ORDINARY_INDEXES, DEFAULT_REGISTRY,
    DEFAULT_STEPS,
};
pub use report::{MaterializationStats, MigrationReport};
