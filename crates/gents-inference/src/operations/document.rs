use anyhow::Context;
use gents_documents::patch::{
    apply_patch, diff_docs, ensure_admissible, FieldDelta, SelfConfigTarget,
};
use gents_documents::{
    apply_desired_state_plan_with_node_binding, load_inference_backend_in_txn,
    read_desired_state_record_in_txn, validate_desired_state_plan, BackendAuth,
    ConfigurationNodeBinding, DesiredStateApplyDocument, DesiredStateApplyPlan, InferenceBackend,
    InferenceProfile,
};
use gents_protocol::operations::inference::{
    DocumentWrite, InferenceDocument, InferenceOperationError, InferenceOutcome, RemoveReceipt,
    WriteMode,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{json, Map, Value};

use super::decision::{
    acting, backend_create_decision, backend_update_decision, profile_choice_decision,
};
use super::OperationFailure;
use crate::backends::provider::BackendProviderOauthExt;
use crate::resolved::ProviderProfilePolicy;

pub type WriteReceipt = gents_protocol::operations::inference::WriteReceipt<FieldDelta>;

fn target(document: InferenceDocument) -> SelfConfigTarget {
    match document {
        InferenceDocument::Backend => SelfConfigTarget::InferenceBackend,
        InferenceDocument::Profile => SelfConfigTarget::InferenceProfile,
    }
}

fn invalid(error: impl Into<anyhow::Error>) -> OperationFailure {
    InferenceOperationError::Invalid(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
    .into()
}

fn single_plan(
    document: InferenceDocument,
    candidate: &Map<String, Value>,
) -> anyhow::Result<DesiredStateApplyPlan> {
    let value = Value::Object(candidate.clone());
    DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: target(document).collection(),
        add: value.clone(),
        update: value,
    }])
}

/// The edit a complete document stands for, and the node it names. A client
/// that holds the whole typed document has no omitted fields: every writable
/// field is set to the document's value and one it leaves empty is cleared.
/// A partial document a person wrote is `DocumentWrite::from_document`.
pub fn whole_document_write<T>(
    document: InferenceDocument,
    value: &T,
    mode: WriteMode,
) -> Result<(String, DocumentWrite), InferenceOperationError>
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(value)
        .map_err(|error| InferenceOperationError::Invalid(error.into()))?;
    let (node_did, mut write) = DocumentWrite::from_document(document, &value, mode)?;
    for field in target(document).writable_fields() {
        if !write.patch.iter().any(|(name, _)| name == field) {
            write.patch.push((field.to_owned(), None));
        }
    }
    Ok((node_did, write))
}

/// The stored document and the candidate a write's patch produces, before
/// any rule has admitted it.
#[derive(Debug, Clone)]
pub struct MergedWrite {
    document: InferenceDocument,
    node_did: String,
    id: String,
    stored: Option<Map<String, Value>>,
    candidate: Map<String, Value>,
}

/// A write every rule admitted, ready to commit in the transaction that
/// planned it.
#[derive(Debug, Clone)]
pub struct PlannedWrite(MergedWrite);

impl MergedWrite {
    pub fn stored(&self) -> Option<&Map<String, Value>> {
        self.stored.as_ref()
    }

    pub fn candidate(&self) -> &Map<String, Value> {
        &self.candidate
    }

    pub fn created(&self) -> bool {
        self.stored.is_none()
    }

    /// Lean `InferenceOperations.backendUpdateDecision`,
    /// `backendCreateDecision` and `profileWriteDecision` for the actor that
    /// signs `txn`, then the document's own validation.
    pub async fn admit_in_txn(
        self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<PlannedWrite, OperationFailure> {
        let actor = acting(txn);
        let decode_auth = |doc: &Map<String, Value>| {
            serde_json::from_value::<BackendAuth>(doc.get("auth").cloned().unwrap_or(Value::Null))
                .context("backend auth is not valid")
        };
        match self.document {
            InferenceDocument::Backend => {
                let candidate: InferenceBackend =
                    serde_json::from_value(Value::Object(self.candidate.clone()))
                        .context("InferenceBackend document is not valid")
                        .map_err(invalid)?;
                match &self.stored {
                    Some(stored) => backend_update_decision(
                        actor,
                        &decode_auth(stored).map_err(invalid)?,
                        &candidate.auth,
                    ),
                    None => backend_create_decision(actor, &candidate),
                }
                .admit()?;
                candidate.validate().map_err(invalid)?;
            }
            InferenceDocument::Profile => {
                let candidate: InferenceProfile =
                    serde_json::from_value(Value::Object(self.candidate.clone()))
                        .context("InferenceProfile document is not valid")
                        .map_err(invalid)?;
                candidate.validate().map_err(invalid)?;
                let current_id = self
                    .stored
                    .as_ref()
                    .and_then(|stored| stored.get("backend_id"))
                    .and_then(Value::as_str);
                if current_id != Some(candidate.backend_id.as_str()) {
                    admit_backend_choice_in_txn(
                        txn,
                        &self.node_did,
                        current_id,
                        &candidate.backend_id,
                    )
                    .await?;
                }
            }
        }
        Ok(PlannedWrite(self))
    }
}

impl PlannedWrite {
    pub fn stored(&self) -> Option<&Map<String, Value>> {
        self.0.stored.as_ref()
    }

