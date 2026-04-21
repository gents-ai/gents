pub(crate) mod convert;
pub(crate) mod diff;
pub(crate) mod load;
pub(crate) mod normalize;
#[cfg(test)]
mod tests;
pub(crate) mod validate;

pub(crate) use convert::{
    export_bundle_from_manifest, manifest_from_export_bundle,
    normalize_tool_service_registry_storage_fields,
};
pub(crate) use diff::diff_manifests;
pub(crate) use load::{load_manifest_root, validate_manifest_root};
pub(crate) use normalize::strip_deprecated_inference_backend_fields;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use defra_agent::BackendProviderKind;

pub(crate) const DEFAULT_TOOL_SERVICE_MCP_PATH: &str = "/mcp";
pub(crate) const TOOL_SERVICE_ADDRESS_FIELDS: &[&str] = &["hostname", "tailscale_ip", "lan_ip"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredAgentPrincipal {
    pub(crate) agent_did: String,
    pub(crate) display_name: Option<String>,
    pub(crate) default_behavior_id: Option<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredAgentBehavior {
    pub(crate) behavior_id: String,
    pub(crate) agent_did: String,
    pub(crate) display_name: Option<String>,
    pub(crate) system_prompt: Option<String>,
    pub(crate) backend_id: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) tool_selection_id: Option<String>,
    pub(crate) inference_profile_id: Option<String>,
    pub(crate) compaction_strategy: Option<String>,
    pub(crate) compaction_threshold: Option<f64>,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredToolSelection {
    pub(crate) selection_id: String,
    pub(crate) agent_did: String,
    pub(crate) display_name: Option<String>,
    pub(crate) enable_file_tools: bool,
    pub(crate) file_tools_mode: String,
    pub(crate) file_tool_root: Option<String>,
    pub(crate) enable_bash: bool,
    pub(crate) bash_mode: String,
    #[serde(default)]
    pub(crate) cli_tool_names: Vec<String>,
    pub(crate) enable_meta_tools: bool,
    #[serde(default)]
    pub(crate) delegate_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DesiredInferenceBackend {
    pub(crate) backend_id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
    pub(crate) max_concurrent: i64,
    #[serde(default = "normalize::default_max_queue_depth")]
    pub(crate) max_queue_depth: i64,
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) models: Vec<String>,
}

impl<'de> Deserialize<'de> for DesiredInferenceBackend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            backend_id: String,
            name: String,
            #[serde(default)]
            provider_kind: BackendProviderKind,
            endpoint: String,
            api_key: Option<String>,
            api_key_env_var: Option<String>,
            max_concurrent: i64,
            #[serde(default = "normalize::default_max_queue_depth")]
            max_queue_depth: i64,
            enabled: bool,
            #[serde(default)]
            models: Vec<String>,
        }

        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let serde_json::Value::Object(object) = &mut value {
            normalize::strip_deprecated_inference_backend_fields(object);
        }
        let wire = Wire::deserialize(value).map_err(D::Error::custom)?;

        Ok(Self {
            backend_id: wire.backend_id,
            name: wire.name,
            provider_kind: wire.provider_kind,
            endpoint: wire.endpoint,
            api_key: wire.api_key,
            api_key_env_var: wire.api_key_env_var,
            max_concurrent: wire.max_concurrent,
            max_queue_depth: wire.max_queue_depth,
            enabled: wire.enabled,
            models: wire.models,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredInferenceProfile {
    pub(crate) profile_id: String,
    pub(crate) display_name: Option<String>,
    pub(crate) context_window: Option<i64>,
    pub(crate) max_output_tokens: Option<i64>,
    pub(crate) max_turns: Option<i64>,
    pub(crate) temperature: Option<f64>,
    pub(crate) stream_batch_ms: Option<i64>,
    pub(crate) deadline_duration_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DesiredToolServiceRegistry {
    pub(crate) service_id: String,
    pub(crate) display_name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) hostname: Option<String>,
    pub(crate) tailscale_ip: Option<String>,
    pub(crate) lan_ip: Option<String>,
    pub(crate) mcp_port: Option<i64>,
    pub(crate) mcp_path: Option<String>,
}

impl<'de> Deserialize<'de> for DesiredToolServiceRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            service_id: String,
            display_name: Option<String>,
            description: Option<String>,
            hostname: Option<String>,
            tailscale_ip: Option<String>,
            lan_ip: Option<String>,
            mcp_port: Option<i64>,
            mcp_path: Option<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            service_id: wire.service_id,
            display_name: wire.display_name,
            description: wire.description,
            hostname: Some(validate::normalize_tool_service_string(wire.hostname)),
            tailscale_ip: Some(validate::normalize_tool_service_string(wire.tailscale_ip)),
            lan_ip: Some(validate::normalize_tool_service_string(wire.lan_ip)),
            mcp_port: wire.mcp_port,
            mcp_path: Some(validate::normalize_tool_service_mcp_path(wire.mcp_path)),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredScheduledTask {
    pub(crate) task_id: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: String,
    pub(crate) name: String,
    pub(crate) prompt: String,
    pub(crate) interval_secs: i64,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DesiredStateManifest {
    pub(crate) agent_principal: DesiredAgentPrincipal,
    pub(crate) agent_behaviors: Vec<DesiredAgentBehavior>,
    pub(crate) tool_selections: Vec<DesiredToolSelection>,
    pub(crate) inference_backends: Vec<DesiredInferenceBackend>,
    pub(crate) inference_profiles: Vec<DesiredInferenceProfile>,
    pub(crate) tool_service_registries: Vec<DesiredToolServiceRegistry>,
    pub(crate) scheduled_tasks: Vec<DesiredScheduledTask>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateCollectionDiff {
    pub(crate) create: Vec<String>,
    pub(crate) update: Vec<String>,
    pub(crate) unchanged: Vec<String>,
    pub(crate) live_only: Vec<String>,
}

impl DesiredStateCollectionDiff {
    pub(super) fn counts(&self) -> DesiredStateDiffCounts {
        DesiredStateDiffCounts {
            create: self.create.len(),
            update: self.update.len(),
            unchanged: self.unchanged.len(),
            live_only: self.live_only.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCounts {
    pub(crate) create: usize,
    pub(crate) update: usize,
    pub(crate) unchanged: usize,
    pub(crate) live_only: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCollections {
    pub(crate) agent_principal: DesiredStateCollectionDiff,
    pub(crate) agent_behaviors: DesiredStateCollectionDiff,
    pub(crate) tool_selections: DesiredStateCollectionDiff,
    pub(crate) inference_backends: DesiredStateCollectionDiff,
    pub(crate) inference_profiles: DesiredStateCollectionDiff,
    pub(crate) tool_service_registries: DesiredStateCollectionDiff,
    pub(crate) scheduled_tasks: DesiredStateCollectionDiff,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCollectionsCounts {
    pub(crate) agent_principal: DesiredStateDiffCounts,
    pub(crate) agent_behaviors: DesiredStateDiffCounts,
    pub(crate) tool_selections: DesiredStateDiffCounts,
    pub(crate) inference_backends: DesiredStateDiffCounts,
    pub(crate) inference_profiles: DesiredStateDiffCounts,
    pub(crate) tool_service_registries: DesiredStateDiffCounts,
    pub(crate) scheduled_tasks: DesiredStateDiffCounts,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffReport {
    pub(crate) status: &'static str,
    pub(crate) ok: bool,
    pub(crate) root: String,
    pub(crate) access_mode: String,
    pub(crate) agent_did: String,
    pub(crate) counts: DesiredStateDiffCollectionsCounts,
    pub(crate) collections: DesiredStateDiffCollections,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateCounts {
    pub(crate) agent_principal: usize,
    pub(crate) agent_behaviors: usize,
    pub(crate) tool_selections: usize,
    pub(crate) inference_backends: usize,
    pub(crate) inference_profiles: usize,
    pub(crate) tool_service_registries: usize,
    pub(crate) scheduled_tasks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateValidationReport {
    pub(crate) status: &'static str,
    pub(crate) ok: bool,
    pub(crate) root: String,
    pub(crate) agent_did: Option<String>,
    pub(crate) counts: DesiredStateCounts,
    pub(crate) errors: Vec<String>,
}

impl DesiredStateValidationReport {
    pub(crate) fn is_ok(&self) -> bool {
        self.ok
    }
}

use defra_agent::DesiredFields;

impl DesiredFields for DesiredAgentPrincipal {
    fn collection_tag(&self) -> &'static str {
        "agent_principal"
    }
}
impl DesiredFields for DesiredAgentBehavior {
    fn collection_tag(&self) -> &'static str {
        "agent_behaviors"
    }
}
impl DesiredFields for DesiredToolSelection {
    fn collection_tag(&self) -> &'static str {
        "tool_selections"
    }
}
impl DesiredFields for DesiredInferenceBackend {
    fn collection_tag(&self) -> &'static str {
        "inference_backends"
    }
}
impl DesiredFields for DesiredInferenceProfile {
    fn collection_tag(&self) -> &'static str {
        "inference_profiles"
    }
}
impl DesiredFields for DesiredToolServiceRegistry {
    fn collection_tag(&self) -> &'static str {
        "tool_service_registries"
    }
}
impl DesiredFields for DesiredScheduledTask {
    fn collection_tag(&self) -> &'static str {
        "scheduled_tasks"
    }
}

#[cfg(test)]
mod desired_fields_tests {
    use super::*;
    use defra_agent::DesiredFields;

    #[test]
    fn desired_structs_report_their_collection_tags() {
        let p = DesiredAgentPrincipal {
            agent_did: "did:x".into(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        };
        assert_eq!(p.collection_tag(), "agent_principal");
    }
}
