use gents_protocol::operations::document::{
    DocumentOperationError, DocumentReceipt, FieldChange, WriteMode, WriteOutcome,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{json, Map, Value};

use super::OperationFailure;
use crate::inference_backend::AdvertisedProfilePolicy;
use crate::patch::{apply_patch, diff_docs, ensure_admissible, SelfConfigPatch, SelfConfigTarget};
use crate::{
    apply_desired_state_plan_with_node_binding, read_desired_state_record_in_txn,
    validate_desired_state_plan, ConfigurationNodeBinding, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};

pub type WriteFailure = OperationFailure<DocumentOperationError>;

/// Lean `DocumentOperations.writeOutcome`.
pub fn decide_write(mode: WriteMode, stored: bool) -> WriteOutcome {
    match (mode, stored) {
        (WriteMode::Create, true) => WriteOutcome::RefusedExists,
        (WriteMode::Create, false) | (WriteMode::Save, false) => WriteOutcome::Created,
        (WriteMode::Update, true) | (WriteMode::Save, true) => WriteOutcome::Updated,
        (WriteMode::Update, false) => WriteOutcome::RefusedMissing,
    }
}

/// The patch a client holding the whole typed document stands for: every
/// writable field set to the document's value, an empty one cleared.
pub fn whole_document_patch<T>(
    target: SelfConfigTarget,
    document: &T,
) -> anyhow::Result<SelfConfigPatch>
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(document)?;
    Ok(target
        .writable_fields()
        .into_iter()
        .map(|field| (field.to_owned(), value.get(field).cloned()))
        .collect())
}

pub struct DocumentWrite<'a> {
    pub target: SelfConfigTarget,
    pub owner: &'a str,
    pub id: &'a str,
    pub mode: WriteMode,
    pub patch: &'a SelfConfigPatch,
    pub node_binding: Option<&'a ConfigurationNodeBinding>,
}

/// What a write would store, before it is validated or published.
pub struct Candidate {
    pub stored: Map<String, Value>,
    pub merged: Map<String, Value>,
    pub created: bool,
}

pub async fn candidate_in_txn(
    txn: &ConfigApplyTxn<'_>,
    write: &DocumentWrite<'_>,
) -> Result<Candidate, WriteFailure> {
    let collection = write.target.collection_name();
    if write.id.trim().is_empty() {
        return Err(OperationFailure::Operation(
            DocumentOperationError::IdRequired { collection },
        ));
    }
    ensure_admissible(write.target, write.patch)
        .map_err(|error| OperationFailure::Other(error.into()))?;
    let stored =
        read_desired_state_record_in_txn(txn, write.target.collection(), write.owner, write.id)
            .await?
            .map(|(_, value)| value.as_object().cloned().unwrap_or_default());
    let created = match decide_write(write.mode, stored.is_some()) {
        WriteOutcome::Created => true,
        WriteOutcome::Updated => false,
        WriteOutcome::RefusedExists => {
            return Err(OperationFailure::Operation(
                DocumentOperationError::Exists {
                    collection,
                    id: write.id.to_owned(),
                },
            ))
        }
        WriteOutcome::RefusedMissing => {
            return Err(OperationFailure::Operation(
                DocumentOperationError::Missing {
                    collection,
                    id: write.id.to_owned(),
                    node_did: write.owner.to_owned(),
                },
            ))
        }
    };
    let stored = stored.unwrap_or_default();
    let mut merged = apply_patch(write.target, &stored, write.patch);
    if created {
        merged.insert(write.target.unique_field().to_owned(), json!(write.id));
        merged.insert("node_did".to_owned(), json!(write.owner));
    }
    Ok(Candidate {
        stored,
        merged,
        created,
    })
}

