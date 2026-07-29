//! Typed errors for the migration engine.

use thiserror::Error;

/// Errors produced by [`crate::ensure_migrations`].
#[derive(Debug, Error)]
pub enum Error {
    /// Schema registration (`add_schema`) failed for a reason other than
    /// "already exists".
    #[error("baseline schema registration failed for {collection}: {source}")]
    BaselineRegister {
        collection: String,
        #[source]
        source: anyhow::Error,
    },

    /// A managed collection is absent after baseline registration should have
    /// created it.
    #[error("managed collection {collection} missing after baseline registration")]
    CollectionMissing { collection: String },

    /// No version in the collection's DAG matches a known pin (database
    /// predates the migration baseline or was produced by foreign patches).
    #[error(
        "unknown lineage for collection {collection}: versions [{versions}] do not match the migration baseline; export/import required (no legacy support)"
    )]
    UnknownLineage {
        collection: String,
        versions: String,
    },

    /// The lineage is recognized but the DAG contains a version or edge that
    /// is neither a pin nor an expected placeholder. Even inactive foreign
    /// versions change head/priority reconstruction and subsequent CIDs.
    #[error(
        "foreign version in collection {collection}: version_id={version_id} is not in the known pin set"
    )]
    ForeignVersion {
        collection: String,
        version_id: String,
    },

    /// Active version does not match the pinned baseline/step expectation.
    #[error(
        "version pin mismatch for collection {collection}: expected {expected}, active is {actual}"
    )]
    VersionPinMismatch {
        collection: String,
        expected: String,
        actual: String,
    },

    /// Active descriptor failed the collection expectation predicate.
    #[error("state verification failed for collection {collection} at step {step}: {detail}")]
    StateVerification {
        collection: String,
        step: String,
        detail: String,
    },

    /// A step could not be applied.
    #[error("migration step {step} failed on collection {collection}: {source}")]
    StepFailed {
        step: String,
        collection: String,
        #[source]
        source: anyhow::Error,
    },

    /// Node introspection failed (get_collection / list versions).
    #[error("defra node error: {0}")]
    Node(#[from] anyhow::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
