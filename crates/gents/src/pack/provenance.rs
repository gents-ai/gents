use anyhow::{Context, Result};

use super::PackManifest;
use crate::documents::PackConfig;
use crate::store::config_client::ConfigAccess;
use crate::Collection;

const PACK_ORIGIN_PREFIX: &str = "gents:pack:";

fn is_coordinate(value: &str) -> bool {
    value.split_once('/').is_some_and(|(namespace, name)| {
        super::is_valid_pack_name(namespace) && super::is_valid_pack_name(name)
    })
}

pub fn pack_origin_tag(coordinate: &str) -> Result<String> {
    anyhow::ensure!(
        is_coordinate(coordinate),
        "pack origin requires a namespace/name coordinate"
    );
    Ok(format!("{PACK_ORIGIN_PREFIX}{coordinate}"))
}

/// Why a configuration document's pack origin cannot be read or does not
/// belong to the pack acting on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackOriginError {
    /// Lean `pack_document_unqualified_origin_refused`: a bare name cannot
    /// be told apart from the same name under another namespace.
    #[error(
        "this document carries the pack origin {origin:?}: an earlier Gents tagged pack documents \
         with the bare pack name, and this version requires a namespace/name coordinate, so it \
         cannot update, reinstall or remove the documents of a pack installed that way. To start \
         this node's packs fresh, reset its home (`gents runtime desktop reset` previews the reset \
         and backs the home up) or set up a new home, then install the pack again"
    )]
    Unqualified { origin: String },
    #[error("configuration document carries more than one pack origin")]
    Multiple,
    #[error("configuration document belongs to pack {origin:?}, not {coordinate:?}")]
    Foreign { origin: String, coordinate: String },
}

pub fn pack_origin_from_tags(tags: &[String]) -> Result<Option<&str>, PackOriginError> {
    let mut origins = tags
        .iter()
        .filter_map(|tag| tag.strip_prefix(PACK_ORIGIN_PREFIX));
    let origin = origins.next();
    if origins.next().is_some() {
        return Err(PackOriginError::Multiple);
    }
    if let Some(origin) = origin {
        if !is_coordinate(origin) {
            return Err(PackOriginError::Unqualified {
                origin: origin.to_owned(),
            });
        }
    }
    Ok(origin)
}

pub(super) fn ensure_origin_owned(
    tags: &[String],
    coordinate: &str,
) -> Result<(), PackOriginError> {
    if let Some(origin) = pack_origin_from_tags(tags)? {
        if origin != coordinate {
            return Err(PackOriginError::Foreign {
                origin: origin.to_owned(),
                coordinate: coordinate.to_owned(),
            });
        }
    }
    Ok(())
}

pub(super) async fn apply_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::installation::PackIdentity,
    config: &PackConfig,
    policy: super::installation::DriftPolicy,
    expected: Vec<crate::documents::DesiredStateExpectation>,
) -> Result<super::installation::InstallReport> {
    let documents = super::installation::installable_documents(config)?;
    let guard =
        crate::documents::DesiredStateApplyPlan::new(Vec::new())?.with_expected(expected)?;
    // The callback re-reads its expectations and re-plans the install inside
    // the transaction, so a confirmed conflict re-plans and re-signs it.
    access
        .transact_signed_conflict_rebuild("pack.documents.install", |txn| {
            let documents = documents.clone();
            let guard = &guard;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, guard).await?;
                let report = super::installation::install_in_txn(
                    txn, owner, pack, documents, policy, None, None, None,
                )
                .await
                .map_err(super::PackChangeError::into_anyhow)?;
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
    documents: &[crate::documents::DesiredStateApplyDocument],
) -> Result<crate::documents::DesiredStateApplyPlan> {
    let mut prepared = Vec::with_capacity(documents.len());
    for authored in documents {
        let mut document = authored.clone();
        let owner = document.add["node_did"]
            .as_str()
            .context("pack document is missing owner")?;
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("pack document is missing logical ID")?;
        if let Some((_, live)) =
            crate::documents::read_desired_state_record_in_txn(txn, document.collection, owner, id)
                .await?
        {
            merge_tags(&mut document.update, &live)?;
        }
        prepared.push(document);
    }
    crate::documents::DesiredStateApplyPlan::new(prepared)
}

pub fn pack_document_digests(
    config: &crate::documents::PackConfig,
) -> Result<std::collections::BTreeMap<(String, String), String>> {
    let plan = crate::documents::DesiredStateApplyPlan::from_pack_config(config)?;
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
    crate::documents::desired_state_document_digest(&canonical)
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
    ensure_origin_owned(&live_strings, desired_origin)?;
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
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let origin = pack_origin_tag(&coordinate)?;
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
        let (fields, _) = crate::documents::config_projection(collection, None)?;
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
                    authored_origin == coordinate,
                    "pack {} document carries origin for another pack {authored_origin:?}",
                    coordinate
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
mod ownership_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pack_origins_require_a_full_coordinate() {
        let bare = pack_origin_from_tags(&["gents:pack:game".into()]).unwrap_err();
        assert_eq!(
            bare,
            PackOriginError::Unqualified {
                origin: "game".into()
            }
        );
        let message = bare.to_string();
        for needle in [
            "namespace/name",
            "gents runtime desktop reset",
            "install the pack again",
        ] {
            assert!(message.contains(needle), "{message}");
        }
        let tags = ["gents:pack:example/game".into()];
        assert_eq!(pack_origin_from_tags(&tags).unwrap(), Some("example/game"));
    }

    #[test]
    fn document_ownership_matches_executable_model() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().pack_document_ownership_cases;
        assert_eq!(cases.len(), 12);
        for case in cases {
            let coordinate = case["coordinate"].as_str().unwrap();
            let origin = case["origin"].as_str();
            let mut desired = json!({ "tags": [format!("gents:pack:{coordinate}")] });
            let live = json!({ "tags": origin.map(|value| vec![format!("gents:pack:{value}")]).unwrap_or_default() });
            assert_eq!(
                merge_tags(&mut desired, &live).is_ok(),
                case["allowed"].as_bool().unwrap(),
                "{case}"
            );
            let tags: Vec<String> = origin
                .map(|value| vec![format!("gents:pack:{value}")])
                .unwrap_or_default();
            assert_eq!(
                ensure_origin_owned(&tags, coordinate).is_ok(),
                case["removal_allowed"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
