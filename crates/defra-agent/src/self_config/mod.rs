//! Agent self-configuration tools (#654).
//!
//! A typed, self-documenting tool family through which an agent manages **its
//! own** configuration documents: `get_my_config` plus one `configure_*` tool
//! per category (behavior, tools, profile, backend, mcp_service, automation).
//! Gated by `ToolSelection.enable_self_config` + `self_config_categories`;
//! writes execute under the agent DID inside one transaction, so DefraDB ACP
//! — not app-level checks — is the authorization boundary. The Lean
//! `SelfConfig` model proves the patch semantics (identity immutability,
//! field containment, transactional totality, no-lockout recoverability);
//! `config_client::patch` is the fenced implementation.
//!
//! The tools change *how* the agent behaves, never *who it is*: every patch
//! surface excludes identity/unique keys, the owner DID, runtime-owned status
//! fields, and secrets (`InferenceBackend.api_key` in particular is neither
//! readable nor writable here).

mod ops;
mod read;
#[cfg(test)]
mod tests;

pub use ops::{PatchOutcome, SelfConfigCore, EFFECT_TIMING_NOTE};

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::config_client::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::document_config::{Schedule, Task};
use crate::llm::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_surface::SelfConfigToolConfig;
use defra_node::EmbeddedNode;
use ops::{decode_merged, guard_selection_keeps_gate, validate_merged_selection, ApplyRequest};

pub const GET_MY_CONFIG_TOOL_NAME: &str = "get_my_config";
pub const CONFIGURE_BEHAVIOR_TOOL_NAME: &str = "configure_behavior";
pub const CONFIGURE_TOOLS_TOOL_NAME: &str = "configure_tools";
pub const CONFIGURE_PROFILE_TOOL_NAME: &str = "configure_profile";
pub const CONFIGURE_BACKEND_TOOL_NAME: &str = "configure_backend";
pub const CONFIGURE_MCP_SERVICE_TOOL_NAME: &str = "configure_mcp_service";
pub const CONFIGURE_AUTOMATION_TOOL_NAME: &str = "configure_automation";

/// Every tool name of the family, for reserved-name checks and surfacing.
pub const SELF_CONFIG_TOOL_NAMES: [&str; 7] = [
    GET_MY_CONFIG_TOOL_NAME,
    CONFIGURE_BEHAVIOR_TOOL_NAME,
    CONFIGURE_TOOLS_TOOL_NAME,
    CONFIGURE_PROFILE_TOOL_NAME,
    CONFIGURE_BACKEND_TOOL_NAME,
    CONFIGURE_MCP_SERVICE_TOOL_NAME,
    CONFIGURE_AUTOMATION_TOOL_NAME,
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

fn behavior_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let behavior_id = core.behavior_id().to_string();
    let agent_did = core.agent_did().to_string();
    let mut request = ApplyRequest::new(SelfConfigTarget::AgentBehavior, patch);
    request.resolve_unique = Box::new(move |_| Ok(behavior_id.clone()));
    request.validate = Box::new(move |txn, _anchor, _stored, merged| {
        let merged = merged.clone();
        let agent_did = agent_did.clone();
        Box::pin(async move {
            let behavior: crate::AgentBehaviorDocument = decode_merged("AgentBehavior", &merged)?;
            for (field, target) in [
                ("tool_selection_id", SelfConfigTarget::ToolSelection),
                ("inference_profile_id", SelfConfigTarget::InferenceProfile),
                ("backend_id", SelfConfigTarget::InferenceBackend),
            ] {
                if let Some(reference) = merged
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    let Some((_, referenced)) =
                        crate::config_client::patch::read_doc_in_txn(txn, target, reference)
                            .await?
                    else {
                        bail!(
                            "behavior.{field} references {reference:?}, which does not exist \
                             in {}",
                            target.collection_name()
                        );
                    };
                    // ToolSelection is per-agent: binding another agent's
                    // selection is invalid config (the document view rejects
                    // cross-agent rows) and would let configure_tools patch a
                    // foreign selection. Profiles/backends are global by
                    // design.
                    if target == SelfConfigTarget::ToolSelection {
                        let owner = referenced
                            .get("agent_did")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if owner != agent_did {
                            bail!(
                                "behavior.tool_selection_id references {reference:?}, which is \
                                 owned by {owner:?}, not this agent — self-config is self only"
                            );
                        }
                    }
                }
            }
            let _ = behavior;
            Ok(())
        })
    });
    request.guard = Box::new(|_anchor, merged| {
        if merged.get("enabled").and_then(Value::as_bool) == Some(false) {
            bail!(
                "no-lockout guard: disabling this behavior would strip the agent's own \
                 reconfigure ability"
            );
        }
        Ok(())
    });
    request
}

