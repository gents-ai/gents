use serde::{Deserialize, Serialize};

pub const DEFAULTS_JSON: &str = include_str!("lsp_defaults.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub is_linter: bool,
    #[serde(default = "default_priority")]
    pub priority: u16,
    #[serde(default)]
    pub language_id: Option<String>,
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    #[serde(default)]
    pub capabilities: Option<serde_json::Value>,
    #[serde(default)]
    pub workspace_ready_timings: Option<serde_json::Value>,
    #[serde(default)]
    pub warmup_timeout_ms: Option<u64>,
}

fn default_priority() -> u16 {
    50
}

pub fn builtin_catalog() -> Vec<CatalogServer> {
    serde_json::from_str(DEFAULTS_JSON).expect("lsp defaults.json is valid")
}
