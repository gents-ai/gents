use anyhow::{Context, Result};

use super::PackManifest;
use crate::config_client::ConfigAccess;
use crate::document_config::PackConfig;
use crate::Collection;

const PACK_ORIGIN_PREFIX: &str = "gents:pack:";

pub fn pack_origin_tag(pack_name: &str) -> Result<String> {
    anyhow::ensure!(
        super::is_valid_pack_name(pack_name),
        "pack origin requires a snake_case pack name"
    );
    Ok(format!("{PACK_ORIGIN_PREFIX}{pack_name}"))
}

pub fn pack_origin_from_tags(tags: &[String]) -> Result<Option<&str>> {
    let mut origins = tags
        .iter()
        .filter_map(|tag| tag.strip_prefix(PACK_ORIGIN_PREFIX));
    let origin = origins.next();
    anyhow::ensure!(
        origins.next().is_none(),
        "configuration document carries more than one pack origin"
    );
    if let Some(origin) = origin {
        anyhow::ensure!(
            super::is_valid_pack_name(origin),
            "configuration document carries invalid pack origin {origin:?}"
        );
    }
    Ok(origin)
}

/// Publish a prepared document pack through the canonical desired-state owner.
/// The principal and bound inference documents are retained, never rewritten.
pub(super) async fn apply_pack_documents(
    access: &ConfigAccess,
    config: &PackConfig,
) -> Result<crate::config_client::DesiredStateApplyCounts> {
    let bundle = crate::config_client::DesiredStateApplyPlan::from_pack_config(config)?;
    let documents = bundle
        .documents()
        .iter()
        .filter(|document| document.collection != Collection::AgentPrincipal)
        .cloned()
        .collect::<Vec<_>>();
    access
        .transact("pack.documents.install", |txn| {
            let documents = &documents;
            Box::pin(async move {
                let plan = prepare_pack_plan_in_txn(txn, documents, false).await?;
                crate::config_client::validate_desired_state_plan(txn, &plan).await?;
                crate::config_client::apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
}

/// Merge retained discovery tags without treating them as package identity.
/// Immutable graph resources compare every canonical field except tags; the
/// package-authored add value remains the artifact digest owner.
pub(crate) async fn prepare_pack_plan_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    documents: &[crate::config_client::DesiredStateApplyDocument],
    immutable: bool,
) -> Result<crate::config_client::DesiredStateApplyPlan> {
    let mut prepared = Vec::with_capacity(documents.len());
    for authored in documents {
        let mut document = authored.clone();
        let owner = document.add["agent_did"]
            .as_str()
            .context("pack document is missing owner")?;
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("pack document is missing logical ID")?;
        if let Some((_, live)) = crate::config_client::read_desired_state_record_in_txn(
            txn,
            document.collection,
            owner,
            id,
        )
        .await?
        {
            if immutable {
                anyhow::ensure!(
                    pack_artifact_document_digest(&document.add)?
                        == pack_artifact_document_digest(&live)?,
                    "immutable package resource {} {owner:?}/{id:?} drifted",
                    document.collection.graphql_type()
                );
            }
            merge_tags(&mut document.update, &live)?;
        }
        prepared.push(document);
    }
    crate::config_client::DesiredStateApplyPlan::new(prepared)
}

/// Discovery tags are mutable labels, not immutable package identity.
pub(crate) fn pack_artifact_document_digest(value: &serde_json::Value) -> Result<String> {
    let mut canonical = value.clone();
    canonical
        .as_object_mut()
        .context("pack document must be an object")?
        .remove("tags");
    crate::config_client::desired_state_document_digest(&canonical)
}

fn merge_tags(desired: &mut serde_json::Value, live: &serde_json::Value) -> Result<()> {
    let Some(desired_tags) = desired.get_mut("tags") else {
        return Ok(());
    };
    let mut merged = desired_tags
        .as_array()
        .context("pack document tags must be an array")?
        .clone();
    let desired_strings = merged
        .iter()
        .map(|tag| {
            tag.as_str()
                .map(ToOwned::to_owned)
                .context("pack document tag must be a string")
        })
        .collect::<Result<Vec<_>>>()?;
    let desired_origin = pack_origin_from_tags(&desired_strings)?
        .context("prepared pack document is missing its origin tag")?;
    let live_strings = live
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|tag| {
            tag.as_str()
                .map(ToOwned::to_owned)
                .context("live document tag must be a string")
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(live_origin) = pack_origin_from_tags(&live_strings)? {
        anyhow::ensure!(
            live_origin == desired_origin,
            "configuration document belongs to pack {live_origin:?}, not {desired_origin:?}"
        );
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
) -> Result<PackConfig> {
    let origin = pack_origin_tag(&manifest.name)?;
    let mut value = serde_json::to_value(config)?;
    let root = value
        .as_object_mut()
        .context("pack config must be an object")?;
    for collection in Collection::ALL {
        if collection == Collection::AgentPrincipal
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
        let (fields, _) = crate::config_client::config_projection(collection, None)?;
        if !fields.contains(&"tags") {
            continue;
        }
        for document in root
            .get_mut(field)
            .and_then(serde_json::Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let object = document
                .as_object_mut()
                .with_context(|| format!("{field} pack document must be an object"))?;
            let mut tags = object
                .remove("tags")
                .filter(|value| !value.is_null())
                .map(serde_json::from_value::<Vec<String>>)
                .transpose()?
                .unwrap_or_default();
            if let Some(authored_origin) = pack_origin_from_tags(&tags)? {
                anyhow::ensure!(
                    authored_origin == manifest.name,
                    "pack {} document carries origin for another pack {authored_origin:?}",
                    manifest.name
                );
            }
            if !tags.iter().any(|tag| tag == &origin) {
                tags.push(origin.clone());
            }
            tags.sort();
            tags.dedup();
            object.insert("tags".to_owned(), serde_json::to_value(tags)?);
        }
    }
    serde_json::from_value(value).context("decoding provenance-stamped pack configuration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_tags_are_unique_and_valid() {
        assert_eq!(
            pack_origin_tag("code_review").unwrap(),
            "gents:pack:code_review"
        );
        assert_eq!(
            pack_origin_from_tags(&["review".into(), "gents:pack:code_review".into()]).unwrap(),
            Some("code_review")
        );
        assert!(pack_origin_from_tags(&[
            "gents:pack:code_review".into(),
            "gents:pack:pipeline".into(),
        ])
        .is_err());
        let left = serde_json::json!({"agent_did":"owner","task_id":"task","tags":["one"]});
        let right = serde_json::json!({"agent_did":"owner","task_id":"task","tags":["two"]});
        assert_eq!(
            pack_artifact_document_digest(&left).unwrap(),
            pack_artifact_document_digest(&right).unwrap()
        );
    }
}
