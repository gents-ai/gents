//! Principal-scoped self-configuration through canonical context and inference documents.
//! Patches name explicit writable fields and commit through the common desired-state
//! transaction after validating the complete retained configuration. Nested tool
//! permissions and auth references retain their existing typed owners. Raw API keys
//! cannot be changed or returned. Optional no-lockout checks the candidate config
//! chain. Persona requests reuse the existing signed admission and reconciliation path.

mod ops;
mod read;
#[cfg(test)]
mod tests;

pub use ops::{
    apply_tool_grant_selection, validate_tool_network_selection, PatchOutcome, SelfConfigCore,
    EFFECT_TIMING_NOTE,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::agent::p2p_reconcile::{GraphqlPersonaRequestStore, PersonaRequestStore};
use crate::agent::persona_ops::{
    decide_persona_request, derive_behavior_id, local_persona_request_mutation, PersonaOp,
    PersonaRequestDoc, PersonaVerdict,
};
use crate::config_client::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::graphql::escape_graphql_string;
use crate::llm::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_surface::SelfConfigToolConfig;
use crate::AgentIdentity;
use defra_node::EmbeddedNode;
use gents_protocol::persona::{LocalPersonaRequestRecord, PERSONA_AUTHORITY_LOCAL_SELF};
use ops::{decode_merged, guard_selection_keeps_gate, validate_merged_selection, ApplyRequest};

pub const GET_MY_CONFIG_TOOL_NAME: &str = "get_my_config";
pub const CONFIGURE_BEHAVIOR_TOOL_NAME: &str = "configure_behavior";
pub const CONFIGURE_TOOLS_TOOL_NAME: &str = "configure_tools";
pub const CONFIGURE_PROFILE_TOOL_NAME: &str = "configure_profile";
pub const CONFIGURE_BACKEND_TOOL_NAME: &str = "configure_backend";
pub const CONFIGURE_MCP_SERVICE_TOOL_NAME: &str = "configure_mcp_service";
pub const CONFIGURE_AUTOMATION_TOOL_NAME: &str = "configure_automation";
/// Model-facing behavior catalog/mutation surface. `PersonaConfigRequest`
/// remains the private signed transport used by paired clients and is not a
/// second runtime configuration model.
pub const CONFIGURE_BEHAVIORS_TOOL_NAME: &str = "configure_behaviors";
pub const INSTALL_PACK_TOOL_NAME: &str = "install_pack";
pub const LIST_GRAPHS_TOOL_NAME: &str = "list_graphs";
pub const RUN_GRAPH_TOOL_NAME: &str = "run_graph";
pub const GET_GRAPH_RUN_TOOL_NAME: &str = "get_graph_run";
pub const GET_GRAPH_RESULT_TOOL_NAME: &str = "get_graph_result";
pub const CANCEL_GRAPH_RUN_TOOL_NAME: &str = "cancel_graph_run";

/// Every tool name of the family, for reserved-name checks and surfacing.
pub const SELF_CONFIG_TOOL_NAMES: [&str; 14] = [
    GET_MY_CONFIG_TOOL_NAME,
    CONFIGURE_BEHAVIOR_TOOL_NAME,
    CONFIGURE_TOOLS_TOOL_NAME,
    CONFIGURE_PROFILE_TOOL_NAME,
    CONFIGURE_BACKEND_TOOL_NAME,
    CONFIGURE_MCP_SERVICE_TOOL_NAME,
    CONFIGURE_AUTOMATION_TOOL_NAME,
    CONFIGURE_BEHAVIORS_TOOL_NAME,
    INSTALL_PACK_TOOL_NAME,
    LIST_GRAPHS_TOOL_NAME,
    RUN_GRAPH_TOOL_NAME,
    GET_GRAPH_RUN_TOOL_NAME,
    GET_GRAPH_RESULT_TOOL_NAME,
    CANCEL_GRAPH_RUN_TOOL_NAME,
];

/// The `configure_*` tool advertised for a category, if any.
pub fn configure_tool_name_for_category(category: &str) -> Option<&'static str> {
    match category {
        "behavior" => Some(CONFIGURE_BEHAVIOR_TOOL_NAME),
        "tools" => Some(CONFIGURE_TOOLS_TOOL_NAME),
        "profile" => Some(CONFIGURE_PROFILE_TOOL_NAME),
        "backend" => Some(CONFIGURE_BACKEND_TOOL_NAME),
        "mcp_service" => Some(CONFIGURE_MCP_SERVICE_TOOL_NAME),
        "automation" => Some(CONFIGURE_AUTOMATION_TOOL_NAME),
        "persona" => Some(CONFIGURE_BEHAVIORS_TOOL_NAME),
        _ => None,
    }
}

/// Error wrapper mirroring `DefraQueryError`: render the full anyhow chain to
/// the model.
#[derive(Debug)]
pub struct SelfConfigError(anyhow::Error);

impl std::fmt::Display for SelfConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for SelfConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for SelfConfigError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

/// A category patch as the model supplies it: writable field → new value,
/// JSON `null` clears the field.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct PatchArg(pub Map<String, Value>);

impl PatchArg {
    fn into_patch(self) -> SelfConfigPatch {
        self.0
            .into_iter()
            .map(|(field, value)| match value {
                Value::Null => (field, None),
                other => (field, Some(other)),
            })
            .collect()
    }
}

fn patch_parameter_schema(target: SelfConfigTarget) -> Value {
    json!({
        "type": "object",
        "description": format!(
            "Partial update for the {} document: map of writable field to its new \
             value; JSON null clears a field. Writable fields: {}. All other \
             fields (identity keys, owner DID, runtime-owned status, secrets) \
             are protected and rejected.",
            target.collection_name(),
            target.writable_fields().join(", "),
        ),
        "additionalProperties": true,
    })
}

fn outcome_text(outcome: &PatchOutcome) -> Result<String> {
    serde_json::to_string_pretty(outcome).map_err(|error| anyhow!("serialize outcome: {error}"))
}

// ---------------------------------------------------------------------------
// Category request builders: each returns the ApplyRequest the core drives.
// ---------------------------------------------------------------------------