fn tools_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let anchor_hint = core.behavior_id().to_string();
    let agent_did = core.agent_did().to_string();
    let mut request = ApplyRequest::new(SelfConfigTarget::ToolSelection, patch);
    request.resolve_unique = Box::new(move |anchor| {
        anchor.ref_id("tool_selection_id").ok_or_else(|| {
            anyhow!(
                "behavior {anchor_hint} has no tool_selection_id; bind one first \
                 (e.g. configure_behavior {{\"tool_selection_id\": \"<selection>\"}})"
            )
        })
    });
    request.validate = Box::new(move |_txn, _anchor, stored, merged| {
        let stored = stored.clone();
        let merged = merged.clone();
        let agent_did = agent_did.clone();
        Box::pin(async move {
            // Self only: the selection being patched must belong to this
            // agent, even if a stale behavior binding points elsewhere.
            let owner = stored
                .get("agent_did")
                .and_then(Value::as_str)
                .unwrap_or("");
            if owner != agent_did {
                bail!(
                    "the bound ToolSelection is owned by {owner:?}, not this agent — \
                     self-config is self only"
                );
            }
            validate_merged_selection(&merged)
        })
    });
    request.guard = Box::new(|_anchor, merged| guard_selection_keeps_gate(merged));
    request
}

fn profile_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceProfile, patch);
    request.resolve_unique = Box::new(|anchor| {
        anchor.ref_id("inference_profile_id").ok_or_else(|| {
            anyhow!("behavior has no inference_profile_id; bind one via configure_behavior first")
        })
    });
    request.validate = Box::new(|_txn, _anchor, _stored, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            let _profile: crate::document_config::InferenceProfile =
                decode_merged("InferenceProfile", &merged)?;
            Ok(())
        })
    });
    request
}

fn backend_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceBackend, patch);
    request.resolve_unique = Box::new(|anchor| {
        anchor
            .ref_id("backend_id")
            .ok_or_else(|| anyhow!("behavior has no backend_id; bind one via configure_behavior"))
    });
    request.validate = Box::new(|_txn, _anchor, _stored, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            crate::BackendProviderKind::parse_optional(
                merged.get("provider_kind").and_then(Value::as_str),
            )?;
            for field in ["max_concurrent", "max_queue_depth"] {
                if let Some(value) = merged.get(field) {
                    let valid = value.as_i64().is_some_and(|value| value > 0);
                    if !valid {
                        bail!("backend.{field} must be a positive integer");
                    }
                }
            }
            Ok(())
        })
    });
    request.guard = Box::new(|anchor, merged| {
        if merged.get("enabled").and_then(Value::as_bool) == Some(false) {
            bail!(
                "no-lockout guard: disabling the behavior's own backend would make its \
                 model unresolvable"
            );
        }
        if let Some(models) = merged.get("models").and_then(Value::as_array) {
            if !models.is_empty() {
                if let Some(model_name) = anchor
                    .doc
                    .get("model_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    let listed = models
                        .iter()
                        .any(|model| model.as_str() == Some(model_name));
                    if !listed {
                        bail!(
                            "no-lockout guard: the patched models list drops this behavior's \
                             model {model_name:?}"
                        );
                    }
                }
            }
        }
        Ok(())
    });
    request
}

