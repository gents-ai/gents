use std::collections::BTreeSet;

use gents_protocol::operations::document::{
    DeleteDigest, DeleteOutcome, DeleteStep, DeletedDocument, DocumentActor, DocumentDeleteError,
    DocumentDeletePreview, DocumentDeleteReceipt,
};
use gents_store::collection::Collection;
use gents_store::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, TransactionFailure,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::inference_backend::AdvertisedProfilePolicy;
use crate::{
    apply_desired_state_plan, desired_state_document_digest, read_desired_state_record_in_txn,
    validate_desired_state_plan, DesiredStateApplyPlan,
};

/// Lean `DocumentOperations.deleteOutcome`.
pub fn delete_outcome(
    actor: DocumentActor,
    step: DeleteStep,
    digest: DeleteDigest,
    present: bool,
    retained_valid: bool,
) -> DeleteOutcome {
    if !present {
        return DeleteOutcome::TargetMissing;
    }
    match (step, actor, digest) {
        (DeleteStep::Commit, _, DeleteDigest::Stale) => DeleteOutcome::DigestStale,
        (DeleteStep::Commit, DocumentActor::Agent, DeleteDigest::Absent) => {
            DeleteOutcome::DigestRequired
        }
        _ if !retained_valid => DeleteOutcome::StillReferenced,
        (DeleteStep::Preview, _, _) => DeleteOutcome::Previewed,
        (DeleteStep::Commit, _, _) => DeleteOutcome::Deleted,
    }
}

#[derive(Debug)]
enum DeleteFailure {
    Operation(DocumentDeleteError),
    Other(anyhow::Error),
}

impl From<DocumentDeleteError> for DeleteFailure {
    fn from(error: DocumentDeleteError) -> Self {
        Self::Operation(error)
    }
}

impl From<anyhow::Error> for DeleteFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<ConfigClientError> for DeleteFailure {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl From<DeleteFailure> for DocumentDeleteError {
    fn from(error: DeleteFailure) -> Self {
        match error {
            DeleteFailure::Operation(error) => error,
            DeleteFailure::Other(error) => {
                Self::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
            }
        }
    }
}

impl TransactionFailure for DeleteFailure {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}

fn removal_plan(
    owner: &str,
    targets: &[(Collection, String)],
) -> Result<DesiredStateApplyPlan, DocumentDeleteError> {
    if targets.is_empty() {
        return Err(DocumentDeleteError::NoTargets);
    }
    let mut seen = BTreeSet::new();
    for (collection, id) in targets {
        if !seen.insert((*collection, id.as_str())) {
            return Err(DocumentDeleteError::DuplicateTarget {
                collection: collection.graphql_type(),
                id: id.clone(),
            });
        }
    }
    let passthrough = |error: anyhow::Error| {
        DocumentDeleteError::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
    };
    DesiredStateApplyPlan::new(Vec::new())
        .map_err(passthrough)?
        .with_removals(
            targets
                .iter()
                .map(|(collection, id)| (*collection, owner.to_owned(), id.clone()))
                .collect(),
        )
        .map_err(passthrough)
}

fn plan_digest(owner: &str, targets: &[DeletedDocument]) -> Result<String, serde_json::Error> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&json!({
            "owner": owner,
            "targets": targets,
        }))?)
    ))
}

#[derive(Clone, Copy)]
struct DeleteRequest<'a> {
    owner: &'a str,
    targets: &'a [(Collection, String)],
    plan: &'a DesiredStateApplyPlan,
    actor: DocumentActor,
    step: DeleteStep,
    expected_digest: Option<&'a str>,
}

