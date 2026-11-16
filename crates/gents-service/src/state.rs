use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::args::{ToolCeilingArg, ToolPackageArg};

#[cfg(not(all(debug_assertions, feature = "native-e2e")))]
pub const DEFAULT_HTTP_PORT: u16 = 9191;
#[cfg(all(debug_assertions, feature = "native-e2e"))]
pub const DEFAULT_HTTP_PORT: u16 = 21919;
pub const INIT_CONFIG_FILE_NAME: &str = "init.json";
pub const RUNTIME_STATE_FILE_NAME: &str = gents_store::local::home::RUNTIME_STATE_FILE_NAME;

pub type StoredInitConfig =
    gents_store::local::home::StoredInitConfig<ToolPackageArg, ToolCeilingArg>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pAdmissionState {
    pub max_pending_dags: usize,
    pub max_concurrent_push_tasks: usize,
    pub max_concurrent_dag_fetches: usize,
    pub rate_limit_burst: u32,
    pub rate_limit_rate: f64,
}

impl P2pAdmissionState {
    pub fn to_json(&self) -> Value {
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
pub struct StoredRuntimeState {
    pub home: String,
    pub graphql: String,
    pub node_name: String,
    pub node_did: String,
    pub default_agent_did: String,
    #[serde(default = "default_p2p_transport")]
    pub p2p_transport: String,
    #[serde(default)]
    pub p2p_peer_id: Option<String>,
    #[serde(default)]
    pub p2p_listen_addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_admission: Option<P2pAdmissionState>,
}

fn default_p2p_transport() -> String {
    "iroh".to_string()
}
