use std::error::Error;

use serde::{Deserialize, Serialize};

use super::NextCall;

/// Lean `InferenceOperations.Actor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceActor {
    Person,
    Agent,
}

/// Field to `Some(value)` to set it or `None` to clear it; later entries win.
pub type FieldPatch = Vec<(String, Option<serde_json::Value>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// The document must not exist.
    Create,
    /// The document must exist.
    Update,
    /// The patch applies to the stored document, or to an empty one.
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceDocument {
    Backend,
    Profile,
}

impl InferenceDocument {
    pub fn resource(self) -> &'static str {
        match self {
            Self::Backend => "backend",
            Self::Profile => "profile",
        }
    }

    pub fn collection_name(self) -> &'static str {
        match self {
            Self::Backend => "InferenceBackend",
            Self::Profile => "InferenceProfile",
        }
    }

    pub fn id_field(self) -> &'static str {
        match self {
            Self::Backend => "backend_id",
            Self::Profile => "profile_id",
        }
    }
}

/// An edit of one backend or profile. Fields the patch does not name keep
/// their stored value, a stored credential included.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentWrite {
    pub document: InferenceDocument,
    pub id: String,
    pub mode: WriteMode,
    pub patch: FieldPatch,
}

impl DocumentWrite {
    /// The edit a document a client holds stands for, and the node it names:
    /// every field the document carries is set, an explicit null clears its
    /// field, and a field it omits keeps its stored value.
    pub fn from_document(
        document: InferenceDocument,
        value: &serde_json::Value,
        mode: WriteMode,
    ) -> Result<(String, Self), InferenceOperationError> {
        let invalid = |message: String| InferenceOperationError::Invalid(message.into());
        let fields = value.as_object().ok_or_else(|| {
            invalid(format!(
                "{} document must be a JSON object",
                document.collection_name()
            ))
        })?;
        let text = |field: &str| {
            fields
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let node_did = text("node_did").ok_or_else(|| {
            invalid(format!(
                "the {} document names no node_did",
                document.collection_name()
            ))
        })?;
        let id = text(document.id_field()).ok_or(InferenceOperationError::IdRequired(document))?;
        let patch = fields
            .iter()
            .filter(|(field, _)| {
                !matches!(field.as_str(), "node_did" | "_docID") && *field != document.id_field()
            })
            .map(|(field, value)| {
                (
                    field.clone(),
                    Some(value.clone()).filter(|value| !value.is_null()),
                )
            })
            .collect();
        Ok((
            node_did,
            Self {
                document,
                id,
                mode,
                patch,
            },
        ))
    }
}

/// `D` is one changed field as the handler's crate types it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteReceipt<D> {
    pub committed: bool,
    pub collection: &'static str,
    pub target_id: String,
    pub created: bool,
    pub changed: Vec<D>,
    pub doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoveReceipt {
    pub committed: bool,
    pub collection: &'static str,
    pub target_id: String,
    pub removed: bool,
}

/// Lean `InferenceOperations.BackendDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendDecision {
    Admitted,
    RawCredential,
    OauthAccount,
    OauthPool,
    LocalServerOnly,
    StartsEnabled,
}

/// Lean `InferenceOperations.ChoiceDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceDecision {
    Admitted,
    OtherAccount,
    OtherPool,
}

/// Lean `InferenceOperations.DiscoveryAnswer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryAnswer {
    ContactsProvider,
    StoredObservation,
}

/// Lean `InferenceOperations.AccountOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountOperation {
    List,
    SelectForProfile,
    SetPool,
    Label,
    Disable,
    Remove,
}

impl AccountOperation {
    pub const ALL: [Self; 6] = [
        Self::List,
        Self::SelectForProfile,
        Self::SetPool,
        Self::Label,
        Self::Disable,
        Self::Remove,
    ];

    fn describe(self) -> &'static str {
        match self {
            Self::List => "listing accounts",
            Self::SelectForProfile => "moving a profile to another account",
            Self::SetPool => "switching a backend between one account and the account pool",
            Self::Label => "renaming an account",
            Self::Disable => "disabling an account",
            Self::Remove => "removing an account",
        }
    }
}

/// Which rule refused, when a failure is a refusal a preview answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceRefusal {
    RawCredential,
    OauthAccount,
    OauthPool,
    LocalServerOnly,
    StartsEnabled,
    OtherAccount,
    OtherPool,
    AccountOperation,
    TargetExists,
    TargetMissing,
    InvalidInput,
}

