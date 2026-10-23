use anyhow::{Context, Result};
use gents::config_client::patch::{SelfConfigTarget, ALL_SELF_CONFIG_TARGETS};
use gents::graphql::escape_graphql_string;
use gents::ConfigAccess;
use gents_desktop_core::client::ClientCore;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEditorEntry {
    pub collection: String,
    pub id: String,
    pub label: String,
}

pub(super) fn target(collection: &str) -> Result<SelfConfigTarget> {
    SelfConfigTarget::from_collection_name(collection)
        .context("This collection is not editable in Runtime Settings.")
}

pub(super) async fn catalog(core: &ClientCore, node_did: &str) -> Result<Vec<ConfigEditorEntry>> {
    let access = core
        .operator_graphql(node_did)
        .map(ConfigAccess::Graphql)
        .context("Connect this local runtime before editing its configuration.")?;
    let owner = escape_graphql_string(node_did);
    let queries: Vec<_> = ALL_SELF_CONFIG_TARGETS
        .iter()
        .map(|target| {
            let name = ["display_name", "name"]
                .into_iter()
                .find(|field| target.all_fields().contains(field));
            format!(
                r#"{}(filter: {{node_did: {{_eq: "{owner}"}}}}) {{ {} {} }}"#,
                target.collection_name(),
                target.unique_field(),
                name.unwrap_or("")
            )
        })
        .collect();
    let response = access
        .execute(&format!("{{ {} }}", queries.join("\n")))
        .await?;
    let mut entries = Vec::new();
    for target in ALL_SELF_CONFIG_TARGETS {
        let rows = response["data"][target.collection_name()]
            .as_array()
            .context("Runtime Settings returned an incomplete catalog.")?;
        let mut identities = rapidhash::RapidHashSet::default();
        for row in rows {
            let id = row[target.unique_field()]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .context("Runtime Settings returned a configuration without an identity.")?;
            anyhow::ensure!(
                identities.insert(id),
                "Runtime Settings returned duplicate configuration identities."
            );
            let label = row["display_name"]
                .as_str()
                .or_else(|| row["name"].as_str())
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(id);
            entries.push(ConfigEditorEntry {
                collection: target.collection_name().to_owned(),
                id: id.to_owned(),
                label: label.to_owned(),
            });
        }
    }
    entries.sort_by(|left, right| {
        left.collection
            .cmp(&right.collection)
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(entries)
}
