use std::path::{Path, PathBuf};

use gents_protocol::document_query::{fields::skill as fields, Filter, Query, Selection};
use gents_protocol::operations::document::{
    DocumentDeleteReceipt, DocumentOperationError, DocumentReceipt, OperationActor, OwnerCount,
    OwnerResolution, WriteMode,
};
use gents_protocol::operations::skill::{SkillImportOutcome, SkillOperationError};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{json, Map, Value};

use super::write::{candidate_in_txn, publish_in_txn, DocumentWrite};
use super::{decide_owner, OperationFailure};
use crate::inference_backend::AdvertisedProfilePolicy;
use crate::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::skills::import::load_skill_source;
use crate::{ConfigurationNodeBinding, SkillDocument, SKILL_FIELDS};

pub type SkillFailure = OperationFailure<SkillOperationError>;

/// Lean `DocumentOperations.importOutcome`.
pub fn decide_import(
    actor: OperationActor,
    inside_tool_root: bool,
    stored: bool,
    overwrite: bool,
) -> SkillImportOutcome {
    if actor == OperationActor::Agent && !inside_tool_root {
        SkillImportOutcome::RefusedOutsideRoot
    } else if stored && !overwrite {
        SkillImportOutcome::RefusedExists
    } else if stored {
        SkillImportOutcome::Replaced
    } else {
        SkillImportOutcome::Created
    }
}

fn valid(merged: &Map<String, Value>) -> anyhow::Result<()> {
    serde_json::from_value::<SkillDocument>(Value::Object(merged.clone()))?;
    Ok(())
}

pub fn finish(failure: SkillFailure) -> SkillOperationError {
    failure.finish(|error| DocumentOperationError::Passthrough(error).into())
}

pub struct SkillWrite<'a> {
    pub owner: &'a str,
    pub skill_id: &'a str,
    pub mode: WriteMode,
    pub patch: &'a SelfConfigPatch,
    pub node_binding: Option<&'a ConfigurationNodeBinding>,
}

