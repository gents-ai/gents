mod engine;
mod error;
mod expectation;
mod lens;
mod materialize;
mod registry;
mod report;

pub use engine::{
    ensure_migrations, ensure_migrations_arc, ensure_migrations_dynamic,
    ensure_migrations_with_registry,
};
pub use error::{Error, Result};
pub use expectation::CollectionExpectation;
pub use lens::{lens_config, predict_transform_id};
pub use registry::{
    fixture_lens_wasm, BaselineCollection, BaselineCollectionOwned, DynamicRegistry, LensSpec,
    LensSpecOwned, MigrationStep, MigrationStepOwned, OrdinaryIndex, OrdinaryIndexField, Registry,
    CLIENT_AUTHORED_COLLECTIONS, DEFAULT_BASELINE, DEFAULT_ORDINARY_INDEXES, DEFAULT_REGISTRY,
    DEFAULT_STEPS, INFERENCE_PROFILE_ADD_SERVICE_TIER_PATCH, INFERENCE_PROFILE_RC0_SDL,
};
pub use report::{MaterializationStats, MigrationReport};
