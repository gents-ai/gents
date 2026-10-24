use gents::config_client::patch::{SelfConfigTarget, ALL_SELF_CONFIG_TARGETS};
use gents::config_client::ConfigClientError;
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

#[derive(Debug, thiserror::Error)]
pub(super) enum SettingsError {
    #[error("This collection is not editable in Runtime Settings.")]
    NotEditable,
    #[error("Connect this local runtime before editing its configuration.")]
    NotConnected,
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error("Runtime Settings returned an incomplete catalog.")]
    IncompleteCatalog,
    #[error("Runtime Settings returned a configuration without an identity.")]
    MissingIdentity,
    #[error("Runtime Settings returned duplicate configuration identities.")]
    DuplicateIdentity,
}

type Result<T, E = SettingsError> = std::result::Result<T, E>;

pub(super) fn target(collection: &str) -> Result<SelfConfigTarget> {
    SelfConfigTarget::from_collection_name(collection).ok_or(SettingsError::NotEditable)
}

pub(super) async fn catalog(core: &ClientCore, node_did: &str) -> Result<Vec<ConfigEditorEntry>> {
    let access = core
        .operator_graphql(node_did)
        .map(ConfigAccess::Graphql)
        .ok_or(SettingsError::NotConnected)?;
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
            .ok_or(SettingsError::IncompleteCatalog)?;
        let mut identities = rapidhash::RapidHashSet::default();
        for row in rows {
            let id = row[target.unique_field()]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or(SettingsError::MissingIdentity)?;
            if !identities.insert(id) {
                return Err(SettingsError::DuplicateIdentity);
            }
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