async fn save_in_txn(
    txn: &ConfigApplyTxn<'_>,
    write: &SkillWrite<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, SkillFailure> {
    let document = DocumentWrite {
        target: SelfConfigTarget::Skill,
        owner: write.owner,
        id: write.skill_id,
        mode: write.mode,
        patch: write.patch,
        node_binding: write.node_binding,
    };
    let mut candidate = candidate_in_txn(txn, &document)
        .await
        .map_err(|failure| failure.map(SkillOperationError::Document))?;
    if candidate.created {
        candidate.merged.insert(
            "created_at".to_owned(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
    }
    valid(&candidate.merged).map_err(|error| {
        OperationFailure::Operation(SkillOperationError::Document(
            DocumentOperationError::Invalid {
                collection: SelfConfigTarget::Skill.collection_name(),
                source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
            },
        ))
    })?;
    publish_in_txn(txn, &document, &candidate, policy, commit)
        .await
        .map_err(|failure| failure.map(SkillOperationError::Document))
}

/// Creates or edits one Skill of `owner`. With `commit` false nothing is
/// written and the receipt is what the write would return.
pub async fn save(
    access: &ConfigAccess,
    write: &SkillWrite<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, SkillFailure> {
    if commit {
        access
            .transact_write("operations.skill.save", |txn| {
                Box::pin(save_in_txn(txn, write, policy, commit))
            })
            .await
    } else {
        access
            .transact_readonly("operations.skill.preview", |txn| {
                Box::pin(save_in_txn(txn, write, policy, commit))
            })
            .await
    }
}

/// The fields a whole Skill document sets and clears, for a client that saves
/// one: every writable field takes the document's value.
pub fn document_patch(document: &SkillDocument) -> anyhow::Result<SelfConfigPatch> {
    let value = serde_json::to_value(document)?;
    Ok(SelfConfigTarget::Skill
        .writable_fields()
        .into_iter()
        .map(|field| (field.to_owned(), value.get(field).cloned()))
        .collect())
}

/// How the path of a skill source is admitted before it is read.
pub enum SkillSourceRoot<'a> {
    /// A User reads any path the process can.
    Unconfined,
    /// An Agent reads only what `resolve` places inside its tool root.
    ToolRoot(&'a (dyn Fn(&Path) -> anyhow::Result<PathBuf> + Send + Sync)),
}

impl SkillSourceRoot<'_> {
    fn actor(&self) -> OperationActor {
        match self {
            Self::Unconfined => OperationActor::User,
            Self::ToolRoot(_) => OperationActor::Agent,
        }
    }
}

pub struct SkillImport<'a> {
    pub owner: &'a str,
    pub skill_id: &'a str,
    pub source: &'a Path,
    pub root: SkillSourceRoot<'a>,
    pub overwrite: bool,
    pub enabled: bool,
    pub node_binding: Option<&'a ConfigurationNodeBinding>,
}

/// Reads one skill directory or `SKILL.md` as `import.root` admits it.
pub fn load_source(import: &SkillImport<'_>) -> Result<SkillDocument, SkillOperationError> {
    let path = import.source.display().to_string();
    let confined = match &import.root {
        SkillSourceRoot::Unconfined => Ok(()),
        SkillSourceRoot::ToolRoot(resolve) => resolve(import.source).map(|_| ()),
    };
    let outcome = decide_import(
        import.root.actor(),
        confined.is_ok(),
        false,
        import.overwrite,
    );
    if let (SkillImportOutcome::RefusedOutsideRoot, Err(error)) = (outcome, confined) {
        return Err(SkillOperationError::OutsideToolRoot {
            path,
            source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
        });
    }
    let loaded = match &import.root {
        SkillSourceRoot::Unconfined => {
            load_skill_source(import.source, import.skill_id, import.owner, |path| {
                Ok(std::fs::canonicalize(path)?)
            })
        }
        SkillSourceRoot::ToolRoot(resolve) => {
            load_skill_source(import.source, import.skill_id, import.owner, resolve)
        }
    };
    let mut document = loaded.map_err(|error| SkillOperationError::Source {
        path,
        source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
    })?;
    document.enabled = import.enabled;
    Ok(document)
}

async fn import_in_txn(
    txn: &ConfigApplyTxn<'_>,
    import: &SkillImport<'_>,
    patch: &SelfConfigPatch,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, SkillFailure> {
    let stored = crate::read_desired_state_record_in_txn(
        txn,
        SelfConfigTarget::Skill.collection(),
        import.owner,
        import.skill_id,
    )
    .await?
    .is_some();
    let mode = match decide_import(import.root.actor(), true, stored, import.overwrite) {
        SkillImportOutcome::Created => WriteMode::Create,
        SkillImportOutcome::Replaced => WriteMode::Update,
        SkillImportOutcome::RefusedExists | SkillImportOutcome::RefusedOutsideRoot => {
            return Err(OperationFailure::Operation(
                SkillOperationError::ImportExists {
                    skill_id: import.skill_id.to_owned(),
                },
            ))
        }
    };
    let write = SkillWrite {
        owner: import.owner,
        skill_id: import.skill_id,
        mode,
        patch,
        node_binding: import.node_binding,
    };
    save_in_txn(txn, &write, policy, commit).await
}

/// Imports one skill source as a Skill of `import.owner`.
pub async fn import(
    access: &ConfigAccess,
    import: &SkillImport<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, SkillFailure> {
    let document = load_source(import).map_err(OperationFailure::Operation)?;
    let patch = document_patch(&document)?;
    let patch = &patch;
    if commit {
        access
            .transact_write("operations.skill.import", |txn| {
                Box::pin(import_in_txn(txn, import, patch, policy, commit))
            })
            .await
    } else {
        access
            .transact_readonly("operations.skill.import_preview", |txn| {
                Box::pin(import_in_txn(txn, import, patch, policy, commit))
            })
            .await
    }
}

/// The Skills of `owner` as stored, ordered by ID.
pub async fn list(access: &ConfigAccess, owner: &str) -> anyhow::Result<Vec<Value>> {
    let rendered = Query::select(Selection::owned(SKILL_FIELDS))
        .filter(Filter::eq(fields::NODE_DID, owner))
        .render();
    let response = access.read(&rendered).await?;
    let mut skills = response["data"]["Skill"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    skills.sort_by(|left, right| {
        left["skill_id"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["skill_id"].as_str().unwrap_or_default())
    });
    Ok(skills)
}

/// One Skill of `owner` as stored.
pub async fn get(
    access: &ConfigAccess,
    owner: &str,
    skill_id: &str,
) -> Result<Value, SkillOperationError> {
    let rendered = Query::select(Selection::owned(SKILL_FIELDS))
        .filter(Filter::eq(fields::NODE_DID, owner).and(Filter::eq(fields::SKILL_ID, skill_id)))
        .limit(1)
        .render();
    let response = access
        .read(&rendered)
        .await
        .map_err(|error| DocumentOperationError::Passthrough(error.into()))?;
    response["data"]["Skill"]
        .as_array()
        .and_then(|rows| rows.first().cloned())
        .ok_or_else(|| {
            DocumentOperationError::Missing {
                collection: SelfConfigTarget::Skill.collection_name(),
                id: skill_id.to_owned(),
                node_did: owner.to_owned(),
            }
            .into()
        })
}

/// The owner a call that may omit it acts on (Lean
/// `DocumentOperations.ownerResolution`): the named owner, or the only owner
/// holding `skill_id` among the Skills the acting identity can read.
pub async fn resolve_owner(
    access: &ConfigAccess,
    owner: Option<&str>,
    skill_id: &str,
) -> Result<String, SkillOperationError> {
    let collection = SelfConfigTarget::Skill.collection_name();
    if decide_owner(owner.is_some(), OwnerCount::None) == OwnerResolution::Named {
        return Ok(owner.unwrap_or_default().to_owned());
    }
    let rendered = Query::select(Selection::new().field(fields::NODE_DID))
        .filter(Filter::eq(fields::SKILL_ID, skill_id))
        .render();
    let response = access
        .read(&rendered)
        .await
        .map_err(|error| DocumentOperationError::Passthrough(error.into()))?;
    let mut owners = response["data"]["Skill"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["node_did"].as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    owners.sort();
    owners.dedup();
    let count = match owners.len() {
        0 => OwnerCount::None,
        1 => OwnerCount::One,
        _ => OwnerCount::Several,
    };
    match decide_owner(false, count) {
        OwnerResolution::Named | OwnerResolution::Sole => Ok(owners.remove(0)),
        OwnerResolution::RefusedMissing => Err(DocumentOperationError::Missing {
            collection,
            id: skill_id.to_owned(),
            node_did: "any owner this identity can read".to_owned(),
        }
        .into()),
        OwnerResolution::RefusedAmbiguous => Err(DocumentOperationError::AmbiguousOwner {
            collection,
            id: skill_id.to_owned(),
            owners,
        }
        .into()),
    }
}

/// Deletes one Skill of `owner` through the one document delete.
pub async fn delete(
    access: &ConfigAccess,
    actor: OperationActor,
    owner: &str,
    skill_id: &str,
    digest: Option<&str>,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentDeleteReceipt, SkillFailure> {
    let targets = [(SelfConfigTarget::Skill.collection(), skill_id.to_owned())];
    super::delete::delete(
        access,
        "operations.skill.delete",
        owner,
        &targets,
        actor,
        digest,
        policy,
    )
    .await
    .map_err(|error| OperationFailure::Operation(SkillOperationError::Delete(error)))
}
