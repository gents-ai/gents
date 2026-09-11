use super::{
    DesiredAgentPrincipal, DesiredStateCollectionDiff, DesiredStateDiffCollections,
    DesiredStateDiffReport, DesiredStateManifest,
};
use anyhow::{Context, Result};
use gents::{config_client::DesiredStateApplyPlan, Collection};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) fn diff_manifests(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
    prune: bool,
) -> DesiredStateDiffReport {
    let result = compare_manifests(desired, live_principal, live, prune);
    let (collections, errors) = match result {
        Ok(collections) => (collections, Vec::new()),
        Err(error) => (
            DesiredStateDiffCollections::default(),
            vec![error.to_string()],
        ),
    };
    let counts = collections.counts();
    DesiredStateDiffReport {
        status: if errors.is_empty() {
            "diffed"
        } else {
            "invalid"
        },
        ok: errors.is_empty() && counts.is_exact_match(),
        root: root.display().to_string(),
        access_mode: access_mode.into(),
        agent_did: desired.agent_principal.agent_did.clone(),
        live_validation_errors: errors,
        counts,
        collections,
    }
}

pub(super) fn canonical_records(
    config: &DesiredStateManifest,
) -> Result<BTreeMap<(Collection, String), Value>> {
    DesiredStateApplyPlan::from_pack_config(config)?
        .documents()
        .iter()
        .map(|document| {
            anyhow::ensure!(
                document.update["agent_did"].as_str()
                    == Some(config.agent_principal.agent_did.as_str()),
                "configuration document belongs to another principal"
            );
            let id = document.update[document.collection.unique_field()]
                .as_str()
                .context("canonical document is missing its logical ID")?
                .to_owned();
            Ok(((document.collection, id), document.update.clone()))
        })
        .collect()
}

fn compare_manifests(
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
    prune: bool,
) -> Result<DesiredStateDiffCollections> {
    anyhow::ensure!(
        desired.agent_principal.agent_did == live.agent_principal.agent_did,
        "desired and live config belong to different principals"
    );
    if let Some(principal) = live_principal {
        anyhow::ensure!(
            principal.agent_did == live.agent_principal.agent_did,
            "live principal does not match config scope"
        );
    }
    let desired_docs = canonical_records(desired)?;
    let mut live_docs = canonical_records(live)?;
    if live_principal.is_none() {
        live_docs.retain(|(collection, _), _| *collection != Collection::AgentPrincipal);
    }
    let mut collections = DesiredStateDiffCollections::default();
    for collection in Collection::ALL {
        let mut desired_rows = Vec::new();
        for ((kind, id), value) in &desired_docs {
            if *kind != collection {
                continue;
            }
            let mut effective = value.clone();
            if collection == Collection::ChainKeyBinding {
                if let Some(retained) = live_docs.get(&(collection, id.clone())) {
                    gents::document_config::preserve_chain_key_binding_update_fields(
                        &mut effective,
                    )?;
                    let mut merged = retained
                        .as_object()
                        .context("retained binding is not an object")?
                        .clone();
                    merged.extend(
                        effective
                            .as_object()
                            .context("binding replacement is not an object")?
                            .clone(),
                    );
                    effective = Value::Object(merged);
                }
            }
            desired_rows.push((id.clone(), effective));
        }
        let live_rows = live_docs
            .iter()
            .filter(|((kind, _), _)| *kind == collection)
            .map(|((_, id), value)| (id.clone(), value))
            .collect();
        *collections.get_mut(collection) = diff_collection(
            desired_rows
                .iter()
                .map(|(id, value)| (id.clone(), value))
                .collect(),
            live_rows,
        );
    }
    if prune {
        collections.record_prune_deletes(&super::prune::prune_safe_deletes(desired, live)?);
    }
    Ok(collections)
}

pub(crate) fn diff_collection<T>(
    desired: Vec<(String, &T)>,
    live: Vec<(String, &T)>,
) -> DesiredStateCollectionDiff
where
    T: PartialEq,
{
    diff_collection_by(desired, live, |desired, live| desired == live)
}

fn diff_collection_by<T, F>(
    desired: Vec<(String, &T)>,
    live: Vec<(String, &T)>,
    matches_live: F,
) -> DesiredStateCollectionDiff
where
    F: Fn(&T, &T) -> bool,
{
    let desired_map = desired
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let live_map = live
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();

    let all_ids = desired_map
        .keys()
        .chain(live_map.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in all_ids {
        match (desired_map.get(&id), live_map.get(&id)) {
            (Some(desired), Some(live)) => {
                if matches_live(desired, live) {
                    unchanged.push(id);
                } else {
                    update.push(id);
                }
            }
            (Some(_), None) => create.push(id),
            (None, Some(_)) => live_only.push(id),
            (None, None) => {}
        }
    }

    DesiredStateCollectionDiff {
        create,
        update,
        delete: Vec::new(),
        unchanged,
        live_only,
    }
}

#[cfg(test)]
mod canonical_tests {
    use super::*;
    use serde_json::json;

    fn fixture(prompt: &str) -> DesiredStateManifest {
        serde_json::from_value(json!({
            "agent_principal": {"agent_did": "did:key:owner"},
            "contexts": [{"agent_did": "did:key:owner", "context_id": "context", "system_prompt": prompt, "tools_id": "kept"}],
            "tools": [{"agent_did": "did:key:owner", "tools_id": "kept"},
                      {"agent_did": "did:key:owner", "tools_id": "unused"}]
        })).unwrap()
    }

    #[test]
    fn canonical_diff_preserves_literals_and_covers_all_collections() {
        let live = fixture("prompt\n");
        let desired = fixture("prompt");
        let report = diff_manifests(
            Path::new("."),
            "local",
            &desired,
            Some(&live.agent_principal),
            &live,
            false,
        );
        assert!(report.live_validation_errors.is_empty());
        assert_eq!(
            report.collections.get(Collection::AgentContext).update,
            ["context"]
        );
        assert_eq!(report.counts.get(Collection::Tools).unchanged, 2);
        let counts = serde_json::to_value(report.counts).unwrap();
        assert_eq!(counts.as_object().unwrap().len(), Collection::ALL.len());
        assert_eq!(counts["tools"]["unchanged"], 2);
    }

    #[test]
    fn prune_uses_retained_canonical_references() {
        let live = fixture("prompt");
        let mut desired = live.clone();
        desired.tools.clear();
        let deletes = super::super::prune::prune_safe_deletes(&desired, &live).unwrap();
        assert_eq!(
            deletes,
            vec![crate::desired_state::DocRef {
                collection: Collection::Tools,
                id: "unused".into()
            }]
        );
        let mut foreign = desired;
        foreign.agent_principal.agent_did = "did:key:foreign".into();
        assert!(super::super::prune::prune_safe_deletes(&foreign, &live).is_err());
    }
}
