use crate::config_writes::ConfigAccess;
use crate::{CONFIG_EXPORT_FORMAT, desired_state, shared::ConfigExportBundle};
use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::{Collection, config_client::config_projection};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(crate) async fn build_config_export_bundle(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<ConfigExportBundle> {
    read_owned_config_bundle(access, agent_did)
        .await?
        .context("configuration owner has no AgentPrincipal")
}

pub(crate) async fn build_desired_state_live_bundle(
    access: &ConfigAccess,
    desired: &desired_state::DesiredStateManifest,
) -> Result<Option<ConfigExportBundle>> {
    read_owned_config_bundle(access, &desired.agent_principal.agent_did).await
}

async fn read_owned_config_bundle(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<Option<ConfigExportBundle>> {
    anyhow::ensure!(!agent_did.trim().is_empty(), "configuration owner is blank");
    let owner = agent_did.to_owned();
    let access_mode = access.mode().to_owned();
    access
        .transact("export_owned_configuration", |txn| {
            let owner = owner.clone();
            let access_mode = access_mode.clone();
            Box::pin(async move {
                let mut config = Map::new();
                let mut has_documents = false;
                for collection in Collection::ALL {
                    let name = collection.graphql_type();
                    let (fields, _) = config_projection(collection, None)?;
                    let response = txn
                        .execute(&format!(
                            "{{{name}(filter:{{agent_did:{{_eq:\"{}\"}}}}) {{{}}}}}",
                            escape_graphql_string(&owner),
                            fields.join(" ")
                        ))
                        .await?;
                    let rows = response
                        .get("data")
                        .and_then(|data| data.get(name))
                        .and_then(Value::as_array)
                        .with_context(|| format!("configuration query returned no {name} rows"))?;
                    let mut normalized = Vec::with_capacity(rows.len());
                    let mut identities = BTreeSet::new();
                    for row in rows {
                        let (_, document) = config_projection(collection, Some(row))?;
                        let document = document.context("canonical config projection missing")?;
                        anyhow::ensure!(
                            document.get("agent_did").and_then(Value::as_str)
                                == Some(owner.as_str()),
                            "{name} query returned a foreign owner"
                        );
                        let id = document
                            .get(collection.unique_field())
                            .and_then(Value::as_str)
                            .context("configuration logical ID missing")?;
                        anyhow::ensure!(
                            identities.insert(id.to_owned()),
                            "ambiguous {name} logical ID {id:?} within owner"
                        );
                        normalized.push(document);
                    }
                    sort_document_rows(&mut normalized, collection.unique_field());
                    has_documents |= !normalized.is_empty();
                    if let Some(key) = collection.dir_name() {
                        if !normalized.is_empty() {
                            config.insert(key.to_owned(), Value::Array(normalized));
                        }
                    } else {
                        anyhow::ensure!(normalized.len() <= 1, "ambiguous configuration principal");
                        if let Some(principal) = normalized.pop() {
                            config.insert("agent_principal".into(), principal);
                        }
                    }
                }
                if !config.contains_key("agent_principal") {
                    anyhow::ensure!(
                        !has_documents,
                        "owned configuration exists without its AgentPrincipal"
                    );
                    return Ok(None);
                }
                let config = serde_json::from_value(Value::Object(config))
                    .context("decode canonical configuration snapshot")?;
                Ok(Some(ConfigExportBundle {
                    format: CONFIG_EXPORT_FORMAT.into(),
                    agent_did: owner,
                    exported_at: chrono::Utc::now().to_rfc3339(),
                    access_mode,
                    config,
                }))
            })
        })
        .await
}

pub(crate) fn live_manifest_from_bundle(
    desired: &desired_state::DesiredStateManifest,
    live: &Option<ConfigExportBundle>,
) -> Result<(
    Option<desired_state::DesiredAgentPrincipal>,
    desired_state::DesiredStateManifest,
)> {
    if let Some(bundle) = live {
        let manifest = desired_state::manifest_from_export_bundle(bundle)?;
        Ok((Some(manifest.agent_principal.clone()), manifest))
    } else {
        let empty =
            serde_json::from_value(serde_json::json!({"agent_principal":desired.agent_principal}))?;
        Ok((None, empty))
    }
}

pub(crate) fn sort_document_rows(rows: &mut [Value], key: &str) {
    rows.sort_by(|left, right| {
        let left_key = left.get(key).and_then(Value::as_str).unwrap_or_default();
        let right_key = right.get(key).and_then(Value::as_str).unwrap_or_default();
        left_key.cmp(right_key)
    });
}

pub(crate) fn collect_string_field_values(rows: &[Value], field: &str) -> Vec<String> {
    let mut values = rows
        .iter()
        .filter_map(|row| row.get(field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

pub(crate) fn select_apply_collection_docs(
    docs: &[Value],
    unique_field: &str,
    collection_name: &str,
    diff: &desired_state::DesiredStateCollectionDiff,
) -> Result<Vec<Value>> {
    let requested_ids = diff
        .create
        .iter()
        .chain(diff.update.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if requested_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut selected = docs
        .iter()
        .filter(|doc| {
            doc.get(unique_field)
                .and_then(Value::as_str)
                .is_some_and(|value| requested_ids.contains(value))
        })
        .cloned()
        .collect::<Vec<_>>();
    sort_document_rows(&mut selected, unique_field);

    let found_ids = selected
        .iter()
        .filter_map(|doc| doc.get(unique_field).and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let missing_ids = requested_ids
        .difference(&found_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_ids.is_empty() {
        anyhow::bail!(
            "desired-state apply missing {collection_name} documents for ids: {}",
            missing_ids.join(", ")
        );
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::{DesiredStateApplyPlan, apply_desired_state_plan};
    use serde_json::json;
    use std::sync::Arc;

    fn bundle(owner: &str) -> ConfigExportBundle {
        ConfigExportBundle {
            format: CONFIG_EXPORT_FORMAT.into(), agent_did:owner.into(), exported_at:"2026-01-01T00:00:00Z".into(), access_mode:"test".into(),
            config:serde_json::from_value(json!({"agent_principal":{"agent_did":owner},"contexts":[{"context_id":"unused-context","agent_did":owner,"tools_id":"unused"}],"inference_backends":[{"backend_id":"unused-backend","agent_did":owner,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}}],"inference_profiles":[{"profile_id":"unused-profile","agent_did":owner,"backend_id":"unused-backend","model_name":"selected-model"}],"tools":[{"agent_did":owner,"tools_id":"unused","host":{"bash":{"allowed_argv_prefixes":[],"forbidden_argv_prefixes":[]}}}]})).unwrap(),
        }
    }

    #[test]
    fn bundle_roundtrips_canonical_nested_config_without_another_schema() {
        let original = bundle("owner");
        let encoded = serde_json::to_value(&original).unwrap();
        assert!(encoded.get("config").is_none());
        assert_eq!(
            encoded["tools"][0]["host"]["bash"]["allowed_argv_prefixes"],
            json!([])
        );
        let decoded: ConfigExportBundle = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded.config).unwrap(),
            serde_json::to_value(original.config).unwrap()
        );
        let mut unknown = encoded;
        unknown["tool_selections"] = json!([]);
        assert!(serde_json::from_value::<ConfigExportBundle>(unknown).is_err());
    }

    #[tokio::test]
    async fn export_includes_unreferenced_config_in_exact_owner_scope_and_preserves_json() {
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        for owner in ["owner-a", "owner-b"] {
            let source = bundle(owner);
            let plan = DesiredStateApplyPlan::from_pack_config(&source.config).unwrap();
            access
                .transact("seed_export_scope", |txn| {
                    let plan = plan.clone();
                    Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
                })
                .await
                .unwrap();
        }
        let exported = build_config_export_bundle(&access, "owner-a")
            .await
            .unwrap();
        assert_eq!(
            exported.config.tools.len(),
            1,
            "unreferenced tools must be exported"
        );
        assert_eq!(exported.config.tools[0].agent_did, "owner-a");
        let counts = crate::shared::ConfigApplyCounts::from_config(&exported.config).unwrap();
        assert_eq!(counts.get(Collection::Tools), 1);
        assert_eq!(counts.get(Collection::AgentContext), 1);
        assert_eq!(counts.get(Collection::InferenceProfile), 1);
        let count_json = serde_json::to_value(counts).unwrap();
        assert_eq!(count_json.as_object().unwrap().len(), Collection::ALL.len());
        assert!(count_json.get("tool_selections").is_none());
        assert!(count_json.get("event_triggers").is_none());

        let encoded = serde_json::to_value(&exported).unwrap();
        assert_eq!(
            encoded["tools"][0]["host"]["bash"]["allowed_argv_prefixes"],
            json!([])
        );
        assert_eq!(
            encoded["tools"][0]["host"]["bash"]["forbidden_argv_prefixes"],
            json!([])
        );
        let mut changed = exported.config.clone();
        changed.tools[0]
            .host
            .as_mut()
            .unwrap()
            .bash
            .as_mut()
            .unwrap()
            .allowed_argv_prefixes = Some(vec![vec!["printf".into(), "".into()]]);
        let plan = DesiredStateApplyPlan::from_pack_config(&changed).unwrap();
        access
            .transact("update_export_json", |txn| {
                let plan = plan.clone();
                Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        let after = build_config_export_bundle(&access, "owner-a")
            .await
            .unwrap();
        assert_eq!(after.config.tools[0].host, changed.tools[0].host);
        let foreign = build_config_export_bundle(&access, "owner-b")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(foreign.config.tools[0].host.as_ref().unwrap()).unwrap()["bash"]["allowed_argv_prefixes"],
            json!([])
        );
        assert!(
            read_owned_config_bundle(&access, "absent")
                .await
                .unwrap()
                .is_none()
        );
        let raw = node
            .execute(
                r#"mutation {create_Tools(input:{agent_did:"orphan",tools_id:"unused"}){_docID}}"#,
            )
            .await;
        assert!(!raw.has_errors(), "{:?}", raw.errors);
        assert!(read_owned_config_bundle(&access, "orphan").await.is_err());
        node.shutdown().await;
    }
}