fn mcp_service_request(service_id: String, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::ToolServiceRegistry, patch);
    request.resolve_unique = Box::new(move |_| Ok(service_id.clone()));
    request.validate = Box::new(|_txn, _anchor, _stored, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            if let Some(port) = merged.get("mcp_port") {
                let valid = port
                    .as_i64()
                    .is_some_and(|port| (1..=65535).contains(&port));
                if !valid {
                    bail!("mcp_service.mcp_port must be within 1..=65535");
                }
            }
            Ok(())
        })
    });
    request
}

fn automation_request(
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let behavior_id = core.behavior_id().to_string();
    let core = core.clone();
    let mut request = ApplyRequest::new(target, patch);
    request.allow_create = true;
    {
        let id = id.clone();
        request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    }
    request.on_create = Box::new(move |unique_value, merged| {
        merged.insert(
            target.unique_field().to_string(),
            Value::String(unique_value.to_string()),
        );
        if target == SelfConfigTarget::Task {
            // The ownership link is pinned at create and immutable after.
            merged.insert(
                "behavior_id".to_string(),
                Value::String(behavior_id.clone()),
            );
        }
        merged
            .entry("enabled".to_string())
            .or_insert(Value::Bool(true));
        let now = chrono::Utc::now().to_rfc3339();
        merged.insert("created_at".to_string(), Value::String(now.clone()));
        merged.insert("updated_at".to_string(), Value::String(now));
        Ok(())
    });
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let core = core.clone();
        let stored = stored.clone();
        let merged = merged.clone();
        Box::pin(async move {
            match target {
                SelfConfigTarget::Task => {
                    let task: Task = decode_merged("Task", &merged)?;
                    let owner = if stored.is_empty() {
                        merged.get("behavior_id").and_then(Value::as_str)
                    } else {
                        stored.get("behavior_id").and_then(Value::as_str)
                    };
                    if owner != Some(core.behavior_id()) {
                        bail!(
                            "task {} is not owned by this behavior (behavior_id {owner:?}); \
                             self-config automation is scoped to this behavior",
                            task.task_id
                        );
                    }
                }
                SelfConfigTarget::Schedule => {
                    let schedule: Schedule = decode_merged("Schedule", &merged)?;
                    // An EXISTING schedule may only be patched if it already
                    // belongs to this behavior — otherwise a patch could seize
                    // another behavior's schedule by re-pointing its task_id
                    // at an owned task.
                    ensure_stored_automation_owned(&core, txn, anchor, "schedule", &stored).await?;
                    let Some(task_id) = schedule
                        .task_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    else {
                        bail!("schedule.task_id is required and must reference an owned task");
                    };
                    if !core.task_owned(txn, anchor, task_id).await? {
                        bail!(
                            "schedule.task_id {task_id:?} does not reference a task owned by \
                             this behavior"
                        );
                    }
                    if schedule.interval_secs.is_none() && schedule.cron.is_none() {
                        bail!("schedule needs a cadence: set interval_secs or cron");
                    }
                }
                SelfConfigTarget::EventTrigger => {
                    ensure_stored_automation_owned(&core, txn, anchor, "event_trigger", &stored)
                        .await?;
                    let task_id = merged
                        .get("task_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            anyhow!(
                                "event_trigger.task_id is required and must reference an \
                                 owned task"
                            )
                        })?;
                    if !core.task_owned(txn, anchor, task_id).await? {
                        bail!(
                            "event_trigger.task_id {task_id:?} does not reference a task \
                             owned by this behavior"
                        );
                    }
                    for field in ["source_collection", "event_kind"] {
                        let present = merged
                            .get(field)
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.trim().is_empty());
                        if !present {
                            bail!("event_trigger.{field} is required");
                        }
                    }
                }
                _ => unreachable!("automation targets only"),
            }
            Ok(())
        })
    });
    request
}

/// A stored schedule/trigger may only be patched when its CURRENT task link
/// already belongs to this behavior; creation (empty stored doc) is exempt.
async fn ensure_stored_automation_owned(
    core: &SelfConfigCore,
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    anchor: &ops::BehaviorAnchor,
    kind: &str,
    stored: &Map<String, Value>,
) -> Result<()> {
    if stored.is_empty() {
        return Ok(());
    }
    let stored_task = stored
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let owned = match stored_task {
        Some(task_id) => core.task_owned(txn, anchor, task_id).await?,
        None => false,
    };
    if !owned {
        bail!(
            "{kind} exists but is not owned by this behavior (its task_id {stored_task:?} \
             does not resolve to an owned task) — self-config automation is self only"
        );
    }
    Ok(())
}

