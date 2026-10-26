use std::collections::BTreeMap;

use super::{PackError, PackManifest};
use crate::config_client::{
    config_projection, desired_state_document_digest, read_desired_state_record_in_txn,
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    DesiredStateError,
};
use crate::document_config::PackConfig;
use crate::Collection;

const PACK_ORIGIN_PREFIX: &str = "gents:pack:";

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("pack document is missing owner")]
    MissingOwner,
    #[error("pack document is missing logical ID")]
    MissingLogicalId,
    #[error("immutable package resource {collection} {owner:?}/{id:?} drifted")]
    ImmutableDrift {
        collection: String,
        owner: String,
        id: String,
    },
    #[error("pack document must be an object")]
    DocumentNotObject,
    #[error("pack document tags must be an array")]
    TagsNotArray,
    #[error("pack document tag must be a string")]
    TagNotString,
    #[error("prepared pack document is missing its origin tag")]
    MissingOriginTag,
    #[error("live document tag must be a string")]
    LiveTagNotString,
    #[error("configuration document belongs to pack {live_origin:?}, not {desired_origin:?}")]
    OriginMismatch {
        live_origin: String,
        desired_origin: String,
    },
    #[error("configuration logical ID missing")]
    ConfigLogicalIdMissing,
    #[error("pack config must be an object")]
    ConfigNotObject,
    #[error("{field} pack document must be an object")]
    StampedDocumentNotObject { field: String },
    #[error("pack {pack} document carries origin for another pack {authored_origin:?}")]
    ForeignOrigin {
        pack: String,
        authored_origin: String,
    },
    #[error("decoding provenance-stamped pack configuration")]
    DecodeStamped(#[source] serde_json::Error),
}

impl crate::config_client::TransactionFailure for ProvenanceError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::DesiredState(error) => error.config_client_error(),
            _ => None,
        }
    }
}

pub fn pack_origin_tag(pack_name: &str) -> Result<String, PackError> {
    if !super::is_valid_pack_name(pack_name) {
        return Err(PackError::OriginName);
    }
    Ok(format!("{PACK_ORIGIN_PREFIX}{pack_name}"))
}

pub fn pack_origin_from_tags(tags: &[String]) -> Result<Option<&str>, PackError> {
    let mut origins = tags
        .iter()
        .filter_map(|tag| tag.strip_prefix(PACK_ORIGIN_PREFIX));
    let origin = origins.next();
    if origins.next().is_some() {
        return Err(PackError::MultipleOrigins);
    }
    if let Some(origin) = origin {
        if !super::is_valid_pack_name(origin) {
            return Err(PackError::InvalidOrigin {
                origin: origin.to_owned(),
            });
        }
    }
    Ok(origin)
}

pub(super) async fn apply_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::installation::PackIdentity,
    config: &PackConfig,
    policy: super::installation::DriftPolicy,
    expected: Vec<crate::config_client::DesiredStateExpectation>,
) -> Result<super::installation::InstallReport, super::installation::InstallationError> {
    let documents = super::installation::installable_documents(config)?;
    let guard = DesiredStateApplyPlan::new(Vec::new())?.with_expected(expected)?;
    access
        .transact_write("pack.documents.install", |txn| {
            let documents = documents.clone();
            let guard = &guard;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, guard).await?;
                super::installation::install_in_txn(txn, owner, pack, documents, policy).await
            })
        })
        .await
}

pub(crate) async fn prepare_pack_plan_in_txn(
    txn: &ConfigApplyTxn<'_>,
    documents: &[DesiredStateApplyDocument],
    immutable: bool,
) -> Result<DesiredStateApplyPlan, ProvenanceError> {
    let mut prepared = Vec::with_capacity(documents.len());
    for authored in documents {
        let mut document = authored.clone();
        let owner = document.add["node_did"]
            .as_str()
            .ok_or(ProvenanceError::MissingOwner)?;
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .ok_or(ProvenanceError::MissingLogicalId)?;
        if let Some((_, live)) =
            read_desired_state_record_in_txn(txn, document.collection, owner, id).await?
        {
            if immutable
                && pack_artifact_document_digest(&document.add)?
                    != pack_artifact_document_digest(&live)?
            {
                return Err(ProvenanceError::ImmutableDrift {
                    collection: document.collection.graphql_type().to_owned(),
                    owner: owner.to_owned(),
                    id: id.to_owned(),
                });
            }
            merge_tags(&mut document.update, &live)?;
        }
        prepared.push(document);
    }
    Ok(DesiredStateApplyPlan::new(prepared)?)
}