impl InferenceRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RawCredential => "raw_credential",
            Self::OauthAccount => "oauth_account",
            Self::OauthPool => "oauth_pool",
            Self::LocalServerOnly => "local_server_only",
            Self::StartsEnabled => "starts_enabled",
            Self::OtherAccount => "other_account",
            Self::OtherPool => "other_pool",
            Self::AccountOperation => "account_operation",
            Self::TargetExists => "target_exists",
            Self::TargetMissing => "target_missing",
            Self::InvalidInput => "invalid_input",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InferenceOperationError {
    #[error("raw credentials are operator-managed; select an environment or OAuth reference")]
    RawCredential,
    #[error(
        "OAuth account references are operator-managed; keep the backend's account_ref as stored"
    )]
    OauthAccount,
    #[error("OAuth account pools are operator-managed; keep the backend's auth as stored")]
    OauthPool,
    #[error("model-facing backend creation is limited to unauthenticated OpenAI-compatible local servers")]
    LocalServerOnly,
    #[error("a newly created local backend must be enabled for discovery")]
    StartsEnabled,
    #[error("backend {backend_id} selects another {provider} account; keep the current account, or pick an account-free backend or another provider's default account")]
    OtherAccount {
        backend_id: String,
        provider: String,
    },
    #[error("backend {backend_id} rotates a {provider} account pool the profile does not use; keep the current backend")]
    OtherPool {
        backend_id: String,
        provider: String,
    },
    #[error("{} is a User's choice, not an Agent's: an Agent reads the account inventory and changes none of it; ask a User to make the change from their own client", .0.describe())]
    AccountOperation(AccountOperation),
    #[error("{} {id:?} already exists; use update with its exact ID", .document.collection_name())]
    Exists {
        document: InferenceDocument,
        id: String,
    },
    #[error("no {} with ID {id:?} on this node; list the resource to find its exact IDs", .document.collection_name())]
    Missing {
        document: InferenceDocument,
        id: String,
    },
    #[error("{} requires a document ID", .0.resource())]
    IdRequired(InferenceDocument),
    /// The patch or the document it produces is not a valid document.
    #[error("{0}")]
    Invalid(#[source] Box<dyn Error + Send + Sync>),
    /// Another owner's failure (plan validation, the store), unchanged.
    #[error("{0}")]
    Passthrough(#[source] Box<dyn Error + Send + Sync>),
}

impl BackendDecision {
    pub fn admit(self) -> Result<(), InferenceOperationError> {
        match self {
            Self::Admitted => Ok(()),
            Self::RawCredential => Err(InferenceOperationError::RawCredential),
            Self::OauthAccount => Err(InferenceOperationError::OauthAccount),
            Self::OauthPool => Err(InferenceOperationError::OauthPool),
            Self::LocalServerOnly => Err(InferenceOperationError::LocalServerOnly),
            Self::StartsEnabled => Err(InferenceOperationError::StartsEnabled),
        }
    }
}

impl ChoiceDecision {
    /// `backend_id` and `provider` name the backend the refused write selects.
    pub fn admit(self, backend_id: &str, provider: &str) -> Result<(), InferenceOperationError> {
        let (backend_id, provider) = (backend_id.to_owned(), provider.to_owned());
        match self {
            Self::Admitted => Ok(()),
            Self::OtherAccount => Err(InferenceOperationError::OtherAccount {
                backend_id,
                provider,
            }),
            Self::OtherPool => Err(InferenceOperationError::OtherPool {
                backend_id,
                provider,
            }),
        }
    }
}

impl InferenceOperationError {
    pub fn refusal(&self) -> Option<InferenceRefusal> {
        match self {
            Self::RawCredential => Some(InferenceRefusal::RawCredential),
            Self::OauthAccount => Some(InferenceRefusal::OauthAccount),
            Self::OauthPool => Some(InferenceRefusal::OauthPool),
            Self::LocalServerOnly => Some(InferenceRefusal::LocalServerOnly),
            Self::StartsEnabled => Some(InferenceRefusal::StartsEnabled),
            Self::OtherAccount { .. } => Some(InferenceRefusal::OtherAccount),
            Self::OtherPool { .. } => Some(InferenceRefusal::OtherPool),
            Self::AccountOperation(_) => Some(InferenceRefusal::AccountOperation),
            Self::Exists { .. } => Some(InferenceRefusal::TargetExists),
            Self::Missing { .. } => Some(InferenceRefusal::TargetMissing),
            Self::IdRequired(_) | Self::Invalid(_) => Some(InferenceRefusal::InvalidInput),
            Self::Passthrough(_) => None,
        }
    }

    /// The call that lets the caller correct this failure of a `verb` on
    /// `document`.
    pub fn recovery(&self, document: InferenceDocument, verb: &'static str) -> NextCall {
        match self {
            Self::RawCredential
            | Self::OauthAccount
            | Self::OauthPool
            | Self::LocalServerOnly
            | Self::StartsEnabled => NextCall::ResourceHelp {
                resource: "backend",
            },
            Self::OtherAccount { .. } | Self::OtherPool { .. } | Self::AccountOperation(_) => {
                NextCall::Verb {
                    resource: "backend",
                    verb: "accounts",
                }
            }
            Self::Missing { document, .. } => NextCall::List {
                resource: document.resource(),
            },
            Self::Exists { document, .. } => NextCall::VerbHelp {
                resource: document.resource(),
                verb: "update",
            },
            Self::IdRequired(_) | Self::Invalid(_) | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: document.resource(),
                verb,
            },
        }
    }

    /// The other owner's failure an `Invalid` or `Passthrough` carries.
    /// `source()` skips it, so a boundary that classifies that owner's errors
    /// asks here.
    pub fn passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Invalid(error) | Self::Passthrough(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
