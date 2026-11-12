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

/// Lean `DocumentOperations.Confirmation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Absent,
    Current,
    Stale,
}

/// Lean `DocumentOperations.DeleteOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteOutcome {
    Deleted,
    RefusedNeedsPreview,
    RefusedMissing,
    RefusedChanged,
    RefusedReferenced,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeleteTarget {
    pub collection: &'static str,
    pub id: String,
    pub content_digest: String,
}

/// The answer to a delete or its preview. `plan_digest` identifies the stored
/// content of every target; a delete that carries it removes exactly what the
/// preview showed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeleteReceipt {
    pub committed: bool,
    pub node_did: String,
    pub targets: Vec<DeleteTarget>,
    pub plan_digest: String,
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
    #[error("deleting {collection} {id:?} needs the digest its delete preview returns; preview the delete, review it, then send the delete with that digest")]
    NeedsPreview {
        collection: &'static str,
        id: String,
    },
    #[error("the content changed since the preview: it authorized {expected:?} and the stored documents are now {current:?}; preview again")]
    Changed { expected: String, current: String },
    #[error("{collection} {id:?} is still selected by another document; remove or redirect that reference, then delete")]
    Referenced {
        collection: &'static str,
        id: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("{collection} is not valid: {source}")]
    Invalid {
        collection: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// Another owner's failure (patch admission, plan validation, the store),
    /// unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl DocumentOperationError {
    /// The call that lets the caller correct this failure on `resource`.
    pub fn recovery(&self, resource: &'static str) -> NextCall {
        match self {
            Self::Exists { .. } => NextCall::Inspect { resource },
            Self::Missing { .. } | Self::AmbiguousOwner { .. } => NextCall::List { resource },
            Self::NeedsPreview { .. } | Self::Changed { .. } => NextCall::VerbHelp {
                resource,
                verb: "delete",
            },
            Self::Referenced { .. } => NextCall::Inspect { resource },
            Self::IdRequired { .. } | Self::Invalid { .. } | Self::Passthrough(_) => {
                NextCall::ResourceHelp { resource }
            }
        }
    }

    /// Lean `DocumentOperations.DeleteOutcome`: which delete refusal this is.
    pub fn delete_refusal(&self) -> Option<DeleteOutcome> {
        match self {
            Self::NeedsPreview { .. } => Some(DeleteOutcome::RefusedNeedsPreview),
            Self::Missing { .. } => Some(DeleteOutcome::RefusedMissing),
            Self::Changed { .. } => Some(DeleteOutcome::RefusedChanged),
            Self::Referenced { .. } => Some(DeleteOutcome::RefusedReferenced),
            _ => None,
        }
    }
}
