use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use defra_agent::BackendProviderKind;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

const AGENT_PRINCIPAL_FILE: &str = "agent-principal.json";
const AGENT_BEHAVIORS_FILE: &str = "agent-behaviors.json";
const TOOL_SELECTIONS_FILE: &str = "tool-selections.json";
const INFERENCE_BACKENDS_FILE: &str = "inference-backends.json";
const INFERENCE_PROFILES_FILE: &str = "inference-profiles.json";
const TOOL_SERVICES_FILE: &str = "tool-services.json";
const TOOL_SERVICES_DIR: &str = "tool-services";
const SCHEDULED_TASKS_FILE: &str = "scheduled-tasks.json";
const SCHEDULED_TASKS_DIR: &str = "scheduled-tasks";

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
    #[serde(default = "default_max_queue_depth")]
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
            #[serde(default = "default_max_queue_depth")]
            max_queue_depth: i64,
            enabled: bool,
            #[serde(default)]
            models: Vec<String>,
        }

        let mut value = Value::deserialize(deserializer)?;
        if let Value::Object(object) = &mut value {
            strip_deprecated_inference_backend_fields(object);
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

const DEFAULT_TOOL_SERVICE_MCP_PATH: &str = "/mcp";
const TOOL_SERVICE_ADDRESS_FIELDS: &[&str] = &["hostname", "tailscale_ip", "lan_ip"];

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
            hostname: Some(normalize_tool_service_string(wire.hostname)),
            tailscale_ip: Some(normalize_tool_service_string(wire.tailscale_ip)),
            lan_ip: Some(normalize_tool_service_string(wire.lan_ip)),
            mcp_port: wire.mcp_port,
            mcp_path: Some(normalize_tool_service_mcp_path(wire.mcp_path)),
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
    fn counts(&self) -> DesiredStateDiffCounts {
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

pub(crate) fn validate_manifest_root(root: &Path) -> DesiredStateValidationReport {
    load_manifest_root(root).1
}

pub(crate) fn load_manifest_root(
    root: &Path,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    let root_display = root.display().to_string();
    let mut errors = Vec::new();

    if !root.exists() {
        errors.push(format!("manifest root does not exist: {root_display}"));
        return (
            None,
            DesiredStateValidationReport {
                status: "invalid",
                ok: false,
                root: root_display,
                agent_did: None,
                counts: DesiredStateCounts {
                    agent_principal: 0,
                    agent_behaviors: 0,
                    tool_selections: 0,
                    inference_backends: 0,
                    inference_profiles: 0,
                    tool_service_registries: 0,
                    scheduled_tasks: 0,
                },
                errors,
            },
        );
    }
    if !root.is_dir() {
        errors.push(format!("manifest root is not a directory: {root_display}"));
        return (
            None,
            DesiredStateValidationReport {
                status: "invalid",
                ok: false,
                root: root_display,
                agent_did: None,
                counts: DesiredStateCounts {
                    agent_principal: 0,
                    agent_behaviors: 0,
                    tool_selections: 0,
                    inference_backends: 0,
                    inference_profiles: 0,
                    tool_service_registries: 0,
                    scheduled_tasks: 0,
                },
                errors,
            },
        );
    }

    let principal =
        load_required_json::<DesiredAgentPrincipal>(root, AGENT_PRINCIPAL_FILE, &mut errors);
    let behaviors =
        load_required_json::<Vec<DesiredAgentBehavior>>(root, AGENT_BEHAVIORS_FILE, &mut errors);
    let tool_selections =
        load_required_json::<Vec<DesiredToolSelection>>(root, TOOL_SELECTIONS_FILE, &mut errors);
    let backends = load_required_json::<Vec<DesiredInferenceBackend>>(
        root,
        INFERENCE_BACKENDS_FILE,
        &mut errors,
    );
    let inference_profiles = load_optional_json::<Vec<DesiredInferenceProfile>>(
        root,
        INFERENCE_PROFILES_FILE,
        &mut errors,
    )
    .unwrap_or_default();
    let tool_service_registries = load_optional_json_collection::<DesiredToolServiceRegistry>(
        root,
        TOOL_SERVICES_FILE,
        TOOL_SERVICES_DIR,
        &mut errors,
    )
    .unwrap_or_default();
    let scheduled_tasks = load_optional_json_collection::<DesiredScheduledTask>(
        root,
        SCHEDULED_TASKS_FILE,
        SCHEDULED_TASKS_DIR,
        &mut errors,
    )
    .unwrap_or_default();

    let counts = DesiredStateCounts {
        agent_principal: usize::from(principal.is_some()),
        agent_behaviors: behaviors.as_ref().map_or(0, Vec::len),
        tool_selections: tool_selections.as_ref().map_or(0, Vec::len),
        inference_backends: backends.as_ref().map_or(0, Vec::len),
        inference_profiles: inference_profiles.len(),
        tool_service_registries: tool_service_registries.len(),
        scheduled_tasks: scheduled_tasks.len(),
    };

    let agent_did = principal.as_ref().map(|value| value.agent_did.clone());

    let manifest =
        if let (Some(principal), Some(behaviors), Some(tool_selections), Some(backends)) =
            (principal, behaviors, tool_selections, backends)
        {
            let mut manifest = DesiredStateManifest {
                agent_principal: principal,
                agent_behaviors: behaviors,
                tool_selections,
                inference_backends: backends,
                inference_profiles,
                tool_service_registries,
                scheduled_tasks,
            };
            normalize_manifest(&mut manifest);
            validate_manifest(&manifest, &mut errors);
            Some(manifest)
        } else {
            None
        };

    (
        manifest,
        DesiredStateValidationReport {
            status: if errors.is_empty() {
                "validated"
            } else {
                "invalid"
            },
            ok: errors.is_empty(),
            root: root_display,
            agent_did,
            counts,
            errors,
        },
    )
}

fn load_required_json<T>(root: &Path, file_name: &str, errors: &mut Vec<String>) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_file(root, file_name) {
        Ok(Some(value)) => Some(value),
        Ok(None) => {
            errors.push(format!(
                "required manifest file is missing: {}",
                root.join(file_name).display()
            ));
            None
        }
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn load_optional_json<T>(root: &Path, file_name: &str, errors: &mut Vec<String>) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_file(root, file_name) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn load_optional_json_collection<T>(
    root: &Path,
    file_name: &str,
    dir_name: &str,
    errors: &mut Vec<String>,
) -> Option<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_collection(root, file_name, dir_name) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn load_json_file<T>(root: &Path, file_name: &str) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let path = root.join(file_name);
    if !path.exists() {
        return Ok(None);
    }

    let bytes =
        fs::read(&path).map_err(|error| format!("reading {} failed: {error}", path.display()))?;
    serde_json::from_slice::<T>(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn load_json_collection<T>(
    root: &Path,
    file_name: &str,
    dir_name: &str,
) -> Result<Option<Vec<T>>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let file_path = root.join(file_name);
    let dir_path = root.join(dir_name);

    if file_path.exists() && dir_path.exists() {
        return Err(format!(
            "manifest root must not contain both {} and {}",
            file_path.display(),
            dir_path.display()
        ));
    }

    if file_path.exists() {
        return load_json_file(root, file_name);
    }

    if !dir_path.exists() {
        return Ok(None);
    }
    if !dir_path.is_dir() {
        return Err(format!(
            "manifest collection path is not a directory: {}",
            dir_path.display()
        ));
    }

    let mut entry_paths = fs::read_dir(&dir_path)
        .map_err(|error| format!("reading {} failed: {error}", dir_path.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("reading {} failed: {error}", dir_path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entry_paths.sort();

    let mut values = Vec::new();
    for path in entry_paths {
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("reading {} failed: {error}", path.display()))?;
        let value = serde_json::from_slice::<T>(&bytes)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        values.push(value);
    }

    Ok(Some(values))
}

fn validate_manifest(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let principal_agent_did = manifest.agent_principal.agent_did.trim();
    if principal_agent_did.is_empty() {
        errors.push("agent-principal.json must contain a non-empty agent_did".to_string());
    }

    let mut behavior_ids = BTreeSet::new();
    let mut backend_ids = BTreeSet::new();
    let mut tool_selection_ids = BTreeSet::new();
    let mut profile_ids = BTreeSet::new();
    let mut service_ids = BTreeSet::new();
    let mut task_ids = BTreeSet::new();

    for backend in &manifest.inference_backends {
        let backend_id = backend.backend_id.trim();
        if backend_id.is_empty() {
            errors.push(
                "inference-backends.json contains a backend with an empty backend_id".to_string(),
            );
        } else if !backend_ids.insert(backend_id.to_string()) {
            errors.push(format!(
                "duplicate backend_id in inference-backends.json: {backend_id}"
            ));
        }

        if backend.endpoint.trim().is_empty() {
            errors.push(format!(
                "backend {} in inference-backends.json must contain a non-empty endpoint",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.is_empty())
        {
            errors.push(format!(
                "backend {} in inference-backends.json contains an empty api_key",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            && backend
                .api_key_env_var
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
        {
            errors.push(format!(
                "backend {} in inference-backends.json must not set both api_key and api_key_env_var",
                backend.backend_id
            ));
        }
    }

    for selection in &manifest.tool_selections {
        let selection_id = selection.selection_id.trim();
        if selection_id.is_empty() {
            errors.push(
                "tool-selections.json contains a tool selection with an empty selection_id"
                    .to_string(),
            );
        } else if !tool_selection_ids.insert(selection_id.to_string()) {
            errors.push(format!(
                "duplicate selection_id in tool-selections.json: {selection_id}"
            ));
        }

        if !principal_agent_did.is_empty() && selection.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "tool selection {} belongs to {} not {}",
                selection.selection_id, selection.agent_did, manifest.agent_principal.agent_did
            ));
        }
    }

    for profile in &manifest.inference_profiles {
        let profile_id = profile.profile_id.trim();
        if profile_id.is_empty() {
            errors.push(
                "inference-profiles.json contains a profile with an empty profile_id".to_string(),
            );
        } else if !profile_ids.insert(profile_id.to_string()) {
            errors.push(format!(
                "duplicate profile_id in inference-profiles.json: {profile_id}"
            ));
        }
    }

    for service in &manifest.tool_service_registries {
        let service_id = service.service_id.trim();
        if service_id.is_empty() {
            errors.push(
                "tool-services manifest contains a service with an empty service_id".to_string(),
            );
        } else if !service_ids.insert(service_id.to_string()) {
            errors.push(format!(
                "duplicate service_id in tool-services manifest: {service_id}"
            ));
        }

        if service.mcp_port.unwrap_or_default() <= 0 {
            errors.push(format!(
                "service {} in tool-services manifest must contain a positive mcp_port",
                service.service_id
            ));
        }

        if non_empty(&service.hostname).is_none()
            && non_empty(&service.tailscale_ip).is_none()
            && non_empty(&service.lan_ip).is_none()
        {
            errors.push(format!(
                "service {} in tool-services manifest must contain at least one of hostname, tailscale_ip, or lan_ip",
                service.service_id
            ));
        }
    }

    for behavior in &manifest.agent_behaviors {
        let behavior_id = behavior.behavior_id.trim();
        if behavior_id.is_empty() {
            errors.push(
                "agent-behaviors.json contains a behavior with an empty behavior_id".to_string(),
            );
        } else if !behavior_ids.insert(behavior_id.to_string()) {
            errors.push(format!(
                "duplicate behavior_id in agent-behaviors.json: {behavior_id}"
            ));
        }

        if !principal_agent_did.is_empty() && behavior.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "behavior {} belongs to {} not {}",
                behavior.behavior_id, behavior.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if let Some(backend_id) = non_empty(&behavior.backend_id) {
            if !backend_ids.contains(backend_id) {
                errors.push(format!(
                    "behavior {} references missing backend_id {}",
                    behavior.behavior_id, backend_id
                ));
            }
        }

        if let Some(selection_id) = non_empty(&behavior.tool_selection_id) {
            if !tool_selection_ids.contains(selection_id) {
                errors.push(format!(
                    "behavior {} references missing tool_selection_id {}",
                    behavior.behavior_id, selection_id
                ));
            }
        }

        if let Some(profile_id) = non_empty(&behavior.inference_profile_id) {
            if !profile_ids.contains(profile_id) {
                errors.push(format!(
                    "behavior {} references missing inference_profile_id {}",
                    behavior.behavior_id, profile_id
                ));
            }
        }
    }

    match non_empty(&manifest.agent_principal.default_behavior_id) {
        Some(default_behavior_id) => {
            if !behavior_ids.contains(default_behavior_id) {
                errors.push(format!(
                    "agent-principal.json default_behavior_id {} is not present in agent-behaviors.json",
                    default_behavior_id
                ));
            }
        }
        None => errors
            .push("agent-principal.json must contain a non-empty default_behavior_id".to_string()),
    }

    for task in &manifest.scheduled_tasks {
        let task_id = task.task_id.trim();
        if task_id.is_empty() {
            errors
                .push("scheduled-tasks manifest contains a task with an empty task_id".to_string());
        } else if !task_ids.insert(task_id.to_string()) {
            errors.push(format!(
                "duplicate task_id in scheduled-tasks manifest: {task_id}"
            ));
        }

        if !principal_agent_did.is_empty() && task.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "scheduled task {} belongs to {} not {}",
                task.task_id, task.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if task.name.trim().is_empty() {
            errors.push(format!(
                "scheduled task {} in scheduled-tasks manifest must contain a non-empty name",
                task.task_id
            ));
        }

        if task.prompt.trim().is_empty() {
            errors.push(format!(
                "scheduled task {} in scheduled-tasks manifest must contain a non-empty prompt",
                task.task_id
            ));
        }

        if task.interval_secs <= 0 {
            errors.push(format!(
                "scheduled task {} in scheduled-tasks manifest must contain interval_secs > 0",
                task.task_id
            ));
        }

        if !behavior_ids.contains(task.behavior_id.trim()) {
            errors.push(format!(
                "scheduled task {} references missing behavior_id {}",
                task.task_id, task.behavior_id
            ));
        }
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalize_tool_service_string(value: Option<String>) -> String {
    value.unwrap_or_default().trim().to_string()
}

fn normalize_tool_service_mcp_path(value: Option<String>) -> String {
    let trimmed = value.as_deref().unwrap_or_default().trim();
    if trimmed.is_empty() {
        DEFAULT_TOOL_SERVICE_MCP_PATH.to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn optional_string_from_value(field: &str, value: Option<&Value>) -> Result<Option<String>> {
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(anyhow!(
            "ToolServiceRegistry field {field} must be a string or null, got {value}"
        )),
    }
}

fn optional_i64_from_value(field: &str, value: Option<&Value>) -> Result<Option<i64>> {
    match value {
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow!("ToolServiceRegistry field {field} must be an integer")),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(anyhow!(
            "ToolServiceRegistry field {field} must be an integer or null, got {value}"
        )),
    }
}

fn tool_service_registry_from_live_value(value: &Value) -> Result<DesiredToolServiceRegistry> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("expected ToolServiceRegistry live row to be an object"))?;
    let service_id = object
        .get("service_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ToolServiceRegistry live row is missing service_id"))?
        .to_string();

    Ok(DesiredToolServiceRegistry {
        service_id,
        display_name: optional_string_from_value("display_name", object.get("display_name"))?,
        description: optional_string_from_value("description", object.get("description"))?,
        hostname: optional_string_from_value("hostname", object.get("hostname"))?,
        tailscale_ip: optional_string_from_value("tailscale_ip", object.get("tailscale_ip"))?,
        lan_ip: optional_string_from_value("lan_ip", object.get("lan_ip"))?,
        mcp_port: optional_i64_from_value("mcp_port", object.get("mcp_port"))?,
        mcp_path: optional_string_from_value("mcp_path", object.get("mcp_path"))?,
    })
}

pub(crate) fn normalize_tool_service_registry_storage_fields(
    object: &mut Map<String, Value>,
) -> Result<()> {
    for field in TOOL_SERVICE_ADDRESS_FIELDS {
        let normalized =
            normalize_tool_service_string(optional_string_from_value(field, object.get(*field))?);
        object.insert((*field).to_string(), Value::String(normalized));
    }

    let mcp_path = normalize_tool_service_mcp_path(optional_string_from_value(
        "mcp_path",
        object.get("mcp_path"),
    )?);
    object.insert("mcp_path".to_string(), Value::String(mcp_path));

    Ok(())
}

pub(crate) fn manifest_from_export_bundle(
    bundle: &super::ConfigExportBundle,
) -> Result<DesiredStateManifest> {
    let principal = bundle
        .agent_principal
        .as_ref()
        .ok_or_else(|| anyhow!("config export bundle is missing agent_principal"))?;

    let mut manifest = DesiredStateManifest {
        agent_principal: desired_from_value(
            principal,
            &[
                "agent_did",
                "display_name",
                "default_behavior_id",
                "enabled",
            ],
        )?,
        agent_behaviors: bundle
            .agent_behaviors
            .iter()
            .map(|value| {
                desired_from_value(
                    value,
                    &[
                        "behavior_id",
                        "agent_did",
                        "display_name",
                        "system_prompt",
                        "backend_id",
                        "model_name",
                        "tool_selection_id",
                        "inference_profile_id",
                        "compaction_strategy",
                        "compaction_threshold",
                        "enabled",
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?,
        tool_selections: bundle
            .tool_selections
            .iter()
            .map(|value| {
                desired_from_value(
                    value,
                    &[
                        "selection_id",
                        "agent_did",
                        "display_name",
                        "enable_file_tools",
                        "file_tools_mode",
                        "file_tool_root",
                        "enable_bash",
                        "bash_mode",
                        "cli_tool_names",
                        "enable_meta_tools",
                        "delegate_to",
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?,
        inference_backends: bundle
            .inference_backends
            .iter()
            .map(|value| {
                desired_from_value(
                    value,
                    &[
                        "backend_id",
                        "name",
                        "provider_kind",
                        "endpoint",
                        "api_key",
                        "api_key_env_var",
                        "max_concurrent",
                        "max_queue_depth",
                        "enabled",
                        "models",
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?,
        inference_profiles: bundle
            .inference_profiles
            .iter()
            .map(|value| {
                desired_from_value(
                    value,
                    &[
                        "profile_id",
                        "display_name",
                        "context_window",
                        "max_output_tokens",
                        "max_turns",
                        "temperature",
                        "stream_batch_ms",
                        "deadline_duration_secs",
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?,
        tool_service_registries: bundle
            .tool_service_registries
            .iter()
            .map(tool_service_registry_from_live_value)
            .collect::<Result<Vec<_>>>()?,
        scheduled_tasks: bundle
            .scheduled_tasks
            .iter()
            .map(|value| {
                desired_from_value(
                    value,
                    &[
                        "task_id",
                        "agent_did",
                        "behavior_id",
                        "name",
                        "prompt",
                        "interval_secs",
                        "enabled",
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?,
    };
    normalize_manifest(&mut manifest);
    Ok(manifest)
}

pub(crate) fn export_bundle_from_manifest(
    manifest: &DesiredStateManifest,
    access_mode: &str,
) -> Result<super::ConfigExportBundle> {
    Ok(super::ConfigExportBundle {
        format: super::CONFIG_EXPORT_FORMAT.to_string(),
        agent_did: manifest.agent_principal.agent_did.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access_mode.to_string(),
        agent_principal: Some(serde_json::to_value(&manifest.agent_principal)?),
        agent_behaviors: manifest
            .agent_behaviors
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
        tool_selections: manifest
            .tool_selections
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
        inference_backends: manifest
            .inference_backends
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
        inference_profiles: manifest
            .inference_profiles
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
        tool_service_registries: manifest
            .tool_service_registries
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
        scheduled_tasks: manifest
            .scheduled_tasks
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<Vec<_>>>()?,
    })
}

pub(crate) fn diff_manifests(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
) -> DesiredStateDiffReport {
    let agent_principal = diff_single(
        &desired.agent_principal.agent_did,
        Some(&desired.agent_principal),
        live_principal,
    );
    let agent_behaviors = diff_collection(
        desired
            .agent_behaviors
            .iter()
            .map(|value| (value.behavior_id.clone(), value))
            .collect(),
        live.agent_behaviors
            .iter()
            .map(|value| (value.behavior_id.clone(), value))
            .collect(),
    );
    let tool_selections = diff_collection(
        desired
            .tool_selections
            .iter()
            .map(|value| (value.selection_id.clone(), value))
            .collect(),
        live.tool_selections
            .iter()
            .map(|value| (value.selection_id.clone(), value))
            .collect(),
    );
    let inference_backends = diff_collection(
        desired
            .inference_backends
            .iter()
            .map(|value| (value.backend_id.clone(), value))
            .collect(),
        live.inference_backends
            .iter()
            .map(|value| (value.backend_id.clone(), value))
            .collect(),
    );
    let inference_profiles = diff_collection(
        desired
            .inference_profiles
            .iter()
            .map(|value| (value.profile_id.clone(), value))
            .collect(),
        live.inference_profiles
            .iter()
            .map(|value| (value.profile_id.clone(), value))
            .collect(),
    );
    let tool_service_registries = diff_collection(
        desired
            .tool_service_registries
            .iter()
            .map(|value| (value.service_id.clone(), value))
            .collect(),
        live.tool_service_registries
            .iter()
            .map(|value| (value.service_id.clone(), value))
            .collect(),
    );
    let scheduled_tasks = diff_collection(
        desired
            .scheduled_tasks
            .iter()
            .map(|value| (value.task_id.clone(), value))
            .collect(),
        live.scheduled_tasks
            .iter()
            .map(|value| (value.task_id.clone(), value))
            .collect(),
    );

    let counts = DesiredStateDiffCollectionsCounts {
        agent_principal: agent_principal.counts(),
        agent_behaviors: agent_behaviors.counts(),
        tool_selections: tool_selections.counts(),
        inference_backends: inference_backends.counts(),
        inference_profiles: inference_profiles.counts(),
        tool_service_registries: tool_service_registries.counts(),
        scheduled_tasks: scheduled_tasks.counts(),
    };
    let ok = [
        &counts.agent_principal,
        &counts.agent_behaviors,
        &counts.tool_selections,
        &counts.inference_backends,
        &counts.inference_profiles,
        &counts.tool_service_registries,
        &counts.scheduled_tasks,
    ]
    .iter()
    .all(|count| count.create == 0 && count.update == 0 && count.live_only == 0);

    DesiredStateDiffReport {
        status: "diffed",
        ok,
        root: root.display().to_string(),
        access_mode: access_mode.to_string(),
        agent_did: desired.agent_principal.agent_did.clone(),
        counts,
        collections: DesiredStateDiffCollections {
            agent_principal,
            agent_behaviors,
            tool_selections,
            inference_backends,
            inference_profiles,
            tool_service_registries,
            scheduled_tasks,
        },
    }
}

fn normalize_manifest(manifest: &mut DesiredStateManifest) {
    manifest
        .agent_behaviors
        .sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
    manifest
        .tool_selections
        .sort_by(|left, right| left.selection_id.cmp(&right.selection_id));
    manifest
        .inference_backends
        .sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    manifest
        .inference_profiles
        .sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    manifest
        .tool_service_registries
        .sort_by(|left, right| left.service_id.cmp(&right.service_id));
    manifest
        .scheduled_tasks
        .sort_by(|left, right| left.task_id.cmp(&right.task_id));

    for selection in &mut manifest.tool_selections {
        selection.cli_tool_names.sort();
        selection.cli_tool_names.dedup();
        selection.delegate_to.sort();
        selection.delegate_to.dedup();
    }
    for backend in &mut manifest.inference_backends {
        backend.models.sort();
        backend.models.dedup();
    }
}

fn default_max_queue_depth() -> i64 {
    100
}

pub(crate) fn strip_deprecated_inference_backend_fields(object: &mut Map<String, Value>) {
    for field in [
        "supports_tool_calls",
        "supports_streaming",
        "supports_structured_outputs",
        "supports_json_schema",
        "context_window",
        "max_output_tokens",
    ] {
        object.remove(field);
    }
}

fn desired_from_value<T>(value: &Value, allowed_fields: &[&str]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("expected object while projecting desired-state document"))?;
    let projected = allowed_fields
        .iter()
        .filter_map(|field| {
            object
                .get(*field)
                .filter(|value| !value.is_null())
                .map(|value| ((*field).to_string(), value.clone()))
        })
        .collect::<Map<String, Value>>();
    Ok(serde_json::from_value(Value::Object(projected))?)
}

fn diff_single<T>(id: &str, desired: Option<&T>, live: Option<&T>) -> DesiredStateCollectionDiff
where
    T: PartialEq,
{
    match (desired, live) {
        (Some(desired), Some(live)) => {
            if desired == live {
                DesiredStateCollectionDiff {
                    create: Vec::new(),
                    update: Vec::new(),
                    unchanged: vec![id.to_string()],
                    live_only: Vec::new(),
                }
            } else {
                DesiredStateCollectionDiff {
                    create: Vec::new(),
                    update: vec![id.to_string()],
                    unchanged: Vec::new(),
                    live_only: Vec::new(),
                }
            }
        }
        (Some(_), None) => DesiredStateCollectionDiff {
            create: vec![id.to_string()],
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
        (None, Some(_)) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: vec![id.to_string()],
        },
        (None, None) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
    }
}

fn diff_collection<T>(
    desired: Vec<(String, &T)>,
    live: Vec<(String, &T)>,
) -> DesiredStateCollectionDiff
where
    T: PartialEq,
{
    let desired_map = desired
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let live_map = live
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();

    let all_ids = desired_map
        .keys()
        .chain(live_map.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in all_ids {
        match (desired_map.get(&id), live_map.get(&id)) {
            (Some(desired), Some(live)) => {
                if *desired == *live {
                    unchanged.push(id);
                } else {
                    update.push(id);
                }
            }
            (Some(_), None) => create.push(id),
            (None, Some(_)) => live_only.push(id),
            (None, None) => {}
        }
    }

    DesiredStateCollectionDiff {
        create,
        update,
        unchanged,
        live_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn desired_tool_service_registry_normalizes_address_storage_fields() {
        let service: DesiredToolServiceRegistry = serde_json::from_value(json!({
            "service_id": "observability-mcp",
            "display_name": "Observability",
            "description": null,
            "hostname": null,
            "tailscale_ip": " 100.64.0.10 ",
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": "mcp"
        }))
        .expect("desired tool service should deserialize");

        assert_eq!(service.hostname.as_deref(), Some(""));
        assert_eq!(service.tailscale_ip.as_deref(), Some("100.64.0.10"));
        assert_eq!(service.lan_ip.as_deref(), Some(""));
        assert_eq!(service.mcp_path.as_deref(), Some("/mcp"));
    }

    #[test]
    fn live_tool_service_registry_preserves_null_storage_for_diff() {
        let service = tool_service_registry_from_live_value(&json!({
            "service_id": "observability-mcp",
            "hostname": null,
            "tailscale_ip": null,
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": null
        }))
        .expect("live tool service should parse");

        assert_eq!(service.hostname, None);
        assert_eq!(service.tailscale_ip, None);
        assert_eq!(service.lan_ip, None);
        assert_eq!(service.mcp_path, None);
    }

    #[test]
    fn diff_marks_live_null_tool_service_storage_for_update() {
        let desired: DesiredToolServiceRegistry = serde_json::from_value(json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "mcp_port": 9201
        }))
        .expect("desired tool service should deserialize");
        let live = tool_service_registry_from_live_value(&json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "tailscale_ip": null,
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": null
        }))
        .expect("live tool service should parse");

        let diff = diff_collection(
            vec![(desired.service_id.clone(), &desired)],
            vec![(live.service_id.clone(), &live)],
        );

        assert_eq!(diff.update, vec!["observability-mcp"]);
        assert!(diff.unchanged.is_empty());
    }

    #[test]
    fn deprecated_backend_capability_fields_are_ignored_for_diff_equality() {
        let with_deprecated: DesiredInferenceBackend = serde_json::from_value(json!({
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "supports_tool_calls": false,
            "supports_streaming": false,
            "supports_structured_outputs": true,
            "supports_json_schema": true,
            "context_window": 32768,
            "max_output_tokens": 4096,
            "models": ["test-model"]
        }))
        .expect("deprecated fields should deserialize");

        let current: DesiredInferenceBackend = serde_json::from_value(json!({
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "models": ["test-model"]
        }))
        .expect("current fields should deserialize");

        assert_eq!(with_deprecated, current);
        assert_eq!(
            serde_json::to_value(with_deprecated).unwrap(),
            serde_json::to_value(current).unwrap()
        );
    }

    #[test]
    fn validate_manifest_accepts_deprecated_backend_capability_fields() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();

        fs::write(
            root.join(AGENT_PRINCIPAL_FILE),
            r#"{
                "agent_did": "did:defra-agent:test",
                "display_name": "Test",
                "default_behavior_id": "did:defra-agent:test:default",
                "enabled": true
            }"#,
        )
        .unwrap();
        fs::write(
            root.join(AGENT_BEHAVIORS_FILE),
            r#"[{
                "behavior_id": "did:defra-agent:test:default",
                "agent_did": "did:defra-agent:test",
                "display_name": "Default",
                "system_prompt": null,
                "backend_id": "local",
                "model_name": "test-model",
                "tool_selection_id": "tools",
                "inference_profile_id": null,
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }]"#,
        )
        .unwrap();
        fs::write(
            root.join(TOOL_SELECTIONS_FILE),
            r#"[{
                "selection_id": "tools",
                "agent_did": "did:defra-agent:test",
                "display_name": "Tools",
                "enable_file_tools": false,
                "file_tools_mode": "ReadOnly",
                "file_tool_root": null,
                "enable_bash": false,
                "bash_mode": "Off",
                "cli_tool_names": [],
                "enable_meta_tools": true,
                "delegate_to": []
            }]"#,
        )
        .unwrap();
        fs::write(
            root.join(INFERENCE_BACKENDS_FILE),
            r#"[{
                "backend_id": "local",
                "name": "Local",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:11434/v1",
                "api_key": null,
                "api_key_env_var": null,
                "max_concurrent": 1,
                "max_queue_depth": 100,
                "enabled": true,
                "supports_tool_calls": true,
                "supports_streaming": true,
                "supports_structured_outputs": false,
                "supports_json_schema": false,
                "models": ["test-model"]
            }]"#,
        )
        .unwrap();

        let report = validate_manifest_root(root);
        assert!(
            report.ok,
            "expected valid manifest, got {:?}",
            report.errors
        );
    }
}
