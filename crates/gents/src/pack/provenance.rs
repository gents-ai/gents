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
    expected: Vec<crate::config_client::DesiredStateExpectation>,
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
            let expected = expected.clone();
            Box::pin(async move {
                let plan = prepare_pack_plan_in_txn(txn, documents, false)
                    .await?
                    .with_expected(expected)?;
                crate::config_client::validate_desired_state_plan(txn, &plan).await?;
                crate::config_client::apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
}

/// Live digest of every document [`apply_pack_documents`] is about to create
/// or replace, read ahead of the install so the caller can pass the result
/// back as `expected`. Every target carries an expectation, so the guarded
/// scope is exactly the set of documents the install writes: a document that
/// exists is expected to still match this read, and one that is absent
/// carries `digest: None`, which the desired-state owner reads as "expected
/// absent" and refuses if another writer created it meanwhile.
pub(super) async fn replaced_document_expectations(
    access: &ConfigAccess,
    config: &PackConfig,
) -> Result<Vec<crate::config_client::DesiredStateExpectation>> {
    let bundle = crate::config_client::DesiredStateApplyPlan::from_pack_config(config)?;
    let documents = bundle
        .documents()
        .iter()
        .filter(|document| document.collection != Collection::AgentPrincipal)
        .cloned()
        .collect::<Vec<_>>();
    access
        .transact("pack.documents.expected", |txn| {
            let documents = &documents;
            Box::pin(async move {
                let mut expected = Vec::new();
                for document in documents {
                    let owner = document.add["agent_did"]
                        .as_str()
                        .context("pack document is missing owner")?;
                    let id = document.add[document.collection.unique_field()]
                        .as_str()
                        .context("pack document is missing logical ID")?;
                    let digest = crate::config_client::read_desired_state_document_in_txn(
                        txn,
                        document.collection,
                        owner,
                        id,
                    )
                    .await?
                    .map(|live| crate::config_client::desired_state_document_digest(&live))
                    .transpose()?;
                    expected.push(crate::config_client::DesiredStateExpectation {
                        collection: document.collection,
                        owner: owner.to_owned(),
                        id: id.to_owned(),
                        digest,
                    });
                }
                Ok(expected)
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

    #[tokio::test]
    async fn install_refuses_a_replaced_document_edited_after_the_expectation_read() -> Result<()> {
        use defra_node::EmbeddedNode;
        use serde_json::json;
        use std::sync::Arc;

        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:pack-owner";
        crate::document_config::ensure_agent_principal(&node, owner).await?;
        let access = ConfigAccess::Local(node.clone());

        let config: PackConfig = serde_json::from_value(json!({
            "agent_principal": {"agent_did": owner},
            "tools": [{
                "agent_did": owner,
                "tools_id": "shared-tools",
                "display_name": "Pack-authored",
                "tags": ["gents:pack:test_pack"],
            }],
        }))?;

        let expected = replaced_document_expectations(&access, &config).await?;
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].digest, None);
        apply_pack_documents(&access, &config, expected).await?;

        let expected = replaced_document_expectations(&access, &config).await?;
        assert_eq!(expected.len(), 1);

        let edited = json!({
            "agent_did": owner,
            "tools_id": "shared-tools",
            "display_name": "Edited out from under the install",
            "tags": ["gents:pack:test_pack"],
        });
        let edit_plan = crate::config_client::DesiredStateApplyPlan::new(vec![
            crate::config_client::DesiredStateApplyDocument {
                collection: Collection::Tools,
                add: edited.clone(),
                update: edited,
            },
        ])?;
        access
            .transact("test.pack.concurrent_edit", |txn| {
                let edit_plan = &edit_plan;
                Box::pin(async move {
                    crate::config_client::apply_desired_state_plan(txn, edit_plan).await
                })
            })
            .await?;

        let error = apply_pack_documents(&access, &config, expected)
            .await
            .unwrap_err();
        let stale = crate::config_client::stale_expectation(&error)
            .unwrap_or_else(|| panic!("expected StaleExpectation, got: {error:#}"));
        assert_eq!(stale.drifted.len(), 1);
        assert_eq!(stale.drifted[0].id, "shared-tools");

        node.shutdown().await;
        Ok(())
    }

    /// A document-pack install is the guarded publication `ApplyReconcile.publishIf`
    /// models, so its verdict and post-state are taken from that model's
    /// executable cases rather than restated here. The install publishes the
    /// case's whole candidate and guards every member of it, which is the
    /// scope [`replaced_document_expectations`] emits, so only a case whose
    /// expectation scope already is its candidate's support is the same
    /// execution natively; `packScopedScenarios` exists to emit that shape.
    /// A case at any narrower scope is out of range here and stays with the
    /// desired-state consumer of the same cases, because projecting its
    /// candidate down to the scope would run inputs the emitted verdict was
    /// not computed for. Each Lean `content` maps onto the `display_name` of
    /// a `Tools` document keyed by the Lean id: the Lean collection is
    /// ignored because the scenario's ids are distinct across collections,
    /// and Lean `refs` are not materialized because reference closure is a
    /// separate gate with its own cases.
    #[tokio::test]
    async fn pack_install_matches_lean_publish_if_cases() -> Result<()> {
        use crate::lean_vocab_test::lean_publish_if_cases;
        use defra_node::EmbeddedNode;
        use serde_json::json;
        use std::collections::{BTreeMap, BTreeSet};
        use std::sync::Arc;

        const OWNER: &str = "did:key:pack-owner";

        fn tools_document(id: &str, content: &str) -> serde_json::Value {
            json!({
                "agent_did": OWNER,
                "tools_id": id,
                "display_name": content,
                "tags": ["gents:pack:test_pack"],
            })
        }

        async fn write(access: &ConfigAccess, documents: Vec<serde_json::Value>) -> Result<()> {
            if documents.is_empty() {
                return Ok(());
            }
            let plan = crate::config_client::DesiredStateApplyPlan::new(
                documents
                    .into_iter()
                    .map(|value| crate::config_client::DesiredStateApplyDocument {
                        collection: Collection::Tools,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )?;
            access
                .transact("test.pack.lean.write", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        crate::config_client::apply_desired_state_plan(txn, plan).await
                    })
                })
                .await?;
            Ok(())
        }

        async fn live_content(access: &ConfigAccess, id: &str) -> Result<Option<String>> {
            let id = id.to_owned();
            access
                .transact("test.pack.lean.read", |txn| {
                    let id = id.clone();
                    Box::pin(async move {
                        Ok(crate::config_client::read_desired_state_document_in_txn(
                            txn,
                            Collection::Tools,
                            OWNER,
                            &id,
                        )
                        .await?
                        .and_then(|live| live["display_name"].as_str().map(ToOwned::to_owned)))
                    })
                })
                .await
        }

        let mut exercised = Vec::new();
        let mut out_of_range = Vec::new();
        for case in lean_publish_if_cases() {
            let support = |target: &crate::lean_vocab_test::LeanApplyDocRef| {
                (
                    target.collection.clone(),
                    target.id.clone(),
                    target.agent_did.clone(),
                )
            };
            if case
                .expected
                .iter()
                .map(|row| support(&row.target))
                .collect::<BTreeSet<_>>()
                != case
                    .candidate
                    .iter()
                    .map(|row| support(&row.target))
                    .collect::<BTreeSet<_>>()
            {
                out_of_range.push(case.name.as_str());
                continue;
            }

            let node = Arc::new(EmbeddedNode::builder().build().await?);
            crate::ensure_runtime_schemas(&node).await?;
            crate::document_config::ensure_agent_principal(&node, OWNER).await?;
            let access = ConfigAccess::Local(node.clone());

            write(
                &access,
                case.expected
                    .iter()
                    .filter_map(|row| {
                        row.content
                            .as_deref()
                            .map(|content| tools_document(&row.target.id, content))
                    })
                    .collect(),
            )
            .await?;

            let config: PackConfig = serde_json::from_value(json!({
                "agent_principal": {"agent_did": OWNER},
                "tools": case
                    .candidate
                    .iter()
                    .map(|row| tools_document(&row.target.id, &row.content))
                    .collect::<Vec<_>>(),
            }))?;

            let expected = replaced_document_expectations(&access, &config).await?;
            assert_eq!(
                expected
                    .iter()
                    .map(|row| (row.id.as_str(), row.digest.is_some()))
                    .collect::<BTreeMap<_, _>>(),
                case.expected
                    .iter()
                    .map(|row| (row.target.id.as_str(), row.content.is_some()))
                    .collect::<BTreeMap<_, _>>(),
                "case {} capture",
                case.name
            );

            write(
                &access,
                case.pre_desired
                    .iter()
                    .map(|row| tools_document(&row.target.id, &row.content))
                    .collect(),
            )
            .await?;

            let outcome = apply_pack_documents(&access, &config, expected).await;
            assert_eq!(
                outcome.is_ok(),
                case.applied,
                "case {}: {outcome:?}",
                case.name
            );
            if let Err(error) = &outcome {
                assert!(
                    crate::config_client::stale_expectation(error).is_some(),
                    "case {}: {error:#}",
                    case.name
                );
            }

            let keys = case
                .expected
                .iter()
                .map(|row| row.target.id.as_str())
                .chain(case.pre_desired.iter().map(|row| row.target.id.as_str()))
                .chain(case.candidate.iter().map(|row| row.target.id.as_str()))
                .chain(
                    case.expected_after_desired
                        .iter()
                        .map(|row| row.target.id.as_str()),
                )
                .collect::<BTreeSet<_>>();
            for id in keys {
                let want = case
                    .expected_after_desired
                    .iter()
                    .find(|row| row.target.id == id)
                    .map(|row| row.content.clone());
                assert_eq!(
                    live_content(&access, id).await?,
                    want,
                    "case {} document {id}",
                    case.name
                );
            }

            node.shutdown().await;
            exercised.push(case.name.as_str());
        }
        assert_eq!(
            exercised,
            [
                "all_expectations_match",
                "closure_document_drifted",
                "pack_scope_all_match",
                "pack_scope_absent_member_present",
                "pack_scope_member_drifted",
            ]
        );
        assert_eq!(
            out_of_range,
            [
                "target_drifted",
                "expected_absent_but_present",
                "expected_absent_and_absent",
                "expected_present_but_absent",
                "empty_scope_is_publish",
            ]
        );
        Ok(())
    }
}
