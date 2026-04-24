use defra_agent::{BackendProviderKind, Collection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::desired_state;

use crate::cli::args::BackendPresetArg;
use crate::cli::args::ToolCeilingArg;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedBackendConfig {
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredBackendTarget {
    pub(crate) backend_id: Option<String>,
    pub(crate) preset: Option<BackendPresetArg>,
    pub(crate) provider_kind: BackendProviderKind,
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
    pub(crate) tool_selection_id: String,
    pub(crate) tool_ceiling: ToolCeilingArg,
    pub(crate) tool_root: Option<String>,
    pub(crate) created_principal: bool,
    pub(crate) created_default_behavior: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredInitConfig {
    /// Filesystem-only bootstrap context. Runtime configuration lives in DefraDB
    /// documents; these fields let later CLI commands find the local key and
    /// operator tool ceiling without asking for flags on every run.
    pub(crate) home: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    pub(crate) key_path: Option<String>,
    pub(crate) tool_ceiling: ToolCeilingArg,
    pub(crate) tool_root: Option<String>,
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
}

fn default_p2p_transport() -> String {
    // Matches P2pTransportArg::Iroh.as_str()
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
pub(crate) struct ConfigExportBundle {
    pub(crate) format: String,
    pub(crate) agent_did: String,
    pub(crate) exported_at: String,
    pub(crate) access_mode: String,
    pub(crate) agent_principal: Option<Value>,
    #[serde(default)]
    pub(crate) agent_behaviors: Vec<Value>,
    #[serde(default)]
    pub(crate) tool_selections: Vec<Value>,
    #[serde(default)]
    pub(crate) inference_backends: Vec<Value>,
    #[serde(default)]
    pub(crate) inference_profiles: Vec<Value>,
    #[serde(default)]
    pub(crate) tool_service_registries: Vec<Value>,
    #[serde(default)]
    pub(crate) tasks: Vec<Value>,
    #[serde(default)]
    pub(crate) schedules: Vec<Value>,
    #[serde(default)]
    pub(crate) event_triggers: Vec<Value>,
}

impl ConfigExportBundle {
    pub(crate) fn docs_for_collection(&self, collection: Collection) -> Option<&[Value]> {
        match collection {
            Collection::AgentPrincipal => None,
            Collection::AgentBehavior => Some(&self.agent_behaviors),
            Collection::ToolSelection => Some(&self.tool_selections),
            Collection::InferenceBackend => Some(&self.inference_backends),
            Collection::InferenceProfile => Some(&self.inference_profiles),
            Collection::ToolServiceRegistry => Some(&self.tool_service_registries),
            Collection::Task => Some(&self.tasks),
            Collection::Schedule => Some(&self.schedules),
            Collection::EventTrigger => Some(&self.event_triggers),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ConfigApplyCounts {
    pub(crate) agent_principal: usize,
    pub(crate) agent_behaviors: usize,
    pub(crate) tool_selections: usize,
    pub(crate) inference_backends: usize,
    pub(crate) inference_profiles: usize,
    pub(crate) tool_service_registries: usize,
    pub(crate) tasks: usize,
    pub(crate) schedules: usize,
    pub(crate) event_triggers: usize,
}

impl ConfigApplyCounts {
    pub(crate) fn set(&mut self, collection: Collection, count: usize) {
        match collection {
            Collection::AgentPrincipal => self.agent_principal = count,
            Collection::AgentBehavior => self.agent_behaviors = count,
            Collection::ToolSelection => self.tool_selections = count,
            Collection::InferenceBackend => self.inference_backends = count,
            Collection::InferenceProfile => self.inference_profiles = count,
            Collection::ToolServiceRegistry => self.tool_service_registries = count,
            Collection::Task => self.tasks = count,
            Collection::Schedule => self.schedules = count,
            Collection::EventTrigger => self.event_triggers = count,
        }
    }

    pub(crate) fn changed(&self) -> bool {
        Collection::ALL.iter().copied().any(|collection| {
            let count = match collection {
                Collection::AgentPrincipal => self.agent_principal,
                Collection::AgentBehavior => self.agent_behaviors,
                Collection::ToolSelection => self.tool_selections,
                Collection::InferenceBackend => self.inference_backends,
                Collection::InferenceProfile => self.inference_profiles,
                Collection::ToolServiceRegistry => self.tool_service_registries,
                Collection::Task => self.tasks,
                Collection::Schedule => self.schedules,
                Collection::EventTrigger => self.event_triggers,
            };
            count > 0
        })
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
    pub(crate) planned: desired_state::DesiredStateDiffCollectionsCounts,
    pub(crate) applied: ConfigApplyCounts,
    pub(crate) remaining: desired_state::DesiredStateDiffCollectionsCounts,
}
