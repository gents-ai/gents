use gents::{BackendProviderKind, Collection, OpenAiWireApi};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::desired_state;

use crate::cli::args::{ToolCeilingArg, ToolPackageArg};

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
    pub(crate) default_behavior_id: String,
    pub(crate) tools_id: String,
    pub(crate) wide_open_preset_id: String,
    pub(crate) inference_profile_id: String,
    pub(crate) tool_package: ToolPackageArg,
    pub(crate) tool_ceiling: ToolCeilingArg,
    pub(crate) tool_root: Option<String>,
    pub(crate) enable_memory: bool,
    pub(crate) enable_defra_query: bool,
    pub(crate) defra_query_collections: Vec<String>,
    pub(crate) created_principal: bool,
    pub(crate) created_default_behavior: bool,
}

/// This home's persisted `init.json`, in gents-cli's own CLI-facing
/// tool-policy vocabulary. The struct itself now lives in `gents::home`
/// (moved so `gc-cell`, which provisions gents homes from outside this
/// binary, writes the exact same shape rather than a second, drifting
/// copy); this alias keeps every existing field access in this crate
/// (`config.tool_ceiling`, `StoredInitConfig { .. }` literals, and so on)
/// unchanged.
pub(crate) type StoredInitConfig = gents::home::StoredInitConfig<ToolPackageArg, ToolCeilingArg>;

/// Operator-visible P2P admission bounds in effect for this server process.
///
/// The Codex shim's live binding, as `/healthz` sees it (#699).
///
/// The shim used to be invisible to every health surface, so a node could report
/// `ok: true` while its advertised WebSocket port was closed — which is exactly
/// how a fleet-wide bring-up looked healthy while no operator could reach a
/// single agent. The state is shared because the shim may bind *after* the HTTP
/// surface is already serving: the supervisor flips it when a published
/// generation makes the bound behavior runnable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexShimHealth {
    /// `--no-codex-shim`: not an error, and not a thing to report as degraded.
    Off,
    /// Listening on its port.
    Listening {
        websocket: String,
        auth_required: bool,
        bound_agent_did: String,
        bound_behavior_id: String,
    },
    /// Waiting for the control plane to supply the bound behavior. Transient by
    /// construction — the supervisor binds on the generation that carries it.
    Pending {
        bound_behavior_id: String,
        reason: String,
    },
    /// A host resource we cannot get. No generation retracts this.
    Disabled { reason: String },
}

impl CodexShimHealth {
    pub(crate) fn is_degraded(&self) -> bool {
        matches!(self, Self::Pending { .. } | Self::Disabled { .. })
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Off => serde_json::json!({ "status": "off" }),
            Self::Listening {
                websocket,
                auth_required,
                bound_agent_did,
                bound_behavior_id,
            } => serde_json::json!({
                "status": "ok",
                "websocket": websocket,
                "auth_required": auth_required,
                "bound_agent_did": bound_agent_did,
                "bound_behavior_id": bound_behavior_id,
            }),
            Self::Pending {
                bound_behavior_id,
                reason,
            } => serde_json::json!({
                "status": "pending",
                "bound_behavior_id": bound_behavior_id,
                "reason": reason,
            }),
            Self::Disabled { reason } => serde_json::json!({
                "status": "disabled",
                "reason": reason,
            }),
        }
    }
}

pub(crate) type CodexShimHealthHandle = std::sync::Arc<std::sync::RwLock<CodexShimHealth>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct P2pAdmissionState {
    pub(crate) max_pending_dags: usize,
    pub(crate) max_concurrent_push_tasks: usize,
    pub(crate) max_concurrent_dag_fetches: usize,
    pub(crate) rate_limit_burst: u32,
    pub(crate) rate_limit_rate: f64,
}

impl P2pAdmissionState {
    pub(crate) fn to_json(&self) -> Value {
        serde_json::json!({
            "max_pending_dags": self.max_pending_dags,
            "max_concurrent_push_tasks": self.max_concurrent_push_tasks,
            "max_concurrent_dag_fetches": self.max_concurrent_dag_fetches,
            "rate_limit_burst": self.rate_limit_burst,
            "rate_limit_rate": self.rate_limit_rate,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredRuntimeState {
    pub(crate) home: String,
    pub(crate) graphql: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    pub(crate) default_behavior_id: String,
    #[serde(default = "default_p2p_transport")]
    pub(crate) p2p_transport: String,
    #[serde(default)]
    pub(crate) p2p_peer_id: Option<String>,
    #[serde(default)]
    pub(crate) p2p_listen_addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p2p_admission: Option<P2pAdmissionState>,
}

fn default_p2p_transport() -> String {
    "iroh".to_string()
}

#[derive(Debug, Deserialize)]
pub(crate) struct P2pPeerRow {
    pub(crate) id: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct P2pCollectionSubscriptionRow {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
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

#[derive(Debug, Serialize)]
pub(crate) struct P2pSyncDocumentsRequest {
    #[serde(rename = "collectionName")]
    pub(crate) collection_name: String,
    #[serde(rename = "docIDs")]
    pub(crate) doc_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct P2pSyncBranchableRequest {
    #[serde(rename = "collectionID")]
    pub(crate) collection_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct P2pSyncVersionsRequest {
    #[serde(rename = "versionIDs")]
    pub(crate) version_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigExportBundle {
    pub(crate) format: String,
    pub(crate) agent_did: String,
    pub(crate) exported_at: String,
    pub(crate) access_mode: String,
    #[serde(flatten)]
    pub(crate) config: gents::document_config::PackConfig,
}

impl ConfigExportBundle {
    pub(crate) fn docs_for_collection(&self, collection: Collection) -> anyhow::Result<Vec<Value>> {
        let config = serde_json::to_value(&self.config)?;
        match collection.dir_name() {
            None => Ok(vec![config["agent_principal"].clone()]),
            Some(key) => match config.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(rows)) => Ok(rows.clone()),
                Some(_) => anyhow::bail!("canonical configuration {key} is not a document array"),
            },
        }
    }
}

/// Counts follow the canonical collection vocabulary; adding a config document
/// cannot silently omit it from the apply report.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigApplyCounts(std::collections::BTreeMap<Collection, usize>);

impl Serialize for ConfigApplyCounts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(Collection::ALL.len()))?;
        for collection in Collection::ALL {
            let key = collection.dir_name().unwrap_or("agent_principal");
            map.serialize_entry(key, &self.get(collection))?;
        }
        map.end()
    }
}

impl ConfigApplyCounts {
    pub(crate) fn from_config(config: &gents::document_config::PackConfig) -> anyhow::Result<Self> {
        let value = serde_json::to_value(config)?;
        let mut counts = Self::default();
        for collection in Collection::ALL {
            let count = match collection.dir_name() {
                Some(key) => value.get(key).and_then(Value::as_array).map_or(0, Vec::len),
                None => 1,
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
    pub(crate) agent_did: String,
    /// Present when `<root>/schemas/` existed and was applied before config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schemas: Option<crate::commands::schema::PackSchemaPhase>,
    pub(crate) planned: desired_state::DesiredStateDiffCollectionsCounts,
    pub(crate) applied: ConfigApplyCounts,
    pub(crate) pruned: ConfigApplyCounts,
    pub(crate) remaining: desired_state::DesiredStateDiffCollectionsCounts,
}
