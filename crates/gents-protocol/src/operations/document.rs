use std::error::Error;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::NextCall;

/// Lean `DocumentOperations.Actor`: who acts. A person signs as the User; an
/// Agent signs as itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationActor {
    User,
    Agent,
}

/// Lean `DocumentOperations.FieldUpdate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldUpdate {
    Omitted,
    Clear,
    Set,
}

/// Lean `DocumentOperations.FieldAfter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldAfter {
    Absent,
    Stored,
    Requested,
}

/// Lean `DocumentOperations.WriteMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    Create,
    Update,
    Save,
}

/// Lean `DocumentOperations.WriteOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Created,
    Updated,
    RefusedExists,
    RefusedMissing,
}

/// Lean `DocumentOperations.OwnerCount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerCount {
    None,
    One,
    Several,
}

/// Lean `DocumentOperations.OwnerResolution`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerResolution {
    Named,
    Sole,
    RefusedMissing,
    RefusedAmbiguous,
}

/// A patch: field to `Some(value)` to set, `None` to clear. Later entries win.
pub type DocumentPatch = Vec<(String, Option<Value>)>;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocumentReceipt {
    pub committed: bool,
    pub collection: &'static str,
    pub id: String,
    pub node_did: String,
    pub created: bool,
    pub changed: Vec<FieldChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentOperationError {
    #[error("{collection} requires a non-empty ID")]
    IdRequired { collection: &'static str },
    #[error("{collection} {id:?} already exists; update it by its exact ID")]
    Exists {
        collection: &'static str,
        id: String,
    },
    #[error("no {collection} with ID {id:?} is owned by {node_did}; list this resource to find its exact IDs")]
    Missing {
        collection: &'static str,
        id: String,
        node_did: String,
    },
    #[error("{collection} {id:?} exists under more than one owner ({}); name the owner", owners.join(", "))]
    AmbiguousOwner {
        collection: &'static str,
        id: String,
        owners: Vec<String>,
    },
    #[error("{collection} is not valid: {source}")]
    Invalid {
        collection: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// Another owner's failure (patch admission, plan validation, the store),
    /// unchanged.
    #[error("{0}")]
    Passthrough(#[source] Box<dyn Error + Send + Sync>),
}

impl DocumentOperationError {
    /// The call that lets the caller correct this failure on `resource`.
    pub fn recovery(&self, resource: &'static str) -> NextCall {
        match self {
            Self::Exists { .. } => NextCall::Inspect { resource },
            Self::Missing { .. } | Self::AmbiguousOwner { .. } => NextCall::List { resource },
            Self::IdRequired { .. } | Self::Invalid { .. } | Self::Passthrough(_) => {
                NextCall::ResourceHelp { resource }
            }
        }
    }
}

/// Lean `DocumentOperations.Step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteStep {
    Preview,
    Commit,
}

/// Lean `DocumentOperations.Digest`: the digest a commit presents against the
/// digest of the targets as its own transaction reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteDigest {
    Absent,
    Current,
    Stale,
}

/// Lean `DocumentOperations.DeleteOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteOutcome {
    Previewed,
    Deleted,
    TargetMissing,
    DigestRequired,
    DigestStale,
    StillReferenced,
}

impl DeleteOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Previewed => "previewed",
            Self::Deleted => "deleted",
            Self::TargetMissing => "target_missing",
            Self::DigestRequired => "digest_required",
            Self::DigestStale => "digest_stale",
            Self::StillReferenced => "still_referenced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeletedDocument {
    pub collection: &'static str,
    pub id: String,
    pub content_digest: String,
}

/// The delete handler's answer. Fields serialize in declaration order, which
/// is the order every front end shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentDeleteReceipt {
    pub committed: bool,
    pub targets: Vec<DeletedDocument>,
    /// Authorizes a commit of exactly these targets with this content.
    pub plan_digest: String,
    pub owner: String,
}

/// Lean `DocumentOperations.deleteOutcome` for a preview. A refused preview is
/// a successful answer carrying the error the refused commit would raise;
/// each front end renders its message and next call. Failures that are not
/// refusals stay errors.
#[derive(Debug)]
pub enum DocumentDeletePreview {
    Admitted(DocumentDeleteReceipt),
    Refused {
        refusal: DeleteOutcome,
        error: DocumentDeleteError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentDeleteError {
    #[error("delete requires at least one target")]
    NoTargets,
    #[error("duplicate delete target {collection} {id:?}")]
    DuplicateTarget {
        collection: &'static str,
        id: String,
    },
    #[error("no owned {collection} with ID {id:?}; list this resource to find its exact IDs before deleting")]
    TargetMissing {
        collection: &'static str,
        id: String,
    },
    #[error("an Agent deletes only what it previewed; preview this delete and repeat it with the returned digest")]
    DigestRequired,
    #[error("delete target content changed: preview authorized {expected:?}, current plan is {current:?}; preview again")]
    DigestStale { expected: String, current: String },
    #[error("refused: after the delete, {collection} {id} field {field} references missing {target} {target_id:?}; update that document to remove or redirect the reference, then retry the delete")]
    StillReferenced {
        collection: &'static str,
        id: String,
        field: String,
        target: &'static str,
        target_id: String,
    },
    #[error("delete did not remove {collection} {id:?}")]
    NotRemoved {
        collection: &'static str,
        id: String,
    },
    /// Another owner's failure (the store, plan construction), unchanged.
    #[error("{0}")]
    Passthrough(#[source] Box<dyn Error + Send + Sync>),
}

impl DocumentDeleteError {
    /// Lean `DocumentOperations.DeleteOutcome`: which check refused, when this
    /// failure is a refusal.
    pub fn refusal(&self) -> Option<DeleteOutcome> {
        match self {
            Self::TargetMissing { .. } => Some(DeleteOutcome::TargetMissing),
            Self::DigestRequired => Some(DeleteOutcome::DigestRequired),
            Self::DigestStale { .. } => Some(DeleteOutcome::DigestStale),
            Self::StillReferenced { .. } => Some(DeleteOutcome::StillReferenced),
            Self::NoTargets
            | Self::DuplicateTarget { .. }
            | Self::NotRemoved { .. }
            | Self::Passthrough(_) => None,
        }
    }

    /// The call that lets the caller correct this failure. `resource` is the
    /// caller's own name for what the call addresses: the referring document's
    /// resource for `StillReferenced`, the target's otherwise.
    pub fn recovery(&self, resource: &'static str) -> NextCall {
        match self {
            Self::TargetMissing { .. } => NextCall::List { resource },
            Self::StillReferenced { .. } => NextCall::Inspect { resource },
            Self::NoTargets
            | Self::DuplicateTarget { .. }
            | Self::DigestRequired
            | Self::DigestStale { .. }
            | Self::NotRemoved { .. }
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource,
                verb: "delete",
            },
        }
    }
}
