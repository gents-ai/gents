use anyhow::{Context, Result};

use super::PackManifest;
use crate::documents::PackConfig;
use crate::store::config_client::ConfigAccess;
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

pub(super) async fn apply_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::installation::PackIdentity,
    config: &PackConfig,
    policy: super::installation::DriftPolicy,
    expected: Vec<crate::store::config_client::DesiredStateExpectation>,
) -> Result<super::installation::InstallReport> {
    let documents = super::installation::installable_documents(config)?;
    let guard = crate::store::config_client::DesiredStateApplyPlan::new(Vec::new())?
        .with_expected(expected)?;
    access
        .transact_write("pack.documents.install", |txn| {
            let documents = documents.clone();
            let guard = &guard;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, guard).await?;
                let report =
                    super::installation::install_in_txn(txn, owner, pack, documents, policy)
                        .await?;
                super::installation::validate_plugin_replacements_in_txn(
                    txn,
                    owner,
                    std::slice::from_ref(pack),
                )
                .await?;
                Ok(report)
            })
        })
        .await
}

pub(crate) async fn prepare_pack_plan_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    documents: &[crate::store::config_client::DesiredStateApplyDocument],
) -> Result<crate::store::config_client::DesiredStateApplyPlan> {
    let mut prepared = Vec::with_capacity(documents.len());
    for authored in documents {
        let mut document = authored.clone();
        let owner = document.add["node_did"]
            .as_str()
            .context("pack document is missing owner")?;
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("pack document is missing logical ID")?;
        if let Some((_, live)) = crate::store::config_client::read_desired_state_record_in_txn(
            txn,
            document.collection,
            owner,
            id,
        )
        .await?
        {
            merge_tags(&mut document.update, &live)?;
        }
        prepared.push(document);
    }
    crate::store::config_client::DesiredStateApplyPlan::new(prepared)
}

pub fn pack_document_digests(
    config: &crate::documents::PackConfig,
) -> Result<std::collections::BTreeMap<(String, String), String>> {
    let plan = crate::store::config_client::DesiredStateApplyPlan::from_pack_config(config)?;
    plan.documents()
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .context("configuration logical ID missing")?
                .to_owned();
            Ok((
                (document.collection.graphql_type().to_owned(), id),
                pack_artifact_document_digest(&document.add)?,
            ))
        })
        .collect()
}

pub(crate) fn pack_artifact_document_digest(value: &serde_json::Value) -> Result<String> {
    let mut canonical = value.clone();
    canonical
        .as_object_mut()
        .context("pack document must be an object")?
        .remove("tags");
    crate::store::config_client::desired_state_document_digest(&canonical)
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
        let (fields, _) = crate::store::config_client::config_projection(collection, None)?;
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
