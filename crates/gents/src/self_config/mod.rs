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

pub use ops::{PatchOutcome, SelfConfigCore, EFFECT_TIMING_NOTE};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::agent::p2p_reconcile::{GraphqlPersonaRequestStore, PersonaRequestStore};
use crate::agent::persona_ops::local_persona_request_mutation;
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
pub const CONFIGURE_PERSONA_TOOL_NAME: &str = "configure_persona";

/// Every tool name of the family, for reserved-name checks and surfacing.
pub const SELF_CONFIG_TOOL_NAMES: [&str; 8] = [
    GET_MY_CONFIG_TOOL_NAME,
    CONFIGURE_BEHAVIOR_TOOL_NAME,
    CONFIGURE_TOOLS_TOOL_NAME,
    CONFIGURE_PROFILE_TOOL_NAME,
    CONFIGURE_BACKEND_TOOL_NAME,
    CONFIGURE_MCP_SERVICE_TOOL_NAME,
    CONFIGURE_AUTOMATION_TOOL_NAME,
    CONFIGURE_PERSONA_TOOL_NAME,
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
        "persona" => Some(CONFIGURE_PERSONA_TOOL_NAME),
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
fn tools_request(_core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    request.validate = Box::new(|_, _, _, merged| {
        let merged = merged.clone();
        Box::pin(async move { validate_merged_selection(&merged) })
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
                    "tools" => tools_request(&self.core, patch),
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
                            "persona actions are request-based; no patch preview is available \
                             — call configure_persona directly (it authors and polls a \
                             PersonaConfigRequest row, not a patch)"
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
        let request = tools_request(&self.core, args.patch.into_patch());
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
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurePersonaParams {
    /// `list` | `create` | `edit` | `clone` | `disable`.
    pub action: String,
    #[serde(default)]
    pub persona_name: Option<String>,
    /// Exact behavior_id of the sibling persona (required for edit/disable).
    #[serde(default)]
    pub behavior_id: Option<String>,
    /// Exact sibling behavior_id to clone; a supplied preset requests a new persona.
    #[serde(default)]
    pub clone_from: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    /// Exact owner-scoped inference profile ID.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Promote the applied persona to this principal's default behavior.
    #[serde(default)]
    pub make_default: bool,
}

/// How long [`ConfigurePersonaTool`] polls a freshly-authored
/// `PersonaConfigRequest` row before returning it still-`pending`: the
/// in-process reconciler sweeps on every `Update` event, so a healthy node
/// converges well inside this window.
const PERSONA_REQUEST_POLL_TIMEOUT: Duration = Duration::from_secs(5);
const PERSONA_REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Default, serde::Serialize)]
struct PersonaCatalogSnapshot {
    allowed_roots: Vec<String>,
    available_profile_ids: Vec<String>,
    behaviors: BTreeMap<String, PersonaBehaviorSnapshot>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct PersonaBehaviorSnapshot {
    enabled: bool,
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
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + PERSONA_REQUEST_POLL_TIMEOUT;
    loop {
        if let Some(row) = load_persona_request_row(node, request_key, agent_did).await? {
            if row.status.as_deref() != Some("pending") {
                return serde_json::to_string_pretty(&row)
                    .map_err(|error| anyhow!("serialize persona request outcome: {error}"));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return serde_json::to_string_pretty(&json!({
                "request_key": request_key,
                "status": "pending",
                "note": "still pending after 5s; the reconciler may need another moment — \
                         retry, or call configure_persona with action \"list\" to see whether \
                         this agent's behaviors already reflect the change",
            }))
            .map_err(|error| anyhow!("serialize pending outcome: {error}"));
        }
        tokio::time::sleep(PERSONA_REQUEST_POLL_INTERVAL).await;
    }
}

async fn persona_list(node: &Arc<EmbeddedNode>, agent_did: &str) -> Result<String> {
    let store = GraphqlPersonaRequestStore::new(node.clone());
    let catalog = store.load_catalog_view(agent_did).await?;
    let snapshot = PersonaCatalogSnapshot {
        allowed_roots: catalog.allowed_roots.into_iter().collect(),
        available_profile_ids: catalog.available_profile_ids.into_iter().collect(),
        behaviors: catalog
            .behaviors
            .into_iter()
            .map(|(behavior_id, reference)| {
                (
                    behavior_id,
                    PersonaBehaviorSnapshot {
                        enabled: reference.enabled,
                    },
                )
            })
            .collect(),
    };
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

async fn persona_mutate(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    identity: &dyn AgentIdentity,
    args: &ConfigurePersonaParams,
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
            bail!("{action} action requires behavior_id (the sibling persona to target)");
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
            if preset_given {
                // Unifies with the mobile composer's semantic: naming a
                // preset means you want DIFFERENT permissions than the
                // clone source (which admission rejects for clone_from
                // anyway — clone copies permissions verbatim), so this
                // authors a plain create instead of a clone.
                ("create", None)
            } else {
                let clone_from = args
                    .clone_from
                    .as_deref()
                    .map(str::to_owned)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "clone action requires clone_from (the sibling behavior_id to clone)"
                        )
                    })?;
                ("create", Some(clone_from))
            }
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
        persona_name: args.persona_name.clone(),
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

    poll_persona_request(node, &request_key, agent_did).await
}

impl Tool for ConfigurePersonaTool {
    const NAME: &'static str = CONFIGURE_PERSONA_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigurePersonaParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: CONFIGURE_PERSONA_TOOL_NAME.to_string(),
            description: "List, create, clone, edit, or disable sibling personas through signed PersonaConfigRequest admission. Choose an existing inference profile. A create/edit may atomically make the applied behavior this principal's default. Document IDs are exact and scoped to this principal. Clone copies source permissions; choosing a preset requests a new persona instead. Pending requests return their key for later inspection.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "create", "edit", "clone", "disable"],
                    },
                    "persona_name": {
                        "type": "string",
                        "description": "Display name for the persona (create/edit).",
                    },
                    "behavior_id": {
                        "type": "string",
                        "description": "Exact behavior_id of the sibling persona (required for edit/disable).",
                    },
                    "clone_from": {
                        "type": "string",
                        "description": "Exact sibling behavior_id to clone from.",
                    },
                    "root": {
                        "type": "string",
                        "description": "Workspace root to scope the persona to, if any. CAUTION on edit: dimensions are replaced wholesale — omitting root CLEARS the persona's existing root scope (widening file access to the host default). Always resend the current root when editing unless you intend to clear it.",
                    },
                    "preset": {
                        "type": "string",
                        "description": "Built-in permission preset (create/edit; also converts a clone into a plain create when set alongside clone_from).",
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
                },
                "required": ["action"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action.as_str() {
            "list" => Ok(persona_list(&self.node, &self.agent_did).await?),
            "create" | "edit" | "clone" | "disable" => {
                Ok(
                    persona_mutate(&self.node, &self.agent_did, self.identity.as_ref(), &args)
                        .await?,
                )
            }
            other => Err(SelfConfigError(anyhow!(
                "unknown action {other:?}; use list|create|edit|clone|disable"
            ))),
        }
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
    if !config.enabled {
        return Vec::new();
    }
    let core =
        match SelfConfigCore::new(node.clone(), agent_did.clone(), config.behavior_id.clone()) {
            Ok(core) => core.with_no_lockout(config.no_lockout),
            Err(error) => {
                tracing::warn!(
                    behavior_id = %config.behavior_id,
                    %error,
                    "self-config tools requested but not registrable; failing closed"
                );
                return Vec::new();
            }
        };

    let mut tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(GetMyConfigTool {
        core: core.clone(),
        categories: config.categories.clone(),
        no_lockout: config.no_lockout,
        dry_run: config.dry_run,
    })];
    for category in &config.categories {
        match category.as_str() {
            "behavior" => tools.push(Box::new(ConfigureBehaviorTool { core: core.clone() })),
            "tools" => tools.push(Box::new(ConfigureToolsTool { core: core.clone() })),
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
                    }))
                }
                _ => tracing::warn!(
                    agent_did = %agent_did,
                    "configure_persona requires the exact local principal signer; skipping"
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
    if !config.enabled {
        return Vec::new();
    }
    let mut names = vec![GET_MY_CONFIG_TOOL_NAME.to_string()];
    names.extend(
        config
            .categories
            .iter()
            .filter_map(|category| configure_tool_name_for_category(category))
            .map(ToOwned::to_owned),
    );
    names
}
