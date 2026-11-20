use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("collection {collection} still has branchable storage; DefraDB cannot change this property in place. Use a separate fresh runtime home for this build; the existing store has not been changed")]
    RuntimeLocalityMismatch { collection: String },

    #[error("baseline schema registration failed for {collection}: {source}")]
    BaselineRegister {
        collection: String,
        #[source]
        source: Box<defra_node::Error>,
    },

    #[error("managed collection {collection} missing after baseline registration")]
    CollectionMissing { collection: String },

    #[error(
        "unknown lineage for collection {collection}: versions [{versions}] are not this build's baseline; the store is not converted"
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

    #[error("defra node error: {0}")]
    Node(#[source] Box<defra_node::Error>),

    #[error("defra node error: retained file image storage requires the configured node identity")]
    MissingNodeIdentity,

    #[error("defra node error: retained file image policy does not match its schema binding")]
    FileImagePolicyMismatch,

    #[error(
        "defra node error: retained file image collection has an incompatible document policy"
    )]
    IncompatibleFileImagePolicy,

    #[error("defra node error: {collection} has an incompatible identity policy")]
    IncompatibleIdentityPolicy { collection: String },

    #[error("defra node error: identity policy does not match its schema binding")]
    IdentityPolicyMismatch,

    #[error("defra node error: invalid identity collection owner DID: {error}")]
    InvalidIdentityCollectionOwner { error: identity::Error },

    #[error(
        "defra node error: branchable identity collection {collection} belongs to a different node"
    )]
    ForeignIdentityCollectionOwner { collection: String },

    #[error("defra node error: invalid image owner DID: {error}")]
    InvalidImageOwner { error: identity::Error },

    #[error("defra node error: retained file image collection belongs to a different node")]
    ForeignFileImageOwner,
}

impl From<defra_node::Error> for SchemaError {
    fn from(error: defra_node::Error) -> Self {
        Self::Node(Box::new(error))
    }
}

impl SchemaError {
    pub fn in_chain<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        gents_protocol::error_chain::find_source::<Self>(error)
    }

    pub fn refusal(&self) -> Option<IncompatibleStoreKind> {
        match self {
            Self::UnknownLineage { .. } => Some(IncompatibleStoreKind::UnknownLineage),
            Self::ForeignVersion { .. } => Some(IncompatibleStoreKind::ForeignVersion),
            Self::RuntimeLocalityMismatch { .. } | Self::IncompatibleIdentityPolicy { .. } => {
                Some(IncompatibleStoreKind::OtherVersionSchema)
            }
            Self::BaselineRegister { .. }
            | Self::CollectionMissing { .. }
            | Self::VersionPinMismatch { .. }
            | Self::Node(_)
            | Self::MissingNodeIdentity
            | Self::FileImagePolicyMismatch
            | Self::IncompatibleFileImagePolicy
            | Self::IdentityPolicyMismatch
            | Self::InvalidIdentityCollectionOwner { .. }
            | Self::ForeignIdentityCollectionOwner { .. }
            | Self::InvalidImageOwner { .. }
            | Self::ForeignFileImageOwner => None,
        }
    }
}
