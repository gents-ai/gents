use gents_inference::backends::provider::BackendProviderKind;
use gents_loop::openai_wire::OpenAiWireApi;
use gents_store::collection::Collection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::desired_state;

use gents_service::args::ToolCeilingArg;
use gents_service::args::ToolPackageArg;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedBackendConfig {
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) openai_wire_api: Option<OpenAiWireApi>,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InitSummary {
    pub(crate) backend_id: String,
    pub(crate) backend_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
    pub(crate) model_name: String,
    pub(crate) max_concurrent: i64,
    pub(crate) max_queue_depth: i64,
    pub(crate) default_agent_did: String,
    pub(crate) tools_id: String,
    pub(crate) wide_open_preset_id: String,
    pub(crate) inference_profile_id: String,
    pub(crate) tool_package: ToolPackageArg,
    pub(crate) tool_ceiling: ToolCeilingArg,
    pub(crate) tool_root: Option<String>,
    pub(crate) enable_defra_query: bool,
    pub(crate) defra_query_collections: Vec<String>,
    pub(crate) created_node: bool,
    pub(crate) created_default_agent: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct P2pReplicatorRow {
    #[serde(rename = "ID", default)]
    pub(crate) id: Option<String>,
    #[serde(rename = "Addresses", default)]
    pub(crate) addresses: Vec<String>,
    #[serde(rename = "CollectionIDs", default)]
    pub(crate) collection_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct P2pReplicatorOutputRow {
    pub(crate) id: Option<String>,
    pub(crate) addresses: Vec<String>,
    pub(crate) collection_ids: Vec<String>,
    pub(crate) collection_names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct P2pReplicatorRequest {
    #[serde(rename = "Collections")]
    pub(crate) collections: Vec<String>,
    #[serde(rename = "Addresses")]
    pub(crate) addresses: Vec<String>,
    /// Per-collection replication filters. Serialized to defradb's
    /// `ReplicatorRequest.Filters` shape (`{Collection: {Conditions}}`); the
    /// node installs a filtered replicator that pushes only matching documents.
    /// Omitted entirely when empty so an unfiltered replicator is requested.
    #[serde(
        rename = "Filters",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub(crate) filters: std::collections::BTreeMap<String, defra_p2p_adapter::ReplicationFilter>,
}

#[derive(Debug, Serialize)]
pub(crate) struct P2pReplicatorDeleteRequest {
    #[serde(rename = "ID")]
    pub(crate) id: String,
    #[serde(rename = "Collections")]
    pub(crate) collections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigExportBundle {
    pub(crate) format: String,
    pub(crate) node_did: String,
    pub(crate) exported_at: String,
    pub(crate) access_mode: String,
    #[serde(flatten)]
    pub(crate) config: gents_documents::PackConfig,
}

impl ConfigExportBundle {
    pub(crate) fn docs_for_collection(&self, collection: Collection) -> anyhow::Result<Vec<Value>> {
        let config = serde_json::to_value(&self.config)?;
        match collection {
            Collection::Node => Ok(vec![config["node"].clone()]),
            Collection::DefaultAgentSelection => Ok(match config.get("default_agent_selection") {
                None | Some(Value::Null) => Vec::new(),
                Some(selection) => vec![selection.clone()],
            }),
            _ => match collection.dir_name().and_then(|key| config.get(key)) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(rows)) => Ok(rows.clone()),
                Some(_) => {
                    anyhow::bail!("canonical configuration {collection} is not a document array")
                }
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigApplyCounts(std::collections::BTreeMap<Collection, usize>);

impl Serialize for ConfigApplyCounts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(Collection::ALL.len()))?;
        for collection in Collection::ALL {
            map.serialize_entry(&collection.to_string(), &self.get(collection))?;
        }
        map.end()
    }
}

impl ConfigApplyCounts {
    pub(crate) fn from_config(config: &gents_documents::PackConfig) -> anyhow::Result<Self> {
        let value = serde_json::to_value(config)?;
        let mut counts = Self::default();
        for collection in Collection::ALL {
            let count = match collection {
                Collection::Node => 1,
                Collection::DefaultAgentSelection => {
                    usize::from(value.get("default_agent_selection").is_some())
                }
                _ => collection
                    .dir_name()
                    .and_then(|key| value.get(key))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            };
            counts.set(collection, count);
        }
        Ok(counts)
    }

    pub(crate) fn get(&self, collection: Collection) -> usize {
        self.0.get(&collection).copied().unwrap_or_default()
    }

    pub(crate) fn set(&mut self, collection: Collection, count: usize) {
        self.0.insert(collection, count);
    }

    pub(crate) fn add(&mut self, collection: Collection, count: usize) {
        *self.0.entry(collection).or_default() += count;
    }

    pub(crate) fn saturating_sub(&self, other: &Self) -> Self {
        let mut counts = Self::default();
        for collection in Collection::ALL {
            counts.set(
                collection,
                self.get(collection).saturating_sub(other.get(collection)),
            );
        }
        counts
    }

    pub(crate) fn changed(&self) -> bool {
        Collection::ALL
            .iter()
            .copied()
            .any(|collection| self.get(collection) > 0)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConfigApplyReport {
    pub(crate) status: &'static str,
    pub(crate) ok: bool,
    pub(crate) exact_match: bool,
    pub(crate) changed: bool,
    pub(crate) root: String,
    pub(crate) access_mode: String,
    pub(crate) node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schemas: Option<crate::commands::schema::PackSchemaPhase>,
    pub(crate) planned: desired_state::DesiredStateDiffCollectionsCounts,
    pub(crate) applied: ConfigApplyCounts,
    pub(crate) pruned: ConfigApplyCounts,
    pub(crate) remaining: desired_state::DesiredStateDiffCollectionsCounts,
}

pub(crate) async fn load_backend_observation(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> anyhow::Result<gents_documents::InferenceBackendObservation> {
    let query = format!(
        "{{ InferenceBackendObservation(filter: {{node_did: {{_eq: \"{}\"}}, backend_id: {{_eq: \"{}\"}}}}, limit: 2) {{backend_id catalogs catalog_freshness probe_status last_probe}} }}",
        gents_store::graphql::escape_graphql_string(node_did),
        gents_store::graphql::escape_graphql_string(backend_id),
    );
    let rows = crate::graphql_rows(access, "InferenceBackendObservation", &query).await?;
    anyhow::ensure!(
        rows.len() == 1,
        "backend observation is missing or ambiguous"
    );
    gents_inference::backends::registry::decode_backend_observation(rows[0].clone())
}

#[cfg(test)]
pub(crate) mod test_support;

/// The serde spelling of a unit-like wire value, as its owner serializes it.
pub(crate) fn wire<T>(value: &T) -> String
where
    T: Serialize,
{
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}