pub fn pack_document_digests(
    config: &PackConfig,
) -> Result<BTreeMap<(String, String), String>, ProvenanceError> {
    let plan = DesiredStateApplyPlan::from_pack_config(config)?;
    plan.documents()
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .ok_or(ProvenanceError::ConfigLogicalIdMissing)?
                .to_owned();
            Ok((
                (document.collection.graphql_type().to_owned(), id),
                pack_artifact_document_digest(&document.add)?,
            ))
        })
        .collect()
}

pub(crate) fn pack_artifact_document_digest(
    value: &serde_json::Value,
) -> Result<String, ProvenanceError> {
    let mut canonical = value.clone();
    canonical
        .as_object_mut()
        .ok_or(ProvenanceError::DocumentNotObject)?
        .remove("tags");
    Ok(desired_state_document_digest(&canonical)?)
}

fn merge_tags(
    desired: &mut serde_json::Value,
    live: &serde_json::Value,
) -> Result<(), ProvenanceError> {
    let Some(desired_tags) = desired.get_mut("tags") else {
        return Ok(());
    };
    let mut merged = desired_tags
        .as_array()
        .ok_or(ProvenanceError::TagsNotArray)?
        .clone();
    let desired_strings = merged
        .iter()
        .map(|tag| {
            tag.as_str()
                .map(ToOwned::to_owned)
                .ok_or(ProvenanceError::TagNotString)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let desired_origin =
        pack_origin_from_tags(&desired_strings)?.ok_or(ProvenanceError::MissingOriginTag)?;
    let live_strings = live
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|tag| {
            tag.as_str()
                .map(ToOwned::to_owned)
                .ok_or(ProvenanceError::LiveTagNotString)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(live_origin) = pack_origin_from_tags(&live_strings)? {
        if live_origin != desired_origin {
            return Err(ProvenanceError::OriginMismatch {
                live_origin: live_origin.to_owned(),
                desired_origin: desired_origin.to_owned(),
            });
        }
    }
    for tag in live
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if !merged.contains(tag) {
            merged.push(tag.clone());
        }
    }
    merged.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    *desired_tags = serde_json::Value::Array(merged);
    Ok(())
}

pub(super) fn stamp_pack_origin(
    manifest: &PackManifest,
    config: &PackConfig,
) -> Result<PackConfig, ProvenanceError> {
    let origin = pack_origin_tag(&manifest.name)?;
    let mut value = serde_json::to_value(config)?;
    let root = value
        .as_object_mut()
        .ok_or(ProvenanceError::ConfigNotObject)?;
    for collection in Collection::ALL {
        if collection == Collection::Node
            || matches!(
                collection,
                Collection::InferenceBackend
                    | Collection::InferenceProfile
                    | Collection::InferenceSampling
                    | Collection::InferenceExecution
                    | Collection::InferenceRetryPolicy
            )
        {
            continue;
        }
        let Some(field) = collection.dir_name() else {
            continue;
        };
        let (fields, _) = config_projection(collection, None)?;
        if !fields.contains(&"tags") {
            continue;
        }
        for document in root
            .get_mut(field)
            .and_then(serde_json::Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let object = document.as_object_mut().ok_or_else(|| {
                ProvenanceError::StampedDocumentNotObject {
                    field: field.to_owned(),
                }
            })?;
            let mut tags = object
                .remove("tags")
                .filter(|value| !value.is_null())
                .map(serde_json::from_value::<Vec<String>>)
                .transpose()?
                .unwrap_or_default();
            if let Some(authored_origin) = pack_origin_from_tags(&tags)? {
                if authored_origin != manifest.name {
                    return Err(ProvenanceError::ForeignOrigin {
                        pack: manifest.name.clone(),
                        authored_origin: authored_origin.to_owned(),
                    });
                }
            }
            if !tags.iter().any(|tag| tag == &origin) {
                tags.push(origin.clone());
            }
            tags.sort();
            tags.dedup();
            object.insert("tags".to_owned(), serde_json::to_value(tags)?);
        }
    }
    serde_json::from_value(value).map_err(ProvenanceError::DecodeStamped)
}
