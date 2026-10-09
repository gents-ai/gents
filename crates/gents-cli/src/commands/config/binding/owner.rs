use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents::{document_config::PackConfig, Collection};
use serde_json::Value;

fn document_roots() -> impl Iterator<Item = &'static str> {
    Collection::ALL
        .into_iter()
        .filter_map(|collection| collection.dir_name())
        .chain(["graph_intents", "graph_capabilities"])
}

pub(super) fn manifest_node_dids(config: &PackConfig) -> Result<BTreeSet<String>> {
    let value = serde_json::to_value(config)?;
    let mut owners = BTreeSet::from([config.node.node_did.clone()]);
    for root in document_roots() {
        for row in value[root].as_array().into_iter().flatten() {
            owners.insert(
                row["node_did"]
                    .as_str()
                    .context("config owner missing")?
                    .to_owned(),
            );
        }
    }
    owners.retain(|owner| !owner.trim().is_empty());
    Ok(owners)
}

pub(super) fn enforce_manifest_rebind_safety(
    manifest: &PackConfig,
    target_did: &str,
    force_rebind_concrete_did: bool,
) -> Result<()> {
    let concrete_mismatches = manifest_node_dids(manifest)?
        .into_iter()
        .filter(|did| did != target_did)
        .collect::<Vec<_>>();
    if !concrete_mismatches.is_empty() && !force_rebind_concrete_did {
        anyhow::bail!(
            "manifest contains concrete node DID(s) that do not match resolved runtime DID {target_did}: {}; pass --force-rebind-concrete-did to rebind them",
            concrete_mismatches.join(", ")
        );
    }
    Ok(())
}

/// Rebind authored document owners, preserving explicit foreign destinations and
/// all nested host data. This is an operator-requested config rewrite, not a
/// recursive replacement of DIDs or a change to graph caller permissions.
pub(super) fn rebind_manifest_node_did(config: &mut PackConfig, target: &str) -> Result<()> {
    anyhow::ensure!(!target.trim().is_empty(), "target owner must not be blank");
    let source_owners = manifest_node_dids(config)?;
    let mut value = serde_json::to_value(&*config)?;
    value["node"]["node_did"] = Value::String(target.to_owned());
    for root in document_roots() {
        for row in value[root].as_array_mut().into_iter().flatten() {
            row["node_did"] = Value::String(target.to_owned());
        }
    }
    let mut rebound: PackConfig = serde_json::from_value(value)?;
    for entry in &mut rebound.agent_targets {
        if source_owners.contains(&entry.target_node_did) {
            entry.target_node_did = target.to_owned();
        }
    }
    *config = rebound;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rebind_canonical_owners_preserves_foreign_destinations_literals_and_order() {
        let mut config: PackConfig = serde_json::from_value(json!({
            "node": {"node_did": "did:test:source"},
            "contexts": [{"context_id": "ctx", "node_did": "did:test:source",
                "system_prompt": "  did:test:source\n", "skill_ids": ["b", "a"]}],
            "tools": [{"tools_id": "tools", "node_did": "did:test:source"}],
            "agent_targets": [
                {"target_id": "local", "node_did": "did:test:source",
                 "target_node_did": "did:test:source", "agent_id": "b", "name": "local"},
                {"target_id": "remote", "node_did": "did:test:source",
                 "target_node_did": "did:test:remote", "agent_id": "b", "name": "remote"}
            ],
            "inference_profiles": [{"profile_id": "p", "node_did": "did:test:source",
                "backend_id": "backend", "model_name": "model"}],
            "graph_capabilities": [{"capability_id": "cap", "revision": "v1",
                "node_did": "did:test:source",
                "target": {"kind": "task", "task_id": "task"},
                "allowed_callers": ["did:test:source", "did:test:remote"]}]
        }))
        .unwrap();
        assert!(enforce_manifest_rebind_safety(&config, "did:test:target", false).is_err());
        enforce_manifest_rebind_safety(&config, "did:test:target", true).unwrap();
        enforce_manifest_rebind_safety(&config, "did:test:source", false).unwrap();
        rebind_manifest_node_did(&mut config, "did:test:target").unwrap();
        assert_eq!(
            manifest_node_dids(&config).unwrap(),
            BTreeSet::from(["did:test:target".into()])
        );
        assert_eq!(config.agent_targets[0].target_node_did, "did:test:target");
        assert_eq!(config.agent_targets[1].target_node_did, "did:test:remote");
        assert_eq!(
            config.contexts[0].system_prompt.as_deref(),
            Some("  did:test:source\n")
        );
        assert_eq!(config.contexts[0].skill_ids, ["b", "a"]);
        assert_eq!(
            config.graph_capabilities[0].allowed_callers,
            ["did:test:source", "did:test:remote"]
        );
        let once = serde_json::to_value(&config).unwrap();
        rebind_manifest_node_did(&mut config, "did:test:target").unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), once);
        assert!(rebind_manifest_node_did(&mut config, " ").is_err());
        assert_eq!(serde_json::to_value(&config).unwrap(), once);
    }
}