fn automation_target(kind: &str) -> Result<SelfConfigTarget> {
    match kind {
        "task" => Ok(SelfConfigTarget::Task),
        "schedule" => Ok(SelfConfigTarget::Schedule),
        "event_trigger" => Ok(SelfConfigTarget::EventTrigger),
        other => bail!("unknown automation kind {other:?}; use task, schedule, or event_trigger"),
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
pub struct GetMyConfigParams {
    /// Dry-run preview (requires `self_config_dry_run`): the diff a patch
    /// would produce, without committing.
    #[serde(default)]
    pub preview: Option<PreviewParams>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PreviewParams {
    pub category: String,
    /// Target id for categories that need one (`mcp_service` service_id,
    /// `automation` task/schedule/trigger id).
    #[serde(default)]
    pub id: Option<String>,
    /// Automation kind (`task` | `schedule` | `event_trigger`).
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
                            "enum": ["task", "schedule", "event_trigger"],
                            "description": "Automation kind (category=automation only).",
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
                         (ToolSelection.self_config_dry_run)"
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
                    "behavior" => behavior_request(&self.core, patch),
                    "tools" => tools_request(&self.core, patch),
                    "profile" => profile_request(patch),
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
pub struct PatchOnlyParams {
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
                "Patch this agent's own AgentBehavior document (prompt, model, backend and \
                 profile references, compaction, skills wiring). Identity fields are \
                 immutable. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::AgentBehavior) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let request = behavior_request(&self.core, args.patch.into_patch());
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
                "Patch this agent's own ToolSelection document (tool gates and scopes, \
                 including the self-config gate itself). tool_policy_version and \
                 write_tools are operator/apply-managed and protected. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::ToolSelection) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
                "Patch the InferenceProfile this behavior references (sampling, turn and \
                 token limits, deadlines, retry tuning). Note: profiles are global \
                 documents — other behaviors referencing the same profile see the change. \
                 {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::InferenceProfile) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let outcome = self
            .core
            .apply(profile_request(args.patch.into_patch()))
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
            description: format!(
                "Patch the InferenceBackend this behavior references (endpoint, models, \
                 api_key_env_var reference, concurrency). The raw api_key secret and \
                 prober-owned health fields are protected. Backends are global documents. \
                 {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": { "patch": patch_parameter_schema(SelfConfigTarget::InferenceBackend) },
                "required": ["patch"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
pub struct ConfigureAutomationParams {
    /// `task` | `schedule` | `event_trigger`.
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
            description: format!(
                "Create or patch automation owned by this behavior: Tasks (kind=task, \
                 pinned to this behavior), Schedules (kind=schedule, task_id must \
                 reference an owned task; cadence via interval_secs or cron), and \
                 EventTriggers (kind=event_trigger). Scheduler/trigger runtime \
                 bookkeeping fields are protected. {EFFECT_TIMING_NOTE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["task", "schedule", "event_trigger"] },
                    "id": { "type": "string" },
                    "patch": {
                        "type": "object",
                        "description": format!(
                            "Writable fields — task: {}; schedule: {}; event_trigger: {}.",
                            SelfConfigTarget::Task.writable_fields().join(", "),
                            SelfConfigTarget::Schedule.writable_fields().join(", "),
                            SelfConfigTarget::EventTrigger.writable_fields().join(", "),
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

/// Build the gated self-config tool family for one behavior. Fails closed:
/// with an empty agent DID (bare oneshot contexts) no tools are registered.
pub fn build_self_config_tools(
    node: Arc<EmbeddedNode>,
    agent_did: String,
    config: &SelfConfigToolConfig,
) -> Vec<Box<dyn ToolDyn>> {
    if !config.enabled {
        return Vec::new();
    }
    let core = match SelfConfigCore::new(node, agent_did, config.behavior_id.clone()) {
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