    pub fn candidate(&self) -> &Map<String, Value> {
        &self.0.candidate
    }

    pub fn created(&self) -> bool {
        self.0.created()
    }

    /// The fields the write changes. A changed `auth` shows its kind and
    /// references, never a secret.
    pub fn changed(&self) -> Vec<FieldDelta> {
        let empty = Map::new();
        let mut deltas = diff_docs(
            target(self.0.document),
            self.0.stored.as_ref().unwrap_or(&empty),
            &self.0.candidate,
        );
        for delta in &mut deltas {
            if delta.field == "auth" {
                for value in [&mut delta.from, &mut delta.to] {
                    if let Value::Object(auth) = value {
                        auth.retain(|key, _| {
                            matches!(
                                key.as_str(),
                                "kind" | "variable" | "account_ref" | "allow_overage" | "id"
                            )
                        });
                    }
                }
            }
        }
        deltas
    }

    fn receipt(&self, committed: bool, doc_id: Option<String>) -> WriteReceipt {
        WriteReceipt {
            committed,
            collection: self.0.document.collection_name(),
            target_id: self.0.id.clone(),
            created: self.created(),
            changed: self.changed(),
            doc_id,
        }
    }

    /// What the commit would write, checked against the node's references.
    pub async fn validate_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<WriteReceipt, OperationFailure> {
        validate_desired_state_plan(
            txn,
            &single_plan(self.0.document, &self.0.candidate)?,
            &ProviderProfilePolicy,
        )
        .await?;
        Ok(self.receipt(false, None))
    }

    pub async fn commit_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<WriteReceipt, OperationFailure> {
        apply_desired_state_plan_with_node_binding(
            txn,
            &single_plan(self.0.document, &self.0.candidate)?,
            binding,
            &ProviderProfilePolicy,
        )
        .await?;
        let doc_id = read_desired_state_record_in_txn(
            txn,
            target(self.0.document).collection(),
            &self.0.node_did,
            &self.0.id,
        )
        .await?
        .context("the written document is missing")?
        .0;
        Ok(self.receipt(true, Some(doc_id)))
    }
}

/// Lean `InferenceOperations.profileChoiceDecision` for the actor that signs
/// `txn`: whether a selection that uses backend `current_backend_id` may move
/// to `next_backend_id`. A profile write, an Agent or Compaction that selects
/// another profile, and a pack slot binding all ask here.
pub async fn admit_backend_choice_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    current_backend_id: Option<&str>,
    next_backend_id: &str,
) -> Result<(), OperationFailure> {
    let current = match current_backend_id {
        Some(id) => load_inference_backend_in_txn(txn, node_did, id).await?,
        None => None,
    };
    let next = load_inference_backend_in_txn(txn, node_did, next_backend_id)
        .await?
        .ok_or_else(|| InferenceOperationError::Missing {
            document: InferenceDocument::Backend,
            id: next_backend_id.to_owned(),
        })?;
    let default_account = match (&next.auth, next.provider_kind.oauth_provider()) {
        (BackendAuth::NodeOAuth { .. }, Some(provider)) => {
            crate::auth::credential::provider_default_account_ref(txn, node_did, provider).await?
        }
        _ => None,
    };
    profile_choice_decision(
        acting(txn),
        current.as_ref(),
        &next,
        default_account.as_deref(),
    )
    .admit(&next.backend_id, &next.provider_kind.to_string())?;
    Ok(())
}

