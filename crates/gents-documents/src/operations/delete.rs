use gents_protocol::operations::document::{
    Confirmation, DeleteOutcome, DeleteReceipt, DeleteTarget, DocumentOperationError,
    OperationActor, OwnerCount, OwnerResolution,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::OperationFailure;
use crate::inference_backend::AdvertisedProfilePolicy;
use crate::patch::SelfConfigTarget;
use crate::{
    apply_desired_state_plan, desired_state_document_digest, read_desired_state_record_in_txn,
    validate_desired_state_plan, DesiredStateApplyPlan, MissingReference,
};

/// A refused delete. The reference that keeps a document stays typed so a
/// client can name the document that still selects it.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error("{collection} {id:?} is still selected by another document; remove or redirect that reference, then delete")]
    Referenced {
        collection: &'static str,
        id: String,
        #[source]
        reference: MissingReference,
    },
    #[error(transparent)]
    Document(#[from] DocumentOperationError),
}

impl From<DeleteError> for DocumentOperationError {
    fn from(error: DeleteError) -> Self {
        match error {
            DeleteError::Referenced {
                collection,
                id,
                reference,
            } => Self::Referenced {
                collection,
                id,
                source: Box::new(reference),
            },
            DeleteError::Document(error) => error,
        }
    }
}

pub type DeleteFailure = OperationFailure<DeleteError>;

pub fn finish(failure: DeleteFailure) -> DocumentOperationError {
    failure
        .map(DocumentOperationError::from)
        .finish(DocumentOperationError::Passthrough)
}

/// Lean `DocumentOperations.deleteOutcome`.
pub fn decide_delete(
    actor: OperationActor,
    confirmation: Confirmation,
    stored: bool,
    referenced: bool,
) -> DeleteOutcome {
    if actor == OperationActor::Agent && confirmation == Confirmation::Absent {
        DeleteOutcome::RefusedNeedsPreview
    } else if !stored {
        DeleteOutcome::RefusedMissing
    } else if confirmation == Confirmation::Stale {
        DeleteOutcome::RefusedChanged
    } else if referenced {
        DeleteOutcome::RefusedReferenced
    } else {
        DeleteOutcome::Deleted
    }
}

/// Lean `DocumentOperations.ownerResolution`.
pub fn decide_owner(owner_named: bool, owners: OwnerCount) -> OwnerResolution {
    if owner_named {
        return OwnerResolution::Named;
    }
    match owners {
        OwnerCount::None => OwnerResolution::RefusedMissing,
        OwnerCount::One => OwnerResolution::Sole,
        OwnerCount::Several => OwnerResolution::RefusedAmbiguous,
    }
}

pub struct DocumentDelete<'a> {
    pub actor: OperationActor,
    pub owner: &'a str,
    pub targets: &'a [(SelfConfigTarget, String)],
    /// The plan digest a preview of the same targets returned.
    pub digest: Option<&'a str>,
}

fn plan_digest(owner: &str, targets: &[DeleteTarget]) -> anyhow::Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&json!({
            "owner": owner,
            "targets": targets
                .iter()
                .map(|target| json!({
                    "collection": target.collection,
                    "id": target.id,
                    "content_digest": target.content_digest,
                }))
                .collect::<Vec<_>>(),
        }))?)
    ))
}

fn refusal(
    outcome: DeleteOutcome,
    target: SelfConfigTarget,
    id: &str,
    delete: &DocumentDelete<'_>,
    current: &str,
    reference: Option<MissingReference>,
) -> Option<DeleteError> {
    let collection = target.collection_name();
    let id = id.to_owned();
    match (outcome, reference) {
        (DeleteOutcome::Deleted, _) => None,
        (DeleteOutcome::RefusedNeedsPreview, _) => {
            Some(DocumentOperationError::NeedsPreview { collection, id }.into())
        }
        (DeleteOutcome::RefusedMissing, _) => Some(
            DocumentOperationError::Missing {
                collection,
                id,
                node_did: delete.owner.to_owned(),
            }
            .into(),
        ),
        (DeleteOutcome::RefusedChanged, _) => Some(
            DocumentOperationError::Changed {
                expected: delete.digest.unwrap_or_default().to_owned(),
                current: current.to_owned(),
            }
            .into(),
        ),
        (DeleteOutcome::RefusedReferenced, Some(reference)) => Some(DeleteError::Referenced {
            collection: reference.target.graphql_type(),
            id: reference.target_id.clone(),
            reference,
        }),
        (DeleteOutcome::RefusedReferenced, None) => Some(
            DocumentOperationError::Passthrough("a reference to the document remains".into())
                .into(),
        ),
    }
}

/// Removes `targets` of one owner together, or with `commit` false reports
/// what the removal would be. A document still selected by one that stays is
/// kept, and so is every other target.
pub async fn delete_in_txn(
    txn: &ConfigApplyTxn<'_>,
    delete: &DocumentDelete<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DeleteReceipt, DeleteFailure> {
    let confirmation_absent = if delete.digest.is_some() {
        Confirmation::Current
    } else {
        Confirmation::Absent
    };
    let mut targets = Vec::with_capacity(delete.targets.len());
    let mut missing = None;
    for (target, id) in delete.targets {
        let stored =
            read_desired_state_record_in_txn(txn, target.collection(), delete.owner, id).await?;
        match stored {
            Some((_, document)) => targets.push(DeleteTarget {
                collection: target.collection_name(),
                id: id.clone(),
                content_digest: desired_state_document_digest(&document)?,
            }),
            None => missing = missing.or(Some((*target, id.as_str()))),
        }
    }
    let first = delete
        .targets
        .first()
        .map(|(target, id)| (*target, id.as_str()));
    let (subject, id) = missing.or(first).ok_or_else(|| {
        DeleteFailure::Other(anyhow::anyhow!("a delete names at least one document"))
    })?;
    let current = plan_digest(delete.owner, &targets)?;
    let confirmation = match delete.digest {
        Some(expected) if expected != current => Confirmation::Stale,
        _ => confirmation_absent,
    };
    let confirmation = if commit {
        confirmation
    } else {
        Confirmation::Current
    };
    let removed = delete
        .targets
        .iter()
        .map(|(target, id)| (target.collection(), id.clone()))
        .collect::<Vec<_>>();
    let stored = missing.is_none();
    let reference = if stored {
        crate::ConfigReferences::load_in_txn(txn, delete.owner)
            .await?
            .retained_reference_to(&removed)?
    } else {
        None
    };
    let outcome = decide_delete(delete.actor, confirmation, stored, reference.is_some());
    if let Some(error) = refusal(outcome, subject, id, delete, &current, reference) {
        return Err(OperationFailure::Operation(error));
    }
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(
        removed
            .into_iter()
            .map(|(collection, id)| (collection, delete.owner.to_owned(), id))
            .collect(),
    )?;
    validate_desired_state_plan(txn, &plan, policy).await?;
    if commit {
        apply_desired_state_plan(txn, &plan, policy).await?;
    }
    Ok(DeleteReceipt {
        committed: commit,
        node_did: delete.owner.to_owned(),
        targets,
        plan_digest: current,
    })
}

pub async fn delete(
    access: &ConfigAccess,
    operation: &'static str,
    delete: &DocumentDelete<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DeleteReceipt, DeleteFailure> {
    if commit {
        access
            .transact_write(operation, |txn| {
                Box::pin(delete_in_txn(txn, delete, policy, commit))
            })
            .await
    } else {
        access
            .transact_readonly(operation, |txn| {
                Box::pin(delete_in_txn(txn, delete, policy, commit))
            })
            .await
    }
}
