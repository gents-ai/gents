use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("baseline schema registration failed for {collection}: {source}")]
    BaselineRegister {
        collection: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("managed collection {collection} missing after baseline registration")]
    CollectionMissing { collection: String },

    #[error(
        "unknown lineage for collection {collection}: versions [{versions}] do not match the migration baseline; export/import required (no legacy support)"
    )]
    UnknownLineage {
        collection: String,
        versions: String,
    },

    #[error(
        "foreign version in collection {collection}: version_id={version_id} is not in the known pin set"
    )]
    ForeignVersion {
        collection: String,
        version_id: String,
    },

    #[error(
        "version pin mismatch for collection {collection}: expected {expected}, active is {actual}"
    )]
    VersionPinMismatch {
        collection: String,
        expected: String,
        actual: String,
    },

    #[error("state verification failed for collection {collection} at step {step}: {detail}")]
    StateVerification {
        collection: String,
        step: String,
        detail: String,
    },

    #[error("migration step {step} failed on collection {collection}: {source}")]
    StepFailed {
        step: String,
        collection: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("eager materialization failed: {detail}")]
    MaterializeFailed { detail: String },

    #[error("defra node error: {0}")]
    Node(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn is_unknown_lineage(&self) -> bool {
        matches!(self, Self::UnknownLineage { .. })
    }

    pub fn is_foreign_version(&self) -> bool {
        matches!(self, Self::ForeignVersion { .. })
    }
}