/// Reads the stored document and applies the write's patch to it. A field
/// the patch does not name keeps its stored value.
pub async fn merge_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    write: &DocumentWrite,
) -> Result<MergedWrite, OperationFailure> {
    let target = target(write.document);
    if write.id.trim().is_empty() {
        return Err(InferenceOperationError::IdRequired(write.document).into());
    }
    ensure_admissible(target, &write.patch).map_err(invalid)?;
    let stored = read_desired_state_record_in_txn(txn, target.collection(), node_did, &write.id)
        .await?
        .map(|(_, value)| match value {
            Value::Object(doc) => Ok(doc),
            _ => Err(anyhow::anyhow!("config object required")),
        })
        .transpose()?;
    match (&stored, write.mode) {
        (Some(_), WriteMode::Create) => {
            return Err(InferenceOperationError::Exists {
                document: write.document,
                id: write.id.clone(),
            }
            .into())
        }
        (None, WriteMode::Update) => {
            return Err(InferenceOperationError::Missing {
                document: write.document,
                id: write.id.clone(),
            }
            .into())
        }
        _ => {}
    }
    let empty = Map::new();
    let mut candidate = apply_patch(target, stored.as_ref().unwrap_or(&empty), &write.patch);
    if stored.is_none() {
        candidate.insert(target.unique_field().into(), json!(write.id));
        candidate.insert("node_did".into(), json!(node_did));
    }
    Ok(MergedWrite {
        document: write.document,
        node_did: node_did.to_owned(),
        id: write.id.clone(),
        stored,
        candidate,
    })
}

pub async fn plan_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    write: &DocumentWrite,
) -> Result<PlannedWrite, OperationFailure> {
    merge_in_txn(txn, node_did, write)
        .await?
        .admit_in_txn(txn)
        .await
}

/// The write inside a caller's transaction, for a save that writes several
/// documents atomically.
pub async fn write_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    write: &DocumentWrite,
) -> Result<WriteReceipt, OperationFailure> {
    plan_in_txn(txn, node_did, write)
        .await?
        .commit_in_txn(txn, None)
        .await
}

/// Edits or creates one backend or profile as whoever `access` signs for.
pub async fn write(
    access: &ConfigAccess,
    node_did: &str,
    write: &DocumentWrite,
) -> Result<WriteReceipt, InferenceOperationError> {
    access
        .transact_write("inference.document.write", |txn| {
            Box::pin(write_in_txn(txn, node_did, write))
        })
        .await
        .map_err(Into::into)
}

/// The receipt the write would return, or the refusal it would raise.
pub async fn preview(
    access: &ConfigAccess,
    node_did: &str,
    write: &DocumentWrite,
    verb: &'static str,
) -> Result<InferenceOutcome<WriteReceipt>, InferenceOperationError> {
    let planned: Result<WriteReceipt, OperationFailure> = access
        .transact_readonly("inference.document.preview", |txn| {
            Box::pin(async move {
                plan_in_txn(txn, node_did, write)
                    .await?
                    .validate_in_txn(txn)
                    .await
            })
        })
        .await;
    match planned {
        Ok(receipt) => Ok(InferenceOutcome::Admitted(receipt)),
        Err(failure) => {
            let error = InferenceOperationError::from(failure);
            let next = error.recovery(write.document, verb);
            error.preview_outcome(next)
        }
    }
}

/// Removes one backend or profile. The shared apply refuses a document
/// another one still selects; an absent one is reported as not removed.
pub async fn remove_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    document: InferenceDocument,
    id: &str,
) -> Result<RemoveReceipt, OperationFailure> {
    if id.trim().is_empty() {
        return Err(InferenceOperationError::IdRequired(document).into());
    }
    let collection = target(document).collection();
    let existed = read_desired_state_record_in_txn(txn, collection, node_did, id)
        .await?
        .is_some();
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        collection,
        node_did.to_owned(),
        id.to_owned(),
    )])?;
    apply_desired_state_plan_with_node_binding(txn, &plan, None, &ProviderProfilePolicy).await?;
    Ok(RemoveReceipt {
        committed: true,
        collection: document.collection_name(),
        target_id: id.to_owned(),
        removed: existed,
    })
}

pub async fn remove(
    access: &ConfigAccess,
    node_did: &str,
    document: InferenceDocument,
    id: &str,
) -> Result<RemoveReceipt, InferenceOperationError> {
    access
        .transact_write("inference.document.remove", |txn| {
            Box::pin(remove_in_txn(txn, node_did, document, id))
        })
        .await
        .map_err(Into::into)
}
