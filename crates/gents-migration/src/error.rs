use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("baseline schema registration failed for {collection}: {source}")]
    BaselineRegister {
        collection: String,
        #[source]
        source: Box<defra_node::Error>,
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
        source: Box<defra_node::Error>,
    },

    #[error(
        "migration step {step} failed on collection {collection}: PatchVersioned steps require expected_version pin"
    )]
    MissingVersionPin { step: String, collection: String },

    #[error("eager materialization failed: {detail}")]
    MaterializeFailed { detail: String },

    #[error("defra node error: {0}")]
    Node(Box<defra_node::Error>),

    #[error("defra node error: retained file image storage requires the configured node identity")]
    MissingNodeIdentity,

    #[error("defra node error: retained file image policy does not match its schema binding")]
    FileImagePolicyMismatch,

    #[error(
        "defra node error: retained file image collection has an incompatible document policy"
    )]
    IncompatibleFileImagePolicy,

    #[error("defra node error: invalid image owner DID: {error}")]
    InvalidImageOwner { error: identity::Error },

    #[error("defra node error: retained file image collection belongs to a different node")]
    ForeignFileImageOwner,
}

impl From<defra_node::Error> for Error {
    fn from(error: defra_node::Error) -> Self {
        Self::Node(Box::new(error))
    }
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