async fn delete_step_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &DeleteRequest<'_>,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentDeleteReceipt, DeleteFailure> {
    let DeleteRequest {
        owner,
        targets,
        plan,
        actor,
        step,
        expected_digest,
    } = *request;
    let mut read = Vec::with_capacity(targets.len());
    let mut missing = None;
    for (collection, id) in targets {
        match read_desired_state_record_in_txn(txn, *collection, owner, id).await? {
            Some((_, document)) => read.push(DeletedDocument {
                collection: collection.graphql_type(),
                id: id.clone(),
                content_digest: desired_state_document_digest(&document)?,
            }),
            None => {
                missing.get_or_insert((*collection, id));
            }
        }
    }
    let current = plan_digest(owner, &read).map_err(anyhow::Error::from)?;
    let digest = match expected_digest {
        None => DeleteDigest::Absent,
        Some(expected) if expected == current => DeleteDigest::Current,
        Some(_) => DeleteDigest::Stale,
    };
    let referrer = if missing.is_none() {
        let removed = targets.iter().cloned().collect::<BTreeSet<_>>();
        let referrer = crate::ConfigReferences::load_in_txn(txn, owner)
            .await?
            .referrer_of_removed(&removed)?;
        if referrer.is_none() {
            validate_desired_state_plan(txn, plan, policy).await?;
        }
        referrer
    } else {
        None
    };
    match delete_outcome(actor, step, digest, missing.is_none(), referrer.is_none()) {
        DeleteOutcome::TargetMissing => {
            let (collection, id) = missing.expect("a missing target was observed");
            Err(DocumentDeleteError::TargetMissing {
                collection: collection.graphql_type(),
                id: id.clone(),
            }
            .into())
        }
        DeleteOutcome::DigestRequired => Err(DocumentDeleteError::DigestRequired.into()),
        DeleteOutcome::DigestStale => Err(DocumentDeleteError::DigestStale {
            expected: expected_digest.unwrap_or_default().to_owned(),
            current,
        }
        .into()),
        DeleteOutcome::StillReferenced => {
            let referrer = referrer.expect("a retained referrer was observed");
            Err(DocumentDeleteError::StillReferenced {
                collection: referrer.collection.graphql_type(),
                id: referrer.id,
                field: referrer.field,
                target: referrer.target.graphql_type(),
                target_id: referrer.target_id,
            }
            .into())
        }
        DeleteOutcome::Previewed => Ok(DocumentDeleteReceipt {
            committed: false,
            targets: read,
            plan_digest: current,
            owner: owner.to_owned(),
        }),
        DeleteOutcome::Deleted => {
            apply_desired_state_plan(txn, plan, policy).await?;
            for (collection, id) in targets {
                if read_desired_state_record_in_txn(txn, *collection, owner, id)
                    .await?
                    .is_some()
                {
                    return Err(DocumentDeleteError::NotRemoved {
                        collection: collection.graphql_type(),
                        id: id.clone(),
                    }
                    .into());
                }
            }
            Ok(DocumentDeleteReceipt {
                committed: true,
                targets: read,
                plan_digest: current,
                owner: owner.to_owned(),
            })
        }
    }
}

/// Deletes exactly `targets` of `owner` in one transaction.
pub async fn delete(
    access: &ConfigAccess,
    operation: &'static str,
    owner: &str,
    targets: &[(Collection, String)],
    actor: DocumentActor,
    expected_digest: Option<&str>,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentDeleteReceipt, DocumentDeleteError> {
    let plan = removal_plan(owner, targets)?;
    let request = &DeleteRequest {
        owner,
        targets,
        plan: &plan,
        actor,
        step: DeleteStep::Commit,
        expected_digest,
    };
    access
        .transact_write(operation, move |txn| {
            Box::pin(delete_step_in_txn(txn, request, policy))
        })
        .await
        .map_err(|error: DeleteFailure| error.into())
}

/// What `delete` of the same targets would answer, without writing.
pub async fn preview_delete(
    access: &ConfigAccess,
    operation: &'static str,
    owner: &str,
    targets: &[(Collection, String)],
    actor: DocumentActor,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentDeletePreview, DocumentDeleteError> {
    let plan = removal_plan(owner, targets)?;
    let request = &DeleteRequest {
        owner,
        targets,
        plan: &plan,
        actor,
        step: DeleteStep::Preview,
        expected_digest: None,
    };
    let previewed = access
        .transact_readonly(operation, move |txn| {
            Box::pin(delete_step_in_txn(txn, request, policy))
        })
        .await
        .map_err(|error: DeleteFailure| DocumentDeleteError::from(error));
    match previewed {
        Ok(receipt) => Ok(DocumentDeletePreview::Admitted(receipt)),
        Err(error) => match error.refusal() {
            Some(refusal) => Ok(DocumentDeletePreview::Refused { refusal, error }),
            None => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn delete_outcomes_match_lean() {
        gents_lean_vocab::families::document_operations_delete_outcome::replay(|i| {
            super::delete_outcome(i.actor, i.step, i.digest, i.present, i.retained_valid)
        });
    }
}
