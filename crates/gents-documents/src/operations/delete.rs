use gents_protocol::error_chain::find_map_source;
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

pub type DeleteFailure = OperationFailure<DocumentOperationError>;

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
    reference: Option<anyhow::Error>,
) -> Option<DocumentOperationError> {
    let collection = target.collection_name();
    match outcome {
        DeleteOutcome::Deleted => None,
        DeleteOutcome::RefusedNeedsPreview => Some(DocumentOperationError::NeedsPreview {
            collection,
            id: id.to_owned(),
        }),
        DeleteOutcome::RefusedMissing => Some(DocumentOperationError::Missing {
            collection,
            id: id.to_owned(),
            node_did: delete.owner.to_owned(),
        }),
        DeleteOutcome::RefusedChanged => Some(DocumentOperationError::Changed {
            expected: delete.digest.unwrap_or_default().to_owned(),
            current: current.to_owned(),
        }),
        DeleteOutcome::RefusedReferenced => Some(DocumentOperationError::Referenced {
            collection,
            id: id.to_owned(),
            source: reference
                .map(anyhow::Error::reallocate_into_boxed_dyn_error_without_backtrace)
                .unwrap_or_else(|| "a reference to it remains".into()),
        }),
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
        OperationFailure::Other(anyhow::anyhow!("a delete names at least one document"))
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
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(
        delete
            .targets
            .iter()
            .map(|(target, id)| (target.collection(), delete.owner.to_owned(), id.clone()))
            .collect(),
    )?;
    let stored = missing.is_none();
    let reference = if stored
        && decide_delete(delete.actor, confirmation, stored, false) == DeleteOutcome::Deleted
    {
        match validate_desired_state_plan(txn, &plan, policy).await {
            Ok(()) => None,
            Err(error)
                if find_map_source(error.as_ref(), |cause| {
                    cause.downcast_ref::<MissingReference>()
                })
                .is_some() =>
            {
                Some(error)
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let outcome = decide_delete(delete.actor, confirmation, stored, reference.is_some());
    if let Some(error) = refusal(outcome, subject, id, delete, &current, reference) {
        return Err(OperationFailure::Operation(error));
    }
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
