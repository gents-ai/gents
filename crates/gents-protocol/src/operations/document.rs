use std::error::Error;

use serde::{Deserialize, Serialize};

use super::agent::NextCall;

/// Lean `DocumentOperations.Actor`: who signs the transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentActor {
    Person,
    Agent,
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
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
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