fn anchored_request(
    target: SelfConfigTarget,
    field: &'static str,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(target, patch);
    request.resolve_unique = Box::new(move |anchor| {
        anchor
            .ref_id(field)
            .ok_or_else(|| anyhow!("bound {field} is missing"))
    });
    request
}
fn behavior_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let id = core.behavior_id().to_owned();
    let mut request = ApplyRequest::new(SelfConfigTarget::AgentBehavior, patch);
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.guard = Box::new(|_, merged| {
        anyhow::ensure!(
            merged.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout guard: behavior must remain enabled"
        );
        Ok(())
    });
    request
}
fn tools_request(
    _core: &SelfConfigCore,
    patch: SelfConfigPatch,
    allow_pack_install: bool,
) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    request.validate = Box::new(move |_, _, _, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            validate_merged_selection(&merged)?;
            if !allow_pack_install {
                let grants_pack_install = merged
                    .get("self_config")
                    .and_then(Value::as_object)
                    .and_then(|config| config.get("enable_pack_install"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                anyhow::ensure!(
                    !grants_pack_install,
                    "pack installation is operator-managed and cannot be self-granted"
                );
            }
            Ok(())
        })
    });
    request.guard = Box::new(|_, merged| guard_selection_keeps_gate(merged));
    request
}
fn profile_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    anchored_request(
        SelfConfigTarget::InferenceProfile,
        "inference_profile_id",
        patch,
    )
}
fn profile_target_request(
    target: Option<&str>,
    patch: SelfConfigPatch,
) -> Result<ApplyRequest<'static>> {
    Ok(match target.unwrap_or("profile") {
        "profile" => profile_request(patch),
        "sampling" => anchored_request(SelfConfigTarget::InferenceSampling, "sampling_id", patch),
        "execution" => {
            anchored_request(SelfConfigTarget::InferenceExecution, "execution_id", patch)
        }
        "retry_policy" => anchored_request(
            SelfConfigTarget::InferenceRetryPolicy,
            "retry_policy_id",
            patch,
        ),
        "compaction" => anchored_request(SelfConfigTarget::Compaction, "compaction_id", patch),
        other => bail!("unknown profile target {other:?}"),
    })
}
fn backend_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::InferenceBackend, "backend_id", patch);
    request.validate = Box::new(move |_, _, stored, merged| {
        let merged = merged.clone();
        let stored = stored.clone();
        Box::pin(async move {
            let backend: crate::InferenceBackend = decode_merged("InferenceBackend", &merged)?;
            if matches!(
                backend.auth,
                crate::document_config::BackendAuth::ApiKey { .. }
            ) {
                let previous: crate::InferenceBackend = decode_merged("InferenceBackend", &stored)?;
                anyhow::ensure!(
                    backend.auth == previous.auth,
                    "raw API keys are operator-managed; select an environment or OAuth reference"
                );
            }
            backend.validate()
        })
    });
    request.guard = Box::new(|_, merged| {
        anyhow::ensure!(
            merged.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout guard: backend must remain enabled"
        );
        Ok(())
    });
    request
}
fn mcp_service_request(service_id: String, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::ToolServiceRegistry, patch);
    request.resolve_unique = Box::new(move |_| Ok(service_id.clone()));
    request
}
fn automation_request(
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let owner = core.agent_did().to_owned();
    let behavior = core.behavior_id().to_owned();
    let mut request = ApplyRequest::new(target, patch);
    request.allow_create = true;
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert(target.unique_field().into(), json!(id));
        merged.insert("agent_did".into(), json!(owner));
        if target == SelfConfigTarget::Task {
            merged.insert("behavior_id".into(), json!(behavior));
        }
        Ok(())
    });
    let core = core.clone();
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let core = core.clone();
        let stored = stored.clone();
        let merged = merged.clone();
        Box::pin(async move {
            if target == SelfConfigTarget::Task {
                anyhow::ensure!(
                    merged.get("behavior_id").and_then(Value::as_str) == Some(core.behavior_id()),
                    "task belongs to another behavior"
                );
            }
            if target == SelfConfigTarget::Trigger {
                for doc in [&stored, &merged].into_iter().filter(|doc| !doc.is_empty()) {
                    let task = doc
                        .get("task_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("trigger task is required"))?;
                    anyhow::ensure!(
                        core.task_owned(txn, anchor, task).await?,
                        "trigger task belongs to another behavior"
                    );
                }
            }
            Ok(())
        })
    });
    request
}
fn automation_target(kind: &str) -> Result<SelfConfigTarget> {
    match kind {
        "task" => Ok(SelfConfigTarget::Task),
        "schedule" => Ok(SelfConfigTarget::Schedule),
        "trigger" => Ok(SelfConfigTarget::Trigger),
        "event_source" => Ok(SelfConfigTarget::EventSource),
        _ => {
            bail!("unknown automation kind {kind:?}; use task, schedule, trigger, or event_source")
        }
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

pub struct GetMyConfigTool {
    core: SelfConfigCore,
    categories: BTreeSet<String>,
    no_lockout: bool,
    dry_run: bool,
    allow_pack_install: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetMyConfigParams {
    /// Dry-run preview (requires `self_config_dry_run`): the diff a patch
    /// would produce, without committing.
    #[serde(default)]
    pub preview: Option<PreviewParams>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewParams {
    pub category: String,
    /// Target id for categories that need one (`mcp_service` service_id,
    /// `automation` task/schedule/trigger id).
    #[serde(default)]
    pub id: Option<String>,
    /// Automation kind (`task` | `schedule` | `trigger`).
    #[serde(default)]
    pub kind: Option<String>,
    pub patch: PatchArg,
}

impl Tool for GetMyConfigTool {
    const NAME: &'static str = GET_MY_CONFIG_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = GetMyConfigParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = serde_json::Map::new();
        if self.dry_run {
            properties.insert(
                "preview".to_string(),
                json!({
                    "type": "object",
                    "description": "Optional dry-run: preview the field-level diff a configure_* patch would produce, without committing.",
                    "properties": {
                        "category": {
                            "type": "string",
                            "enum": self.categories.iter().collect::<Vec<_>>(),
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["behavior", "context", "profile", "sampling", "execution", "retry_policy", "compaction", "task", "schedule", "trigger", "event_source"],
                            "description": "Target within the selected behavior, profile, or automation category.",
                        },
                        "id": {
                            "type": "string",
                            "description": "Target id (mcp_service service_id or automation doc id).",
                        },
                        "patch": { "type": "object" },
                    },
                    "required": ["category", "patch"],
                }),
            );
        }
        ToolDefinition {
            name: GET_MY_CONFIG_TOOL_NAME.to_string(),
            description: format!(
                "Read this agent's own effective configuration documents: behavior, tool \
                 selection, inference profile, backend (secrets excluded), owned skills and \
                 automation. Enabled self-config categories: {}.{} {}",
                self.categories
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if self.dry_run {
                    " Supports dry-run patch previews via the preview parameter."
                } else {
                    ""
                },
                EFFECT_TIMING_NOTE,
            ),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": [],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.preview {
            None => {
                let config = self
                    .core
                    .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
                    .await?;
                serde_json::to_string_pretty(&config)
                    .map_err(|error| SelfConfigError(anyhow!("serialize config: {error}")))
            }
            Some(preview) => {
                if !self.dry_run {
                    return Err(SelfConfigError(anyhow!(
                        "dry-run preview is not enabled for this behavior \
                         (Tools.self_config_dry_run)"
                    )));
                }
                if !self.categories.contains(&preview.category) {
                    return Err(SelfConfigError(anyhow!(
                        "category {:?} is not enabled for self-config (enabled: {})",
                        preview.category,
                        self.categories
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    )));
                }
                let patch = preview.patch.into_patch();
                let request = match preview.category.as_str() {
                    "behavior" => match preview.kind.as_deref().unwrap_or("behavior") {
                        "behavior" => behavior_request(&self.core, patch),
                        "context" => {
                            anchored_request(SelfConfigTarget::AgentContext, "context_id", patch)
                        }
                        other => return Err(anyhow!("unknown behavior target {other:?}").into()),
                    },
                    "tools" => tools_request(&self.core, patch, self.allow_pack_install),
                    "profile" => profile_target_request(preview.kind.as_deref(), patch)?,
                    "backend" => backend_request(patch),
                    "mcp_service" => {
                        let id = preview.id.ok_or_else(|| {
                            SelfConfigError(anyhow!("preview.id (service_id) is required"))
                        })?;
                        mcp_service_request(id, patch)
                    }
                    "automation" => {
                        let kind = preview.kind.as_deref().ok_or_else(|| {
                            SelfConfigError(anyhow!("preview.kind is required for automation"))
                        })?;
                        let id = preview.id.ok_or_else(|| {
                            SelfConfigError(anyhow!("preview.id is required for automation"))
                        })?;
                        automation_request(&self.core, automation_target(kind)?, id, patch)
                    }
                    "persona" => {
                        return Err(SelfConfigError(anyhow!(
                            "behavior catalog actions are request-based; no patch preview is available \
                             — call configure_behaviors with action \"preview\""
                        )));
                    }
                    other => {
                        return Err(SelfConfigError(anyhow!("unknown category {other:?}")));
                    }
                };
                let outcome = self.core.preview(request).await?;
                Ok(outcome_text(&outcome)?)
            }
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchOnlyParams {
    #[serde(default)]
    pub target: Option<String>,
    pub patch: PatchArg,
}

pub struct ConfigureBehaviorTool {
    core: SelfConfigCore,
}

impl Tool for ConfigureBehaviorTool {
    const NAME: &'static str = CONFIGURE_BEHAVIOR_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = PatchOnlyParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_BEHAVIOR_TOOL_NAME.to_string(),
            description: format!(
                "Patch the bound behavior or context document. Context owns prompt, tools, skills and compaction references. Identity fields are immutable. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "target": {"type":"string","enum":["behavior","context"],"default":"behavior"}, "patch": {"type":"object","description":"Writable fields of the selected canonical behavior or context document."} },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let request = match args.target.as_deref().unwrap_or("behavior") {
            "behavior" => behavior_request(&self.core, args.patch.into_patch()),
            "context" => anchored_request(
                SelfConfigTarget::AgentContext,
                "context_id",
                args.patch.into_patch(),
            ),
            other => return Err(anyhow!("unknown behavior target {other:?}").into()),
        };
        let outcome = self.core.apply(request).await?;
        Ok(outcome_text(&outcome)?)
    }
}

pub struct ConfigureToolsTool {
    core: SelfConfigCore,
    allow_pack_install: bool,
}

impl Tool for ConfigureToolsTool {
    const NAME: &'static str = CONFIGURE_TOOLS_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = PatchOnlyParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_TOOLS_TOOL_NAME.to_string(),
            description: format!(
                "Patch the bound Tools document using its canonical nested groups and explicit permissions. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::Tools) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.target.is_some() {
            return Err(anyhow!("configure_tools has no target selector").into());
        }
        let request = tools_request(&self.core, args.patch.into_patch(), self.allow_pack_install);
        let outcome = self.core.apply(request).await?;
        Ok(outcome_text(&outcome)?)
    }
}

pub struct ConfigureProfileTool {
    core: SelfConfigCore,
}

impl Tool for ConfigureProfileTool {
    const NAME: &'static str = CONFIGURE_PROFILE_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = PatchOnlyParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_PROFILE_TOOL_NAME.to_string(),
            description: format!(
                "Patch the selected bound inference or compaction document. Shared references within this principal observe the committed change. \
                 {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "target": {"type":"string","enum":["profile","sampling","execution","retry_policy","compaction"],"default":"profile"}, "patch": {"type":"object","description":"Writable fields of the selected canonical inference or compaction document."} },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let outcome = self
            .core
            .apply(profile_target_request(
                args.target.as_deref(),
                args.patch.into_patch(),
            )?)
            .await?;
        Ok(outcome_text(&outcome)?)
    }
}

pub struct ConfigureBackendTool {
    core: SelfConfigCore,
}

impl Tool for ConfigureBackendTool {
    const NAME: &'static str = CONFIGURE_BACKEND_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = PatchOnlyParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_BACKEND_TOOL_NAME.to_string(),
            description: format!("Patch the backend selected by the bound inference profile. Endpoint, auth references, discovery limits and concurrency are configurable; raw keys and observations remain protected. {EFFECT_TIMING_NOTE}"),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::InferenceBackend) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.target.is_some() {
            return Err(anyhow!("configure_backend has no target selector").into());
        }
        let outcome = self
            .core
            .apply(backend_request(args.patch.into_patch()))
            .await?;
        Ok(outcome_text(&outcome)?)
    }
}

pub struct ConfigureMcpServiceTool {
    core: SelfConfigCore,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureMcpServiceParams {
    pub service_id: String,
    pub patch: PatchArg,
}

impl Tool for ConfigureMcpServiceTool {
    const NAME: &'static str = CONFIGURE_MCP_SERVICE_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigureMcpServiceParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_MCP_SERVICE_TOOL_NAME.to_string(),
            description: format!(
                "Patch a ToolServiceRegistry document (MCP service host/port/path, \
                 send_agent_did, status). The service must already exist; registry \
                 version/updated_at are runtime-owned. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "service_id": { "type": "string" },
                    "patch": patch_parameter_schema(SelfConfigTarget::ToolServiceRegistry),
                },
                "required": ["service_id", "patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let request = mcp_service_request(args.service_id, args.patch.into_patch());
        let outcome = self.core.apply(request).await?;
        Ok(outcome_text(&outcome)?)
    }
}

pub struct ConfigureAutomationTool {
    core: SelfConfigCore,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureAutomationParams {
    /// `task` | `schedule` | `trigger`.
    pub kind: String,
    /// The document's unique id (created if absent).
    pub id: String,
    pub patch: PatchArg,
}

impl Tool for ConfigureAutomationTool {
    const NAME: &'static str = CONFIGURE_AUTOMATION_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigureAutomationParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_AUTOMATION_TOOL_NAME.to_string(),
            description: format!("Create or patch Task, Trigger, Schedule, or EventSource documents owned by this principal. Tasks and trigger task links are scoped to this behavior. Schedule owns cadence; Trigger owns task, source and concurrency. Runtime observations are protected. {EFFECT_TIMING_NOTE}"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["task", "schedule", "trigger", "event_source"] },
                    "id": { "type": "string" },
                    "patch": {
                        "type": "object",
                        "description": format!(
                            "Writable fields — task: {}; schedule: {}; trigger: {}.",
                            SelfConfigTarget::Task.writable_fields().join(", "),
                            SelfConfigTarget::Schedule.writable_fields().join(", "),
                            SelfConfigTarget::Trigger.writable_fields().join(", "),
                        ),
                        "additionalProperties": true,
                    },
                },
                "required": ["kind", "id", "patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let target = automation_target(&args.kind)?;
        let request = automation_request(&self.core, target, args.id, args.patch.into_patch());
        let outcome = self.core.apply(request).await?;
        Ok(outcome_text(&outcome)?)
    }
}

/// Manage SIBLING personas of this agent through the `PersonaConfigRequest`
/// channel — see the module doc for why this tool, alone in the family, is
/// not "self only" at the behavior level. Unlike the patch-based tools above,
/// this one authors a request document and lets the existing persona
/// reconciler (`crate::agent::p2p_reconcile::persona_requests`) admit and
/// materialize it, so admission can never drift between this tool, the
/// P2P-replicated path, and the `gents` CLI.
pub struct ConfigurePersonaTool {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    identity: Arc<dyn AgentIdentity>,
    process_ceiling: crate::tool_surface::SelfConfigProcessCeiling,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurePersonaParams {
    /// `list` | `inspect` | `preview` | `create` | `edit` | `clone` | `disable`.
    pub action: String,
    /// Proposed mutation for `action: "preview"`.
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// User-facing summary for the working behavior and its context.
    #[serde(default)]
    pub description: Option<String>,
    /// Complete operating instructions for the working behavior. Required
    /// when creating from a permission preset; optional overrides a clone or
    /// an existing behavior.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Exact behavior_id of the sibling behavior (required for edit/disable).
    #[serde(default)]
    pub behavior_id: Option<String>,
    /// Exact sibling behavior_id to clone; a supplied preset requests a new behavior.
    #[serde(default)]
    pub clone_from: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    /// Exact owner-scoped inference profile ID.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Promote the applied behavior to this principal's default behavior.
    #[serde(default)]
    pub make_default: bool,
    #[serde(default)]
    pub enable_lsp: Option<bool>,
    #[serde(default)]
    pub enable_graph_tools: Option<bool>,
    /// Optional fail-closed network narrowing for canonical host bash tools.
    #[serde(default)]
    pub network_mode: Option<crate::toolset::CommandNetworkMode>,
}

/// How long [`ConfigurePersonaTool`] polls a freshly-authored
/// `PersonaConfigRequest` row before returning it still-`pending`: the
/// in-process reconciler sweeps on every `Update` event, so a healthy node
/// converges well inside this window.
const PERSONA_REQUEST_POLL_TIMEOUT: Duration = Duration::from_secs(5);
const PERSONA_REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Default, serde::Serialize)]
struct PersonaCatalogSnapshot {
    process_ceiling: crate::tool_surface::SelfConfigProcessCeiling,
    allowed_roots: Vec<String>,
    permission_presets: Vec<String>,
    available_profile_ids: Vec<String>,
    default_behavior_id: Option<String>,
    behaviors: BTreeMap<String, Value>,
    activation: Value,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PersonaRequestRowOut {
    #[serde(default)]
    request_key: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    clone_from: Option<String>,
    #[serde(default)]
    persona_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    make_default: Option<bool>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    status_detail: Option<String>,
    #[serde(default)]
    applied_behavior_id: Option<String>,
    #[serde(default)]
    processed_at: Option<String>,
}

async fn load_persona_request_row(
    node: &Arc<EmbeddedNode>,
    request_key: &str,
    agent_did: &str,
) -> Result<Option<PersonaRequestRowOut>> {
    let escaped = escape_graphql_string(request_key);
    let owner = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            PersonaConfigRequest(filter: {{ agent_did: {{ _eq: "{owner}" }}, request_key: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                request_key
                requester_did
                agent_did
                op
                behavior_id
                clone_from
                persona_name
                description
                system_prompt
                root
                preset
                profile_id
                make_default
                created_at
                status
                status_detail
                applied_behavior_id
                processed_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        bail!("query PersonaConfigRequest failed: {:?}", response.errors);
    }
    let Some(value) = response
        .data
        .as_ref()
        .and_then(|data| data.get("PersonaConfigRequest"))
    else {
        return Ok(None);
    };
    let rows: Vec<PersonaRequestRowOut> =
        serde_json::from_value(value.clone()).map_err(|error| anyhow!("decode row: {error}"))?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous persona request identity");
    Ok(rows.into_iter().next())
}

/// Poll a freshly-authored row until the reconciler (which sweeps on every
/// `Update` event) drives it to a terminal status, or [`PERSONA_REQUEST_POLL_TIMEOUT`]
/// elapses. A still-pending row is returned as-is rather than an error: the
/// request is valid and will converge, the caller just needs to check again.
async fn poll_persona_request(
    node: &Arc<EmbeddedNode>,
    request_key: &str,
    agent_did: &str,
) -> Result<PersonaRequestRowOut> {
    let deadline = tokio::time::Instant::now() + PERSONA_REQUEST_POLL_TIMEOUT;
    loop {
        if let Some(row) = load_persona_request_row(node, request_key, agent_did).await? {
            if row.status.as_deref() != Some("pending") {
                return Ok(row);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(PersonaRequestRowOut {
                request_key: Some(request_key.to_owned()),
                status: Some("pending".to_owned()),
                status_detail: Some(
                    "still pending after 5s; the reconciler may need another moment — inspect the request again or list behaviors to determine whether materialization completed"
                        .to_owned(),
                ),
                ..Default::default()
            });
        }
        tokio::time::sleep(PERSONA_REQUEST_POLL_INTERVAL).await;
    }
}

async fn principal_default_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<String>> {
    let owner = escape_graphql_string(agent_did);
    let response = node
        .execute(&format!(
            r#"{{ AgentPrincipal(filter: {{agent_did: {{_eq: "{owner}"}}}}, limit: 2) {{ default_behavior_id }} }}"#
        ))
        .await;
    if response.has_errors() {
        bail!("query AgentPrincipal failed: {:?}", response.errors);
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentPrincipal"))
        .and_then(Value::as_array)
        .context("AgentPrincipal query returned no rows array")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous principal identity");
    Ok(rows
        .first()
        .and_then(|row| row.get("default_behavior_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn behavior_snapshot(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    behavior_id: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    protected: bool,
    default_behavior_id: Option<&str>,
) -> Result<Value> {
    let effective =
        SelfConfigCore::new(node.clone(), agent_did.to_owned(), behavior_id.to_owned())?
            .with_process_ceiling(process_ceiling.clone())
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
    Ok(json!({
        "behavior_id": behavior_id,
        "protected": protected,
        "is_default": default_behavior_id == Some(behavior_id),
        "effective_config": effective,
    }))
}

async fn persona_list(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let store =
        GraphqlPersonaRequestStore::with_ceiling(node.clone(), process_ceiling.root.clone());
    let catalog = store.load_catalog_view(agent_did).await?;
    let default_behavior_id = principal_default_behavior(node, agent_did).await?;
    let mut behaviors = BTreeMap::new();
    for (behavior_id, reference) in &catalog.behaviors {
        let snapshot = match behavior_snapshot(
            node,
            agent_did,
            behavior_id,
            process_ceiling,
            reference.protected,
            default_behavior_id.as_deref(),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => json!({
                "behavior_id": behavior_id,
                "enabled": reference.enabled,
                "protected": reference.protected,
                "is_default": default_behavior_id.as_deref() == Some(behavior_id.as_str()),
                "configuration_error": format!("{error:#}"),
            }),
        };
        behaviors.insert(behavior_id.clone(), snapshot);
    }
    let snapshot = PersonaCatalogSnapshot {
        process_ceiling: process_ceiling.clone(),
        allowed_roots: catalog.allowed_roots.into_iter().collect(),
        permission_presets: crate::agent::persona_presets::builtin_preset_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        available_profile_ids: catalog.available_profile_ids.into_iter().collect(),
        default_behavior_id,
        behaviors,
        activation: json!({
            "durable_config": "after the admitted transaction commits",
            "running_generation": "after the runtime reconciler generation swap",
            "session": "select the behavior in a new session; existing sessions remain bound to their original behavior",
            "pairing_and_restart": "canonical documents and request outcomes replicate to authorized paired clients and are reloaded after restart",
        }),
    };
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

async fn persona_inspect(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    behavior_id: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let store =
        GraphqlPersonaRequestStore::with_ceiling(node.clone(), process_ceiling.root.clone());
    let catalog = store.load_catalog_view(agent_did).await?;
    let reference = catalog.behaviors.get(behavior_id).with_context(|| {
        format!(
            "unknown behavior_id {behavior_id:?}; call configure_behaviors with action \"list\""
        )
    })?;
    let default_behavior_id = principal_default_behavior(node, agent_did).await?;
    let snapshot = behavior_snapshot(
        node,
        agent_did,
        behavior_id,
        process_ceiling,
        reference.protected,
        default_behavior_id.as_deref(),
    )
    .await?;
    serde_json::to_string_pretty(&snapshot)
        .map_err(|error| anyhow!("serialize behavior inspection: {error}"))
}

async fn persona_preview(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    args: &ConfigurePersonaParams,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let operation = args
        .operation
        .as_deref()
        .context("preview requires operation: create|edit|clone|disable")?;
    let (op_raw, op, clone_from) = match operation {
        "create" => ("create", PersonaOp::Create { clone_from: None }, None),
        "clone" => {
            let source = args
                .clone_from
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .context("preview clone requires clone_from")?
                .to_owned();
            (
                "create",
                PersonaOp::Create {
                    clone_from: Some(source.clone()),
                },
                Some(source),
            )
        }
        "edit" => ("edit", PersonaOp::Edit, None),
        "disable" => ("disable", PersonaOp::Disable, None),
        other => bail!("unknown preview operation {other:?}; use create|edit|clone|disable"),
    };
    let store =
        GraphqlPersonaRequestStore::with_ceiling(node.clone(), process_ceiling.root.clone());
    let catalog = store.load_catalog_view(agent_did).await?;
    let request_key = format!("preview-{}", uuid::Uuid::new_v4());
    let doc = PersonaRequestDoc {
        request_key: request_key.clone(),
        requester_did: agent_did.to_owned(),
        agent_did: agent_did.to_owned(),
        authority_kind: PERSONA_AUTHORITY_LOCAL_SELF.to_owned(),
        local_signer_did: agent_did.to_owned(),
        local_signature_valid: true,
        op_raw: op_raw.to_owned(),
        op: Some(op.clone()),
        behavior_id: args.behavior_id.clone(),
        persona_name: args.display_name.clone(),
        description: args.description.clone(),
        system_prompt: args.system_prompt.clone(),
        root: args.root.clone(),
        preset: args.preset.clone(),
        profile_id: args.profile_id.clone(),
        make_default: args.make_default,
        ..Default::default()
    };
    let verdict = decide_persona_request(&doc, &catalog);
    let rejection = match &verdict {
        PersonaVerdict::Admit => None,
        PersonaVerdict::Reject(detail) => Some(detail.clone()),
    };
    let behavior_id = match &op {
        PersonaOp::Create { .. } => args
            .display_name
            .as_deref()
            .map(|name| derive_behavior_id(agent_did, name, &catalog.behaviors)),
        PersonaOp::Edit | PersonaOp::Disable => args.behavior_id.clone(),
    };
    let inherited = match operation {
        "clone" => clone_from.as_deref(),
        "edit" | "disable" => args.behavior_id.as_deref(),
        _ => None,
    };
    let inherited_config = match inherited {
        Some(behavior_id) if catalog.behaviors.contains_key(behavior_id) => {
            let reference = &catalog.behaviors[behavior_id];
            let default_behavior_id = principal_default_behavior(node, agent_did).await?;
            Some(
                behavior_snapshot(
                    node,
                    agent_did,
                    behavior_id,
                    process_ceiling,
                    reference.protected,
                    default_behavior_id.as_deref(),
                )
                .await?,
            )
        }
        _ => None,
    };
    let preset_requested = args.preset.as_deref().and_then(|preset| {
        crate::agent::persona_presets::preset_fields(preset).map(|fields| {
            json!({
                "file_mode": fields.file_tools_mode,
                "bash_mode": fields.bash_mode,
                "self_configuration": fields.enable_self_config,
            })
        })
    });
    serde_json::to_string_pretty(&json!({
        "committed": false,
        "admitted": matches!(verdict, PersonaVerdict::Admit),
        "rejection": rejection,
        "operation": operation,
        "proposed_ids": {
            "behavior_id": behavior_id,
            "context_id": matches!(&op, PersonaOp::Create { .. }).then(|| format!("context-{request_key}")),
            "tools_id": matches!(&op, PersonaOp::Create { .. }).then(|| format!("tools-{request_key}")),
            "profile_id": args.profile_id,
        },
        "proposed_values": {
            "display_name": args.display_name,
            "description": args.description,
            "context_description": args.description,
            "system_prompt": args.system_prompt,
            "root": args.root,
            "preset": args.preset,
            "make_default": args.make_default,
        },
        "preset_requested": preset_requested,
        "inherited_config": inherited_config,
        "process_ceiling": process_ceiling,
        "note": "Preview checks request admission without writing; it does not verify materialization or runtime readiness. Preset values are requested authority, narrowed by the process ceiling at runtime. Inspect the applied behavior for effective authority. Applied create IDs use the admitted request key and will differ from these preview-only IDs.",
    }))
    .map_err(|error| anyhow!("serialize behavior preview: {error}"))
}

async fn persona_mutate(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    identity: &dyn AgentIdentity,
    args: &ConfigurePersonaParams,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let resolved_behavior_id = args.behavior_id.as_deref().map(str::to_owned);
    let resolved_profile_id = args.profile_id.as_deref().map(str::to_owned);

    let required_behavior_id = |action: &str| -> Result<()> {
        if resolved_behavior_id
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            bail!("{action} action requires behavior_id (the sibling behavior to target)");
        }
        Ok(())
    };
    let (op, clone_from): (&str, Option<String>) = match args.action.as_str() {
        "create" => ("create", None),
        "clone" => {
            let preset_given = args
                .preset
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
            anyhow::ensure!(
                !preset_given,
                "clone action inherits its source permissions and must not include preset"
            );
            let clone_from = args
                .clone_from
                .as_deref()
                .map(str::to_owned)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow!("clone action requires clone_from (the sibling behavior_id to clone)")
                })?;
            ("create", Some(clone_from))
        }
        "edit" => {
            required_behavior_id("edit")?;
            ("edit", None)
        }
        "disable" => {
            required_behavior_id("disable")?;
            ("disable", None)
        }
        other => bail!("unknown action {other:?}; use list|create|edit|clone|disable"),
    };

    let request_key = format!("pcr-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut record = LocalPersonaRequestRecord {
        request_key: request_key.clone(),
        requester_did: agent_did.to_string(),
        agent_did: agent_did.to_string(),
        authority_kind: PERSONA_AUTHORITY_LOCAL_SELF.to_string(),
        local_signer_did: agent_did.to_string(),
        op: op.to_string(),
        behavior_id: resolved_behavior_id,
        clone_from,
        persona_name: args.display_name.clone(),
        description: args.description.clone(),
        system_prompt: args.system_prompt.clone(),
        root: args.root.clone(),
        preset: args.preset.clone(),
        profile_id: resolved_profile_id,
        make_default: args.make_default,
        created_at: now,
        local_signature: Vec::new(),
    };
    record.local_signature = identity.sign(&record.signing_payload()).await?;
    record.validate_shape()?;
    let mutation = local_persona_request_mutation(&record);
    crate::config_client::ConfigAccess::write_local(
        node,
        "self_config.create_persona_request",
        &mutation,
    )
    .await?;

    let row = poll_persona_request(node, &request_key, agent_did).await?;
    let mut output = json!({"request": row});
    if row.status.as_deref() == Some("applied") {
        let applied_behavior_id = row
            .applied_behavior_id
            .as_deref()
            .context("applied behavior request has no applied_behavior_id")?;
        if args.action == "disable" {
            let behavior = crate::list_agent_behaviors(node, agent_did)
                .await?
                .into_iter()
                .find(|behavior| behavior.behavior_id == applied_behavior_id)
                .context("applied disable outcome cannot re-read its behavior")?;
            anyhow::ensure!(
                !behavior.enabled,
                "applied disable outcome still resolves the behavior as enabled"
            );
            output["effective"] = json!({
                "behavior": behavior,
                "is_default": principal_default_behavior(node, agent_did).await?.as_deref()
                    == Some(applied_behavior_id),
            });
            output["activation"] = json!({
                "durable": "confirmed",
                "runtime": "new requests cannot select the disabled behavior after reconciliation",
                "current_turn": "unchanged",
            });
            return serde_json::to_string_pretty(&output)
                .map_err(|error| anyhow!("serialize behavior disable outcome: {error}"));
        }
        let effective: Value = serde_json::from_str(
            &persona_inspect(node, agent_did, applied_behavior_id, process_ceiling).await?,
        )?;
        let effective_config = &effective["effective_config"];
        let required_materialized_id = |pointer: &str, name: &str| -> Result<String> {
            effective_config
                .pointer(pointer)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .with_context(|| {
                    format!(
                        "applied behavior request reported success without a materialized {name}"
                    )
                })
        };
        let context_id = required_materialized_id("/context/context_id", "context_id")?;
        let profile_id = required_materialized_id("/inference_profile/profile_id", "profile_id")?;
        let requires_tools = args
            .preset
            .as_deref()
            .is_some_and(|preset| !preset.trim().is_empty())
            || args
                .root
                .as_deref()
                .is_some_and(|root| !root.trim().is_empty());
        let tools_id = if !requires_tools {
            effective_config
                .pointer("/documents/Tools/tools_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        } else {
            Some(required_materialized_id(
                "/documents/Tools/tools_id",
                "tools_id",
            )?)
        };
        if args.action == "create" {
            anyhow::ensure!(
                effective_config
                    .pointer("/context/system_prompt")
                    .and_then(Value::as_str)
                    .is_some_and(|prompt| !prompt.trim().is_empty()),
                "applied behavior request reported success without effective system instructions"
            );
        }
        output["materialized_ids"] = json!({
            "behavior_id": applied_behavior_id,
            "context_id": context_id,
            "tools_id": tools_id,
            "profile_id": profile_id,
        });
        output["effective"] = effective;
        output["activation"] = json!({
            "durable": "confirmed",
            "runtime": "applies to requests dispatched after the reconciler publishes the new generation",
            "current_turn": "unchanged",
            "test": "start a new session explicitly selecting applied_behavior_id",
            "restart_and_pairing": "durable canonical documents and the terminal request outcome survive restart and replicate to authorized paired clients",
        });
    } else if row.status.as_deref() == Some("rejected") {
        output["recovery"] = json!({
            "guidance": row.status_detail,
            "next": "list or inspect behavior configuration and retry only with published profile/root/preset choices",
        });
    }
    serde_json::to_string_pretty(&output)
        .map_err(|error| anyhow!("serialize behavior configuration outcome: {error}"))
}

impl Tool for ConfigurePersonaTool {
    const NAME: &'static str = CONFIGURE_BEHAVIORS_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigurePersonaParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_BEHAVIORS_TOOL_NAME.to_string(),
            description: "Preview, list, inspect, create, clone, edit, or disable this principal's canonical behaviors through the signed command owner. Choose an existing profile and a complete system_prompt for a preset-based create. Then use the separate configure_tools action with an existing behavior_id to select LSP/native graph capabilities or disable host-command network access through the canonical config patch owner. Creation and tool selection are distinct commits; retry only the failed operation. Setup and shared tool references are protected. Inspect configured grants and test a new request after reconciliation before claiming readiness.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "inspect", "preview", "create", "edit", "clone", "disable", "configure_tools"],
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["create", "edit", "clone", "disable"],
                        "description": "Mutation to validate when action is preview.",
                    },
                    "display_name": {
                        "type": "string",
                        "description": "Display name for the working behavior (create/edit).",
                    },
                    "description": {
                        "type": "string",
                        "description": "Concise user-facing purpose for the working behavior and context.",
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Complete operating instructions. Required for a preset-based create; optional to override a clone or edit. Must describe the requested role, scope, tools, constraints, and verification expectations.",
                    },
                    "behavior_id": {
                        "type": "string",
                        "description": "Exact behavior_id (required for inspect/edit/disable).",
                    },
                    "clone_from": {
                        "type": "string",
                        "description": "Exact sibling behavior_id to clone from.",
                    },
                    "root": {
                        "type": "string",
                        "description": "Workspace root narrowing, if any. It must be a published allowed_root within the managed process ceiling. CAUTION on edit: omitting root clears the existing narrowing, so inspect and resend it unless widening to the process ceiling is explicitly intended.",
                    },
                    "preset": {
                        "type": "string",
                        "description": "Built-in permission preset for create/edit. Clone inherits permissions and rejects this field.",
                    },
                    "profile_id": {
                        "type": "string",
                        "description": "Exact inference profile_id owned by this principal.",
                    },
                    "make_default": {
                        "type": "boolean",
                        "description": "For create/edit, atomically promote the applied behavior to this principal's default. Must be false for disable.",
                        "default": false,
                    },
                    "enable_lsp": {
                        "type": "boolean",
                        "description": "Only for action configure_tools on an existing sibling. Select LSP in canonical Tools; omit to preserve, false disables. This is a separate committed operation after create/clone/edit, not an atomic creation option. Test the server before claiming readiness."
                    },
                    "enable_graph_tools": {
                        "type": "boolean",
                        "description": "Only for action configure_tools on an existing sibling. Select native list_graphs/run_graph/get_graph_run/get_graph_result/cancel_graph_run on this node/principal, without pack installation or self-configuration. Omit to preserve; false disables. Graph caller admission still applies."
                    },
                    "network_mode": {
                        "type": "string",
                        "enum": ["disabled"],
                        "description": "Only for action configure_tools on an existing sibling. Set disabled to narrow canonical host bash commands to no network; omit to preserve the current policy. This control cannot widen network access. Inspect the effective report and test enforcement before claiming isolation."
                    },
                },
                "required": ["action"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.action != "configure_tools"
            && (args.enable_lsp.is_some()
                || args.enable_graph_tools.is_some()
                || args.network_mode.is_some())
        {
            return Err(anyhow!("tool selections require the separate configure_tools action with an existing behavior_id; create/clone/edit remains a signed atomic behavior command").into());
        }
        match args.action.as_str() {
            "configure_tools" => {
                let behavior_id = args
                    .behavior_id
                    .as_deref()
                    .context("configure_tools requires behavior_id")?;
                let core = SelfConfigCore::new(
                    self.node.clone(),
                    self.agent_did.clone(),
                    behavior_id.into(),
                )?
                .with_process_ceiling(self.process_ceiling.clone());
                let outcome = core
                    .select_sibling_tools(
                        args.enable_lsp,
                        args.enable_graph_tools,
                        args.network_mode,
                    )
                    .await?;
                let effective = core
                    .read_effective_config(&BTreeSet::new(), false, false)
                    .await?;
                for (requested, name) in [
                    (args.enable_lsp, "lsp"),
                    (args.enable_graph_tools, "native_graph_tools"),
                ] {
                    if let Some(requested) = requested {
                        if effective["tool_grants"]["configured"][name].as_bool() != Some(requested)
                        {
                            return Err(
                                anyhow!("committed tool selection did not verify {name}").into()
                            );
                        }
                    }
                }
                if let Some(requested) = args.network_mode {
                    if effective["tool_grants"]["configured"]["network_mode"].as_str()
                        != Some(requested.as_str())
                    {
                        return Err(anyhow!(
                            "committed tool selection did not verify network_mode"
                        )
                        .into());
                    }
                }
                Ok(serde_json::to_string_pretty(&json!({
                    "outcome": outcome, "behavior_id": behavior_id,
                    "requested": {"enable_lsp": args.enable_lsp, "enable_graph_tools": args.enable_graph_tools, "network_mode": args.network_mode},
                    "effective_config": effective,
                    "effect": "Tool selection committed separately from behavior creation. Existing IDs and prompt are unchanged; test a new request after reconciliation.",
                })).context("serialize sibling tool selection")?)
            }
            "list" => Ok(persona_list(&self.node, &self.agent_did, &self.process_ceiling).await?),
            "inspect" => {
                let behavior_id = args
                    .behavior_id
                    .as_deref()
                    .context("inspect requires behavior_id")?;
                Ok(persona_inspect(
                    &self.node,
                    &self.agent_did,
                    behavior_id,
                    &self.process_ceiling,
                )
                .await?)
            }
            "preview" => {
                Ok(
                    persona_preview(&self.node, &self.agent_did, &args, &self.process_ceiling)
                        .await?,
                )
            }
            "create" | "edit" | "clone" | "disable" => Ok(persona_mutate(
                &self.node,
                &self.agent_did,
                self.identity.as_ref(),
                &args,
                &self.process_ceiling,
            )
            .await?),
            other => Err(SelfConfigError(anyhow!(
                "unknown action {other:?}; use list|inspect|preview|create|edit|clone|disable"
            ))),
        }
    }
}

/// Install bundled graph packs through the canonical package and activation
/// owners. The running principal is always the install owner; callers cannot
/// select another DID, filesystem distribution, or control-plane endpoint.
pub struct InstallPackTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPackParams {
    /// Bundled graph pack name, such as `code_review`.
    pub package: String,
    /// Optional interpolation overrides declared by the selected pack. Setup's
    /// current inference model and endpoint supply `*_MODEL`/`*_ENDPOINT` by
    /// default, without changing process-global environment variables.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

impl InstallPackTool {
    async fn install(&self, args: InstallPackParams) -> anyhow::Result<String> {
        let package_name = args.package.trim();
        anyhow::ensure!(!package_name.is_empty(), "pack name must not be blank");
        anyhow::ensure!(
            crate::pack::is_valid_pack_name(package_name),
            "invalid pack name {package_name:?}; bundled pack names use snake_case"
        );
        anyhow::ensure!(
            args.variables.len() <= 32,
            "pack variables exceed the 32-entry limit"
        );
        for (name, value) in &args.variables {
            anyhow::ensure!(
                name != "GENTS_PACK_AGENT_DID"
                    && !name.is_empty()
                    && name.len() <= 128
                    && name.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    }),
                "invalid or protected pack variable name {name:?}"
            );
            anyhow::ensure!(value.len() <= 4096, "pack variable {name:?} is too large");
        }

        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let model = effective
            .pointer("/inference_profile/model_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);
        let endpoint = effective
            .pointer("/documents/InferenceBackend/endpoint")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);
        let variables = args.variables;
        let environment = |name: &str| {
            variables
                .get(name)
                .cloned()
                .or_else(|| name.ends_with("_MODEL").then(|| model.clone()).flatten())
                .or_else(|| {
                    name.ends_with("_ENDPOINT")
                        .then(|| endpoint.clone())
                        .flatten()
                })
                .or_else(|| std::env::var(name).ok())
        };

        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let bindings = crate::graph_package::bundled_graph_package_install_bindings_for_owner(
            &access,
            package_name,
            self.core.agent_did(),
        )
        .await?;
        let distribution = crate::pack::resolve_pack(package_name)?;
        let package = crate::graph_package::load_resolved_graph_package_with_environment(
            &distribution,
            &bindings,
            &environment,
        )?;
        let external_dependencies = package.manifest.external_dependencies.clone();
        let receipt = crate::graph_package::install_loaded_graph_package(
            &access,
            self.core.agent_did(),
            &package,
            &bindings,
            None,
        )
        .await?;
        let previous = crate::graph_pipeline::load_active_graph_plan_with_access(
            &access,
            self.core.agent_did(),
            &receipt.graph_id,
        )
        .await?
        .map(|plan| plan.digest);
        let activation = crate::graph_pipeline::activate_graph_revision_with_access(
            &access,
            self.core.agent_did(),
            &receipt.graph_id,
            &receipt.revision_digest,
            previous.as_deref(),
        )
        .await?;
        Ok(serde_json::to_string_pretty(&json!({
            "install": receipt,
            "activation": activation,
            "external_dependencies": external_dependencies,
            "effect": "The bundled graph is installed and active. Installation did not start a graph run.",
        }))?)
    }
}

impl Tool for InstallPackTool {
    const NAME: &'static str = INSTALL_PACK_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = InstallPackParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Install and activate one known bundled graph pack for the current principal. Uses Setup's current inference model and endpoint for pack model/endpoint variables by default. This writes durable package configuration; it cannot install arbitrary paths, URLs, or another principal's pack. Installation does not run the graph.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "package": {
                        "type": "string",
                        "description": "Exact bundled graph pack name, for example code_review.",
                    },
                    "variables": {
                        "type": "object",
                        "description": "Optional pack interpolation overrides keyed by the exact declared environment-style variable name. Omit for the current Setup inference model/endpoint. Use only values the user requested.",
                        "additionalProperties": {"type": "string"},
                        "default": {},
                    },
                },
                "required": ["package"],
                "additionalProperties": false,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.install(args).await.map_err(SelfConfigError::from)
    }
}

fn graph_access(node: &Arc<EmbeddedNode>) -> crate::config_client::ConfigAccess {
    crate::config_client::ConfigAccess::Local(node.clone())
}

pub struct ListGraphsTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

impl Tool for ListGraphsTool {
    const NAME: &'static str = LIST_GRAPHS_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = GetMyConfigParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Discover installed graphs on this managed node for the current principal. Returns exact active revision digests, package attribution, entry schemas/input contracts, results, limits, and activation state; it never searches a home directory or another endpoint.".to_owned(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.preview.is_some() {
            return Err(anyhow!("list_graphs accepts no parameters").into());
        }
        let owner = escape_graphql_string(self.core.agent_did());
        let access = graph_access(&self.node);
        let response = access
            .execute(&format!(
                r#"{{ GraphDefinition(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{ graph_id agent_did enabled active_revision_digest generation created_at updated_at tags }} }}"#
            ))
            .await?;
        let rows = response
            .get("data")
            .and_then(|data| data.get("GraphDefinition"))
            .and_then(Value::as_array)
            .context("GraphDefinition query returned no rows array")?;
        let mut graphs = Vec::with_capacity(rows.len());
        for definition in rows {
            let graph_id = definition
                .get("graph_id")
                .and_then(Value::as_str)
                .context("GraphDefinition is missing graph_id")?;
            let plan = crate::graph_pipeline::load_active_graph_plan_with_access(
                &access,
                self.core.agent_did(),
                graph_id,
            )
            .await?;
            let package = plan
                .as_ref()
                .and_then(|plan| plan.package.as_ref())
                .map(|package| package.name.clone());
            let revision_digest = plan.as_ref().map(|plan| plan.digest.clone());
            graphs.push(json!({
                "definition": definition,
                "active_plan": &plan,
                "run_with": {
                    "tool": RUN_GRAPH_TOOL_NAME,
                    "package": package,
                    "graph_id": graph_id,
                    "revision_digest": revision_digest,
                },
            }));
        }
        graphs.sort_by(|left, right| {
            left["definition"]["graph_id"]
                .as_str()
                .cmp(&right["definition"]["graph_id"].as_str())
        });
        serde_json::to_string_pretty(&json!({
            "agent_did": self.core.agent_did(),
            "node_bound": true,
            "graphs": graphs,
        }))
        .map_err(|error| SelfConfigError(anyhow!(error)))
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunGraphParams {
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub graph_id: Option<String>,
    #[serde(default)]
    pub revision_digest: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub research_scope: Option<String>,
    #[serde(default)]
    pub freshness: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub output_requirements: Option<String>,
    #[serde(default)]
    pub investigator_count: Option<u8>,
}

pub struct RunGraphTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

impl Tool for RunGraphTool {
    const NAME: &'static str = RUN_GRAPH_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = RunGraphParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Start an installed graph on this managed node as the current principal. For code_review, supply package, repository, base, and head; the node validates the repository against the process root, captures immutable Git evidence, and provisions the workspace. For web_deep_research, supply package and question. For another graph, supply the exact graph_id, revision_digest, entry, and input returned by list_graphs. Returns a durable run receipt and observed initial state.".to_owned(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "package":{"type":"string"},
                    "graph_id":{"type":"string"},
                    "revision_digest":{"type":"string"},
                    "entry":{"type":"string"},
                    "input":{"type":"object"},
                    "repository":{"type":"string","description":"Absolute repository path for code_review; must be under the published process ceiling."},
                    "base":{"type":"string","description":"Git base revision for code_review."},
                    "head":{"type":"string","description":"Git head revision for code_review."},
                    "focus":{"type":"string"},
                    "question":{"type":"string"},
                    "research_scope":{"type":"string"},
                    "freshness":{"type":"string"},
                    "audience":{"type":"string"},
                    "output_requirements":{"type":"string"},
                    "investigator_count":{"type":"integer","minimum":2,"maximum":8}
                },
                "additionalProperties":false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let access = graph_access(&self.node);
        let (graph_id, digest, entry, input) = if let Some(package) = args.package.as_deref() {
            if args.graph_id.is_some()
                || args.revision_digest.is_some()
                || args.entry.is_some()
                || args.input.is_some()
            {
                return Err(anyhow!(
                    "package runs must not include generic graph_id/revision_digest/entry/input selectors"
                )
                .into());
            }
            let plan = crate::graph_package::load_installed_package_plan(
                &access,
                package,
                self.core.agent_did(),
            )
            .await?
            .with_context(|| {
                format!("package {package:?} is not installed; call install_pack first")
            })?;
            let attribution = plan
                .package
                .as_ref()
                .context("active graph revision has no bundled package attribution")?;
            if attribution.name != package {
                return Err(anyhow!(
                    "active graph package attribution changed; call list_graphs again"
                )
                .into());
            }
            let prepared = match package {
                "code_review" => {
                    let effective = self
                        .core
                        .read_effective_config(&BTreeSet::new(), false, false)
                        .await?;
                    let effective_file_mode = effective
                        .pointer("/runtime_effective/effective/file_mode")
                        .and_then(Value::as_str)
                        .map(crate::tool_surface::FileToolMode::parse)
                        .transpose()?
                        .unwrap_or_default();
                    if effective_file_mode == crate::tool_surface::FileToolMode::Off {
                        return Err(anyhow!(
                            "code_review requires effective read authority on the current behavior"
                        )
                        .into());
                    }
                    let effective_root = effective
                        .pointer("/runtime_effective/effective/root")
                        .and_then(Value::as_str)
                        .context("code_review requires an explicit effective managed root")?;
                    crate::graph_package::prepare_code_review_run(
                        &access,
                        self.core.agent_did(),
                        std::path::Path::new(
                            args.repository
                                .as_deref()
                                .context("code_review requires repository")?,
                        ),
                        args.base.as_deref().context("code_review requires base")?,
                        args.head.as_deref().context("code_review requires head")?,
                        args.focus.clone(),
                        Some(std::path::Path::new(effective_root)),
                    )
                    .await?
                }
                "web_deep_research" => {
                    let count = args.investigator_count.unwrap_or(4);
                    if !(2..=8).contains(&count) {
                        return Err(anyhow!(
                            "investigator_count must be between 2 and 8"
                        )
                        .into());
                    }
                    let question = args
                        .question
                        .as_deref()
                        .map(str::trim)
                        .filter(|question| !question.is_empty())
                        .context("web_deep_research requires question")?;
                    crate::graph_package::PreparedGraphRun {
                        entry_name: "research".to_owned(),
                        input: json!({
                            "question": question,
                            "scope": args.research_scope.unwrap_or_default(),
                            "freshness": args.freshness.unwrap_or_default(),
                            "audience": args.audience.unwrap_or_default(),
                            "output_requirements": args.output_requirements.unwrap_or_default(),
                            "investigator_count": count.to_string(),
                        }),
                    }
                }
                other => {
                    return Err(anyhow!("bundled graph {other:?} has no node-bound entry adapter; use exact graph_id/revision_digest/entry/input from list_graphs").into())
                }
            };
            (
                plan.graph_id,
                plan.digest,
                prepared.entry_name,
                prepared.input,
            )
        } else {
            (
                args.graph_id
                    .context("run_graph requires package or graph_id")?,
                args.revision_digest
                    .context("generic graph run requires revision_digest from list_graphs")?,
                args.entry
                    .context("generic graph run requires entry from list_graphs")?,
                args.input.context(
                    "generic graph run requires input matching the advertised entry contract",
                )?,
            )
        };
        let receipt = crate::graph_pipeline::start_graph_run_with_access(
            &access,
            self.core.agent_did(),
            &graph_id,
            Some(&digest),
            &entry,
            input,
        )
        .await?;
        let observed = crate::graph_pipeline::load_graph_run_view_with_access(
            &access,
            self.core.agent_did(),
            &receipt.run_id,
        )
        .await?;
        serde_json::to_string_pretty(&json!({
            "node_bound": true,
            "principal": self.core.agent_did(),
            "receipt": receipt,
            "observed": observed,
            "next": {"status_tool": GET_GRAPH_RUN_TOOL_NAME, "result_tool": GET_GRAPH_RESULT_TOOL_NAME},
        }))
        .map_err(|error| SelfConfigError(anyhow!(error)))
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRunIdParams {
    pub run_id: String,
}

pub struct GetGraphRunTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

impl Tool for GetGraphRunTool {
    const NAME: &'static str = GET_GRAPH_RUN_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = GraphRunIdParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Inspect durable status, stages, requests, cancellation, and result-contract progress for one exact run on this managed node and principal.".to_owned(),
            parameters: json!({"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let view = crate::graph_pipeline::load_graph_run_view_with_access(
            &graph_access(&self.node),
            self.core.agent_did(),
            &args.run_id,
        )
        .await?;
        serde_json::to_string_pretty(&view).map_err(|error| SelfConfigError(anyhow!(error)))
    }
}

pub struct GetGraphResultTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

impl Tool for GetGraphResultTool {
    const NAME: &'static str = GET_GRAPH_RESULT_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = GraphRunIdParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Load terminal graph results and their durable documents for one exact run on this managed node and principal. A nonterminal run is reported honestly as not ready.".to_owned(),
            parameters: json!({"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let view = crate::graph_pipeline::load_graph_run_result_view_with_access(
            &graph_access(&self.node),
            self.core.agent_did(),
            &args.run_id,
        )
        .await?;
        serde_json::to_string_pretty(&view).map_err(|error| SelfConfigError(anyhow!(error)))
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelGraphRunParams {
    pub run_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

pub struct CancelGraphRunTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

impl Tool for CancelGraphRunTool {
    const NAME: &'static str = CANCEL_GRAPH_RUN_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = CancelGraphRunParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Persist cancellation intent and interrupt active requests for one exact graph run on this managed node and principal. Returns the observed durable run state.".to_owned(),
            parameters: json!({"type":"object","properties":{"run_id":{"type":"string"},"reason":{"type":"string"}},"required":["run_id"],"additionalProperties":false}),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let view = crate::graph_pipeline::request_graph_run_cancellation_with_access(
            &graph_access(&self.node),
            self.core.agent_did(),
            &args.run_id,
            args.reason.as_deref(),
        )
        .await?;
        serde_json::to_string_pretty(&view).map_err(|error| SelfConfigError(anyhow!(error)))
    }
}

/// Build the gated self-config tool family for one behavior. Fails closed:
/// with an empty agent DID (bare oneshot contexts) no tools are registered.
pub fn build_self_config_tools(
    node: Arc<EmbeddedNode>,
    agent_did: String,
    identity: Option<Arc<dyn AgentIdentity>>,
    config: &SelfConfigToolConfig,
) -> Vec<Box<dyn ToolDyn>> {
    if !config.enabled && !config.enable_graph_tools {
        return Vec::new();
    }
    let core =
        match SelfConfigCore::new(node.clone(), agent_did.clone(), config.behavior_id.clone()) {
            Ok(core) => core
                .with_no_lockout(config.no_lockout)
                .with_process_ceiling(config.process_ceiling.clone()),
            Err(error) => {
                tracing::warn!(
                    behavior_id = %config.behavior_id,
                    %error,
                    "self-config tools requested but not registrable; failing closed"
                );
                return Vec::new();
            }
        };

    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    if config.enable_graph_tools {
        tools.push(Box::new(ListGraphsTool {
            core: core.clone(),
            node: node.clone(),
        }));
        tools.push(Box::new(RunGraphTool {
            core: core.clone(),
            node: node.clone(),
        }));
        tools.push(Box::new(GetGraphRunTool {
            core: core.clone(),
            node: node.clone(),
        }));
        tools.push(Box::new(GetGraphResultTool {
            core: core.clone(),
            node: node.clone(),
        }));
        tools.push(Box::new(CancelGraphRunTool {
            core: core.clone(),
            node: node.clone(),
        }));
    }
    if !config.enabled {
        return tools;
    }
    tools.push(Box::new(GetMyConfigTool {
        core: core.clone(),
        categories: config.categories.clone(),
        no_lockout: config.no_lockout,
        dry_run: config.dry_run,
        allow_pack_install: config.enable_pack_install,
    }));
    if config.enable_pack_install {
        tools.push(Box::new(InstallPackTool {
            core: core.clone(),
            node: node.clone(),
        }));
    }
    for category in &config.categories {
        match category.as_str() {
            "behavior" => tools.push(Box::new(ConfigureBehaviorTool { core: core.clone() })),
            "tools" => tools.push(Box::new(ConfigureToolsTool {
                core: core.clone(),
                allow_pack_install: config.enable_pack_install,
            })),
            "profile" => tools.push(Box::new(ConfigureProfileTool { core: core.clone() })),
            "backend" => tools.push(Box::new(ConfigureBackendTool { core: core.clone() })),
            "mcp_service" => tools.push(Box::new(ConfigureMcpServiceTool { core: core.clone() })),
            "automation" => tools.push(Box::new(ConfigureAutomationTool { core: core.clone() })),
            "persona" => match identity.clone() {
                Some(identity) if identity.did() == agent_did => {
                    tools.push(Box::new(ConfigurePersonaTool {
                        node: node.clone(),
                        agent_did: agent_did.clone(),
                        identity,
                        process_ceiling: config.process_ceiling.clone(),
                    }))
                }
                _ => tracing::warn!(
                    agent_did = %agent_did,
                    "configure_behaviors requires the exact local principal signer; skipping"
                ),
            },
            other => {
                tracing::warn!(category = %other, "unknown self-config category; skipping");
            }
        }
    }
    tools
}

/// Advertised tool names for a resolved self-config surface.
pub fn self_config_tool_names(config: &SelfConfigToolConfig) -> Vec<String> {
    let mut names = Vec::new();
    if config.enable_graph_tools {
        names.extend([
            LIST_GRAPHS_TOOL_NAME.to_string(),
            RUN_GRAPH_TOOL_NAME.to_string(),
            GET_GRAPH_RUN_TOOL_NAME.to_string(),
            GET_GRAPH_RESULT_TOOL_NAME.to_string(),
            CANCEL_GRAPH_RUN_TOOL_NAME.to_string(),
        ]);
    }
    if !config.enabled {
        return names;
    }
    names.push(GET_MY_CONFIG_TOOL_NAME.to_string());
    if config.enable_pack_install {
        names.push(INSTALL_PACK_TOOL_NAME.to_string());
    }
    names.extend(
        config
            .categories
            .iter()
            .filter_map(|category| configure_tool_name_for_category(category))
            .map(ToOwned::to_owned),
    );
    names
}