/// Validates `candidate` against every document that selects or is selected
/// by it and, with `commit`, publishes it.
pub async fn publish_in_txn(
    txn: &ConfigApplyTxn<'_>,
    write: &DocumentWrite<'_>,
    candidate: &Candidate,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, WriteFailure> {
    let value = Value::Object(candidate.merged.clone());
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: write.target.collection(),
        add: value.clone(),
        update: value,
    }])?;
    let doc_id = if commit {
        apply_desired_state_plan_with_node_binding(txn, &plan, write.node_binding, policy).await?;
        read_desired_state_record_in_txn(txn, write.target.collection(), write.owner, write.id)
            .await?
            .map(|(doc_id, _)| doc_id)
    } else {
        validate_desired_state_plan(txn, &plan, policy).await?;
        None
    };
    Ok(DocumentReceipt {
        committed: commit,
        collection: write.target.collection_name(),
        id: write.id.to_owned(),
        node_did: write.owner.to_owned(),
        created: candidate.created,
        changed: diff_docs(write.target, &candidate.stored, &candidate.merged)
            .into_iter()
            .map(|delta| FieldChange {
                field: delta.field,
                from: delta.from,
                to: delta.to,
            })
            .collect(),
        doc_id,
    })
}

pub async fn write_in_txn(
    txn: &ConfigApplyTxn<'_>,
    write: &DocumentWrite<'_>,
    validate: &(dyn Fn(&Map<String, Value>) -> anyhow::Result<()> + Send + Sync),
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, WriteFailure> {
    let candidate = candidate_in_txn(txn, write).await?;
    validate(&candidate.merged).map_err(|error| {
        OperationFailure::Operation(DocumentOperationError::Invalid {
            collection: write.target.collection_name(),
            source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
        })
    })?;
    publish_in_txn(txn, write, &candidate, policy, commit).await
}

/// One write in its own transaction; a preview reads and validates without
/// writing.
pub async fn write(
    access: &ConfigAccess,
    operation: &'static str,
    write: &DocumentWrite<'_>,
    validate: &(dyn Fn(&Map<String, Value>) -> anyhow::Result<()> + Send + Sync),
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, WriteFailure> {
    if commit {
        access
            .transact_write(operation, |txn| {
                Box::pin(write_in_txn(txn, write, validate, policy, commit))
            })
            .await
    } else {
        access
            .transact_readonly(operation, |txn| {
                Box::pin(write_in_txn(txn, write, validate, policy, commit))
            })
            .await
    }
}

pub fn finish(failure: WriteFailure) -> DocumentOperationError {
    failure.finish(DocumentOperationError::Passthrough)
}

/// Saves a whole typed document as the client holding it: its owner and id
/// are the document's own and every writable field takes its value. A
/// create also takes the fields only a create may set (a Task's `agent_did`);
/// an update keeps their stored values.
pub async fn save_whole<T>(
    access: &ConfigAccess,
    operation: &'static str,
    target: SelfConfigTarget,
    document: &T,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentReceipt, DocumentOperationError>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let value = serde_json::to_value(document)
        .map_err(|error| DocumentOperationError::Passthrough(error.into()))?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let (owner, id) = (field("node_did"), field(target.unique_field()));
    if owner.trim().is_empty() {
        return Err(DocumentOperationError::Invalid {
            collection: target.collection_name(),
            source: "node_did is required".into(),
        });
    }
    let patch = whole_document_patch(target, document)
        .map_err(|error| DocumentOperationError::Passthrough(error.into()))?;
    let write = &DocumentWrite {
        target,
        owner: &owner,
        id: &id,
        mode: WriteMode::Save,
        patch: &patch,
        node_binding: None,
    };
    let value = &value;
    access
        .transact_write(operation, |txn| {
            Box::pin(async move {
                let mut candidate = candidate_in_txn(txn, write).await?;
                if candidate.created {
                    for name in target.all_fields() {
                        if let Some(field) = value.get(*name).filter(|field| !field.is_null()) {
                            candidate
                                .merged
                                .entry((*name).to_owned())
                                .or_insert_with(|| field.clone());
                        }
                    }
                }
                serde_json::from_value::<T>(Value::Object(candidate.merged.clone())).map_err(
                    |error| {
                        OperationFailure::Operation(DocumentOperationError::Invalid {
                            collection: target.collection_name(),
                            source: error.into(),
                        })
                    },
                )?;
                publish_in_txn(txn, write, &candidate, policy, true).await
            })
        })
        .await
        .map_err(finish)
}
