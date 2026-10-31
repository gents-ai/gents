//! Raw API keys cannot be changed or returned.

pub mod agent;
mod agent_management;
pub use agent_management::configure_agent;
use agent_management::load_agent_catalog;
#[cfg(test)]
use agent_management::{agent_mutate, agent_preview};
mod command;
pub(crate) use command::{is_help_call, ConfigCommandParams};
mod execution;
pub use execution::ConfigExecutionReceipt;
mod ops;
mod pack_plugins;
mod read;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;

pub use ops::{
    apply_tool_grant_selection, guard_agent_keeps_reach, guard_backend_auth, guard_backend_choice,
    guard_tools_keep_control, guard_tools_keep_grants, reselection_keeps_grants,
    validate_tool_network_selection, OperatorGrants, PatchOutcome, SelfConfigCore,
    EFFECT_TIMING_NOTE,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config_client::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::graphql::escape_graphql_string;
use crate::llm::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_surface::presets::{builtin_preset_names, preset_fields};
use crate::tool_surface::SelfConfigToolConfig;
use crate::NodeIdentity;
use agent::derive_agent_id;
use defra_node::EmbeddedNode;
use ops::{decode_merged, validate_merged_selection, ApplyRequest};

pub const ENGINEER_AGENT_TAG: &str = "gents:engineer";

pub fn engineer_self_config() -> crate::document_config::SelfConfigTools {
    serde_json::from_str(gents_protocol::ENGINEER_SELF_CONFIG_JSON)
        .expect("bundled Engineer grant must match canonical SelfConfigTools")
}

pub const CONFIG_TOOL_NAME: &str = "config";

pub const SELF_CONFIG_TOOL_NAMES: [&str; 1] = [CONFIG_TOOL_NAME];

pub(crate) use crate::tool_output::{ordered, Ordered};

const EFFECTIVE_ORDER: &[&str] = &[
    "execution_settings",
    "agent_id",
    "agent",
    "context",
    "inference_profile",
    "documents",
    "automation",
    "skills",
    "runtime_effective",
    "tool_grants",
    "self_config",
    "effect_timing",
];

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

fn outcome_text(outcome: &PatchOutcome) -> Result<String> {
    serde_json::to_string_pretty(outcome).map_err(|error| anyhow!("serialize outcome: {error}"))
}

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
fn agent_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let id = core.agent_id().to_owned();
    let mut request = ApplyRequest::new(SelfConfigTarget::Agent, patch);
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.guard = Box::new(|_, stored, merged| guard_agent_keeps_reach(stored, merged));
    fence_profile_pick(&mut request);
    request
}
fn fence_profile_pick(request: &mut ApplyRequest<'static>) {
    request.validate = Box::new(|txn, anchor, stored, merged| {
        Box::pin(async move {
            let Some(next) = merged.get("inference_profile_id").and_then(Value::as_str) else {
                return Ok(());
            };
            let current = stored.get("inference_profile_id").and_then(Value::as_str);
            if current == Some(next) {
                return Ok(());
            }
            let owner = merged
                .get("node_did")
                .and_then(Value::as_str)
                .context("document is missing node_did")?;
            let current_backend = match current {
                Some(id) => Some(ops::profile_backend_id(txn, owner, id).await?),
                None => anchor
                    .profile
                    .get("backend_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            };
            let next_backend = ops::profile_backend_id(txn, owner, next).await?;
            ops::guard_backend_choice_in_txn(txn, owner, current_backend.as_deref(), &next_backend)
                .await
        })
    });
}

/// Model-facing patches may target any owned agent, including the invoking
/// configurator itself. Keep the shared-reference check inside the same
/// transaction as validation/publication so a stale preflight cannot
/// authorize a write.
fn protect_working_agent(mut request: ApplyRequest<'static>) -> ApplyRequest<'static> {
    let target = request.target;
    let acknowledged = request.shared_ack;
    let validate = request.validate;
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let validation = validate(txn, anchor, stored, merged);
        Box::pin(async move {
            let owner = anchor
                .doc
                .get("node_did")
                .and_then(Value::as_str)
                .context("selected agent is missing node_did")?;
            // A Context or Tools that more than one agent selects is refused
            // unless the caller acknowledged it; the refusal names every agent
            // the write would reach.
            let require_only_referrer = |agents: Vec<String>| -> Result<()> {
                if acknowledged || agents.len() <= 1 {
                    return Ok(());
                }
                Err(ops::shared_refusal(&agents))
            };
            if target == SelfConfigTarget::AgentContext || target == SelfConfigTarget::Tools {
                let context_id = anchor.context.get("context_id").and_then(Value::as_str)
                    .context("selected agent has no context; create and select a Context before editing it or its Tools")?;
                require_only_referrer(
                    ops::shared_referrers(txn, SelfConfigTarget::AgentContext, owner, context_id)
                        .await?,
                )?;
                if target == SelfConfigTarget::Tools {
                    let tools_id = stored
                        .get("tools_id")
                        .and_then(Value::as_str)
                        .context("selected Tools is missing tools_id")?;
                    require_only_referrer(
                        ops::shared_referrers(txn, SelfConfigTarget::Tools, owner, tools_id)
                            .await?,
                    )?;
                }
            }
            ops::guard_compaction_choice_in_txn(txn, target, anchor, stored, merged).await?;
            validation.await
        })
    });
    request
}

fn stored_host_root(stored: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    serde_json::from_value::<crate::document_config::Tools>(serde_json::Value::Object(
        stored.clone(),
    ))
    .ok()?
    .host?
    .root
}

fn tools_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    let ceiling_root = core.process_ceiling().root.clone();
    let held = core.held_grants().clone();
    request.normalize = Box::new(move |txn, _, stored, merged| {
        let ceiling_root = ceiling_root.clone();
        Box::pin(async move {
            let policy = crate::tool_surface::load_workspace_root_policy_in_txn(
                txn,
                ceiling_root.as_deref(),
            )
            .await?;
            let stored_root = stored_host_root(stored);
            let mut tools = decode_merged::<crate::document_config::Tools>("Tools", merged)?;
            crate::tool_surface::canonicalize_tools_root(
                &mut tools,
                &policy,
                stored_root.as_deref(),
            )?;
            let canonical = serde_json::to_value(tools)?
                .as_object()
                .context("canonical Tools document must be an object")?
                .clone();
            *merged = canonical;
            Ok(())
        })
    });
    request.validate = Box::new(move |_, _, stored, merged| {
        let stored = stored.clone();
        let merged = merged.clone();
        let held = held.clone();
        Box::pin(async move {
            validate_merged_selection(&merged)?;
            guard_tools_keep_grants(&held, Some(&stored), &merged)
        })
    });
    request.guard = Box::new(|_, stored, merged| guard_tools_keep_control(stored, merged));
    request
}
fn refuse_silent_tools_drops(
    mut request: ApplyRequest<'static>,
    allow_drop: BTreeSet<String>,
) -> ApplyRequest<'static> {
    let groups = request
        .patch
        .iter()
        .map(|(field, _)| field.clone())
        .filter(|field| !allow_drop.contains(field))
        .collect::<Vec<_>>();
    let guard = request.guard;
    request.guard = Box::new(move |anchor, stored, merged| {
        guard(anchor, stored, merged)?;
        let mut dropped = Vec::new();
        for group in &groups {
            dropped_settings(
                group,
                stored.get(group).unwrap_or(&Value::Null),
                merged.get(group).unwrap_or(&Value::Null),
                &mut dropped,
            );
        }
        if dropped.is_empty() {
            return Ok(());
        }
        let mut groups = dropped
            .iter()
            .map(|path| path.split(['.', ' ']).next().unwrap_or_default())
            .collect::<Vec<_>>();
        groups.dedup();
        bail!(
            "this would drop existing settings from your own Tools: {}. Send them back in set (see [\"tools\",\"get\"]), or to drop them on purpose add options.allow-drop = {:?}",
            dropped.join(", "),
            groups.join(",")
        )
    });
    request
}

fn dropped_settings(path: &str, before: &Value, after: &Value, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Null, _) => {}
        (Value::Object(before), Value::Object(after)) => {
            for (key, value) in before {
                dropped_settings(
                    &format!("{path}.{key}"),
                    value,
                    after.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(before), Value::Array(after)) => {
            out.extend(
                before
                    .iter()
                    .filter(|item| !after.contains(item))
                    .map(|item| format!("{path} item {item}")),
            );
        }
        (_, Value::Null) => out.push(path.to_owned()),
        _ => {}
    }
}
fn profile_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(
        SelfConfigTarget::InferenceProfile,
        "inference_profile_id",
        patch,
    );
    fence_profile_backend(&mut request);
    request
}
fn fence_profile_backend(request: &mut ApplyRequest<'static>) {
    request.validate = Box::new(|txn, _, stored, merged| {
        Box::pin(async move {
            let Some(next) = merged.get("backend_id").and_then(Value::as_str) else {
                return Ok(());
            };
            let current = stored.get("backend_id").and_then(Value::as_str);
            if current == Some(next) {
                return Ok(());
            }
            let owner = merged
                .get("node_did")
                .and_then(Value::as_str)
                .context("profile is missing node_did")?;
            ops::guard_backend_choice_in_txn(txn, owner, current, next).await
        })
    });
}
fn profile_create_request(
    owner: String,
    profile_id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceProfile, patch);
    request.allow_create = true;
    request.require_create = true;
    request.guard_selected_chain = false;
    request.resolve_unique = Box::new(move |_| Ok(profile_id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert("profile_id".into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        Ok(())
    });
    fence_profile_backend(&mut request);
    request
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
        "compaction" => {
            let mut request =
                anchored_request(SelfConfigTarget::Compaction, "compaction_id", patch);
            fence_profile_pick(&mut request);
            request
        }
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
            guard_backend_auth(&stored, &merged)?;
            backend.validate()
        })
    });
    request.guard = Box::new(|_, _, merged| {
        anyhow::ensure!(
            merged.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout guard: backend must remain enabled"
        );
        Ok(())
    });
    request
}
fn local_backend_create_request(
    owner: String,
    backend_id: String,
    endpoint: String,
    name: Option<String>,
    wire_api: Option<crate::openai_wire::OpenAiWireApi>,
) -> ApplyRequest<'static> {
    let mut patch = vec![
        (
            "name".into(),
            Some(json!(name.unwrap_or_else(|| format!("Local {backend_id}")))),
        ),
        (
            "provider_kind".into(),
            Some(json!(crate::BackendProviderKind::OpenAiCompatible)),
        ),
        ("endpoint".into(), Some(json!(endpoint))),
        ("auth".into(), Some(json!({"kind": "unauthenticated"}))),
    ];
    if let Some(wire_api) = wire_api {
        patch.push(("openai_wire_api".into(), Some(json!(wire_api))));
    }
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceBackend, patch);
    request.allow_create = true;
    request.require_create = true;
    request.guard_selected_chain = false;
    request.resolve_unique = Box::new(move |_| Ok(backend_id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert("backend_id".into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        Ok(())
    });
    request.validate = Box::new(move |_, _, _, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            let backend: crate::InferenceBackend = decode_merged("InferenceBackend", &merged)?;
            anyhow::ensure!(
                backend.provider_kind == crate::BackendProviderKind::OpenAiCompatible
                    && matches!(
                        backend.auth,
                        crate::document_config::BackendAuth::Unauthenticated
                    ),
                "model-facing backend creation is limited to unauthenticated OpenAI-compatible local servers"
            );
            anyhow::ensure!(
                backend.enabled,
                "a newly created local backend must be enabled for discovery"
            );
            backend.validate()
        })
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
    let owner = core.node_did().to_owned();
    let agent = core.agent_id().to_owned();
    let mut request = ApplyRequest::new(target, patch);
    request.allow_create = true;
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert(target.unique_field().into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        if target == SelfConfigTarget::Task {
            merged.insert("agent_id".into(), json!(agent));
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
                    merged.get("agent_id").and_then(Value::as_str) == Some(core.agent_id()),
                    "task belongs to agent {:?}, not {:?}; retry with options.agent set to its owner; to run a different agent, create a separate task",
                    merged.get("agent_id").and_then(Value::as_str),
                    core.agent_id()
                );
            }
            // Packs may publish an event source before installing its schema;
            // a collection missing here is a typo that would never fire.
            if target == SelfConfigTarget::EventSource {
                if let Some(collection) = merged.get("source_collection").and_then(Value::as_str) {
                    anyhow::ensure!(
                        crate::config_client::collection_is_installed(txn, collection).await?,
                        "event source source_collection {collection:?} is not an installed collection; install its schema first with the schema tool (argv:[\"help\",\"collection\"])"
                    );
                }
            }
            if target == SelfConfigTarget::Trigger {
                for doc in [&stored, &merged].into_iter().filter(|doc| !doc.is_empty()) {
                    let task = doc
                        .get("task_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("Trigger.task_id must name an existing Task; inspect tasks with {{\"argv\":[\"task\",\"list\"]}}"))?;
                    anyhow::ensure!(
                        core.task_owned(txn, anchor, task).await?,
                        "Trigger.task_id {task:?} must reference a Task owned by selected agent {:?}. Inspect it with {{\"argv\":[\"task\",\"get\"],\"target_id\":{task:?}}}. Create a missing Task first with options.agent set to that owner. An existing Trigger cannot switch task owners; create a separate Trigger for a different owner and preserve the current workflow until its replacement is valid",
                        core.agent_id()
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

/// Model-facing tri-state for agent edits. A present JSON null is `Clear`;
/// the property must be absent to stay `Omitted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StringUpdate {
    #[default]
    Omitted,
    Clear,
    Set(String),
}

impl<'de> serde::Deserialize<'de> for StringUpdate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl StringUpdate {
    fn is_present(&self) -> bool {
        !matches!(self, Self::Omitted)
    }

    fn value(&self) -> Option<&str> {
        match self {
            Self::Set(value) => Some(value),
            Self::Omitted | Self::Clear => None,
        }
    }

    fn owned_value(&self) -> Option<String> {
        self.value().map(ToOwned::to_owned)
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureAgentParams {
    pub action: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub display_name: StringUpdate,
    #[serde(default)]
    pub description: StringUpdate,
    #[serde(default)]
    pub system_prompt: StringUpdate,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub clone_from: Option<String>,
    #[serde(default)]
    pub root: StringUpdate,
    #[serde(default)]
    pub preset: StringUpdate,
    #[serde(default)]
    pub profile_id: StringUpdate,
    #[serde(default)]
    pub make_default: bool,
    #[serde(default)]
    pub enable_lsp: Option<bool>,
    #[serde(default)]
    pub network_mode: Option<crate::toolset::CommandNetworkMode>,
}

async fn node_default_agent(node: &EmbeddedNode, node_did: &str) -> Result<Option<String>> {
    let query = format!(
        r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{ default_agent_id }} }}"#,
        escape_graphql_string(node_did)
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "self_config.default_agent")
            .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("Node"))
        .and_then(Value::as_array)
        .context("Node query returned no rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous node identity");
    Ok(rows
        .first()
        .and_then(|row| row.get("default_agent_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn agent_snapshot(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    protected: bool,
    default_agent_id: Option<&str>,
) -> Result<Value> {
    let effective = SelfConfigCore::new(node.clone(), node_did.to_owned(), agent_id.to_owned())?
        .with_process_ceiling(process_ceiling.clone())
        .read_effective_config(&BTreeSet::new(), false, false)
        .await?;
    Ok(json!({
        "agent_id": agent_id,
        "protected": protected,
        "is_default": default_agent_id == Some(agent_id),
        "effective_config": effective,
    }))
}

async fn agent_list(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    limit: usize,
    cursor: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        (1..=50).contains(&limit),
        "--limit must be between 1 and 50"
    );
    let catalog = load_agent_catalog(node, node_did).await?;
    let default_agent_id = node_default_agent(node, node_did).await?;
    let total = catalog.agents.len();
    let selected = catalog
        .agents
        .iter()
        .filter(|(agent_id, _)| cursor.is_none_or(|cursor| agent_id.as_str() > cursor))
        .take(limit + 1)
        .collect::<Vec<_>>();
    let truncated = selected.len() > limit;
    let mut agents = Vec::new();
    for (agent_id, reference) in selected.into_iter().take(limit) {
        let snapshot = match agent_snapshot(
            node,
            node_did,
            agent_id,
            process_ceiling,
            catalog.protected_ids.contains(agent_id),
            default_agent_id.as_deref(),
        )
        .await
        {
            Ok(snapshot) => {
                let effective = &snapshot["effective_config"];
                json!({
                    "agent_id": agent_id,
                    "display_name": effective.pointer("/agent/display_name"),
                    "description": effective.pointer("/agent/description"),
                    "enabled": *reference,
                    "protected": catalog.protected_ids.contains(agent_id),
                    "is_default": default_agent_id.as_deref() == Some(agent_id.as_str()),
                    "context_id": effective.pointer("/agent/context_id"),
                    "profile_id": effective.pointer("/agent/inference_profile_id"),
                    "backend_id": effective.pointer("/inference_profile/backend_id"),
                    "model_name": effective.pointer("/inference_profile/model_name"),
                    "reasoning_effort": effective.pointer("/inference_profile/reasoning_effort"),
                    "service_tier": effective.pointer("/inference_profile/service_tier"),
                })
            }
            Err(error) => json!({
                "agent_id": agent_id,
                "enabled": *reference,
                "protected": catalog.protected_ids.contains(agent_id),
                "is_default": default_agent_id.as_deref() == Some(agent_id.as_str()),
                "configuration_error": format!("{error:#}"),
            }),
        };
        agents.push(snapshot);
    }
    let next_cursor = truncated
        .then(|| {
            agents
                .last()?
                .get("agent_id")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .flatten();
    let snapshot = json!({
        "page": {
            "limit": limit,
            "total": total,
            "returned": agents.len(),
            "truncated": truncated,
            "next_cursor": next_cursor,
        },
        "process_ceiling": process_ceiling,
        "permission_presets": builtin_preset_names(),
        "available_profile_ids": catalog.published_profiles,
        "default_agent_id": default_agent_id,
        "agents": agents,
        "activation": {
            "durable_config": "after the admitted transaction commits",
            "running_generation": "after the runtime reconciler generation swap",
            "session": "select the agent in a new session; existing sessions remain bound to their original agent",
            "pairing_and_restart": "canonical documents replicate to authorized paired clients and are reloaded after restart",
        },
    });
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

async fn agent_inspect(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let catalog = load_agent_catalog(node, node_did).await?;
    let _ = catalog.agents.get(agent_id).with_context(|| {
        format!("unknown agent_id {agent_id:?}; copy an exact ID from [\"agent\",\"list\"]")
    })?;
    let default_agent_id = node_default_agent(node, node_did).await?;
    let snapshot = agent_snapshot(
        node,
        node_did,
        agent_id,
        process_ceiling,
        catalog.protected_ids.contains(agent_id),
        default_agent_id.as_deref(),
    )
    .await?;
    let Value::Object(mut snapshot) = snapshot else {
        bail!("agent snapshot must be an object");
    };
    let effective = snapshot.remove("effective_config").unwrap_or_default();
    let mut result = Ordered::reading_order(
        Value::Object(snapshot),
        &["agent_id", "is_default", "protected"],
    );
    result.push(Ordered::entry(
        "effective_config",
        &Ordered::reading_order(effective, EFFECTIVE_ORDER),
    ));
    result.pretty()
}

async fn guard_agent_profile_choice(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    current_agent: Option<&str>,
    clone_from: Option<&str>,
    next_profile: Option<&str>,
) -> Result<()> {
    let Some(next_profile) = next_profile else {
        return Ok(());
    };
    let current_profile = match current_agent {
        Some(agent_id) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, agent_id)
            .await?
            .and_then(|(_, doc)| {
                doc.get("inference_profile_id")?
                    .as_str()
                    .map(ToOwned::to_owned)
            }),
        None => None,
    };
    let current_backend = match current_profile {
        Some(profile) => Some(ops::profile_backend_id(txn, node_did, &profile).await?),
        None => None,
    };
    let next_backend = ops::profile_backend_id(txn, node_did, next_profile).await?;
    ops::guard_backend_choice_in_txn(txn, node_did, current_backend.as_deref(), &next_backend)
        .await?;
    let source = match clone_from {
        Some(id) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, id).await?,
        None => None,
    };
    let Some((_, source)) = source else {
        return Ok(());
    };
    let own =
        serde_json::Map::from_iter([("inference_profile_id".to_owned(), json!(next_profile))]);
    let mut cloned = own.clone();
    if let Some(context) = source.get("context_id") {
        cloned.insert("context_id".to_owned(), context.clone());
    }
    let anchor = ops::AgentAnchor {
        doc: source,
        context: serde_json::Map::new(),
        profile: serde_json::Map::new(),
        execution: serde_json::Map::new(),
    };
    ops::guard_compaction_choice_in_txn(txn, SelfConfigTarget::Agent, &anchor, &own, &cloned).await
}

/// Install document and plugins-only packs from the home's pack store or the registry through the
/// canonical resolver, package publication, and activation owners. The running
/// node is always the install owner; callers cannot select another DID,
/// filesystem distribution, registry endpoint, or control-plane endpoint.
struct PackInstaller {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
    /// `None` resolves from the registry in memory and installs no plugins.
    home: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
struct PackInstallParams {
    pub package: String,
    pub variables: BTreeMap<String, String>,
    pub inference_slots: crate::pack::PackInferenceBindings,
    pub expected_digest: Option<String>,
}

struct ConfigPackDistribution(crate::pack_resolve::ResolvedNamedPack);

impl ConfigPackDistribution {
    fn manifest(&self) -> &crate::pack::PackManifest {
        self.0.archive.manifest()
    }

    fn digest(&self) -> &str {
        self.0.archive.digest()
    }

    fn source(&self) -> &'static str {
        match self.0.from {
            crate::pack_resolve::ResolvedFrom::Installed => "installed",
            crate::pack_resolve::ResolvedFrom::Store => "store",
            crate::pack_resolve::ResolvedFrom::Registry { .. } => "registry",
        }
    }

    /// Pin the preview's resolution so applying it resolves the same pack
    /// again rather than whatever is newest by then.
    fn pinned_spec(&self, requested: &str) -> String {
        if crate::pack::local::classify(requested).is_some() {
            return self.digest().to_owned();
        }
        let manifest = self.manifest();
        if crate::pack_store::is_valid_index_version(&manifest.version) {
            format!(
                "{}/{}@{}",
                manifest.metadata.namespace, manifest.name, manifest.version
            )
        } else {
            requested.to_owned()
        }
    }

    fn registry_artifact_digest(&self) -> Option<&str> {
        match &self.0.from {
            crate::pack_resolve::ResolvedFrom::Registry {
                artifact_digest, ..
            } => Some(artifact_digest),
            _ => None,
        }
    }
}

impl PackInstaller {
    fn validate(&self, args: &PackInstallParams) -> anyhow::Result<()> {
        let coordinate = args.package.trim();
        anyhow::ensure!(!coordinate.is_empty(), "pack name must not be blank");
        if crate::pack::local::classify(coordinate).is_none() {
            crate::pack_resolve::parse_pack_spec(coordinate)?;
        }
        anyhow::ensure!(
            args.variables.len() <= 32,
            "pack variables exceed the 32-entry limit"
        );
        for (name, value) in &args.variables {
            anyhow::ensure!(
                name != "GENTS_PACK_NODE_DID"
                    && !name.is_empty()
                    && name.len() <= 128
                    && name.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    }),
                "invalid or protected pack variable name {name:?}"
            );
            anyhow::ensure!(
                !name.ends_with("_MODEL") && !name.ends_with("_ENDPOINT"),
                "inference variable {name:?} is unsupported; bind declared roles with --inference-slot NAME=PROFILE_ID"
            );
            anyhow::ensure!(value.len() <= 4096, "pack variable {name:?} is too large");
        }
        anyhow::ensure!(
            args.inference_slots.len() <= 32,
            "pack inference slot bindings exceed the 32-entry limit"
        );
        Ok(())
    }

    async fn resolve(
        &self,
        operation: &str,
        args: &PackInstallParams,
    ) -> anyhow::Result<ConfigPackDistribution> {
        self.validate(args)?;
        let coordinate = args.package.trim();
        if let Some(local) = crate::pack::local::classify(coordinate) {
            let home = self
                .home
                .as_deref()
                .context("local packs require a runtime plugin home")?;
            let archive = match local {
                crate::pack::local::LocalName::Digest(_) => crate::pack::local::open(&local, home)?,
                crate::pack::local::LocalName::Path(path) => {
                    // Effective authority, not the process ceiling alone: a
                    // narrower agent selection (or WorkspaceRoot policy) must
                    // bound what this inspection can read.
                    let effective = self
                        .core
                        .read_effective_config(&BTreeSet::new(), false, false)
                        .await?;
                    let mode = effective
                        .pointer("/runtime_effective/effective/file_mode")
                        .and_then(Value::as_str)
                        .map(crate::tool_surface::FileToolMode::parse)
                        .transpose()?
                        .unwrap_or_default();
                    anyhow::ensure!(mode != crate::tool_surface::FileToolMode::Off, "local pack inspection requires effective file read permission on the invoking agent");
                    let root = effective
                        .pointer("/runtime_effective/effective/root")
                        .and_then(Value::as_str)
                        .context("local pack inspection requires an effective tool root")?;
                    let context = crate::toolset::ToolContext::new(root.into(), false)?;
                    let path = context
                        .resolve_existing_file(path.to_str().context("pack path is not UTF-8")?)?;
                    anyhow::ensure!(path.extension().is_some_and(|extension| extension == crate::pack_archive::EXTENSION), "native local inspection accepts a built .pack artifact; directories require a scoped build owner");
                    // The no-follow import carries the same handle the
                    // admission validated, so a swap after admission cannot
                    // change which bytes are verified and stored.
                    crate::pack::local::open(&crate::pack::local::LocalName::Path(&path), home)?
                }
            };
            // Local built artifacts flow through the same canonical
            // installers as registry-resolved packs: documents through
            // prepare/install_prepared_document_pack, plugins-only through
            // plugins_pack. Plugin authority is unchanged: preview reports
            // installation_blockers and commit refuses them unless the
            // operator grants authority, so a locally built pack (e.g. one
            // declaring child_process or bind_dir capabilities) cannot
            // silently authorize itself.
            anyhow::ensure!(
                matches!(
                    archive.manifest().metadata.kind,
                    crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
                ),
                "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
                archive.manifest().name,
                archive.manifest().metadata.kind
            );
            return Ok(ConfigPackDistribution(
                crate::pack_resolve::ResolvedNamedPack {
                    archive,
                    from: crate::pack_resolve::ResolvedFrom::Store,
                },
            ));
        }
        let registry_url = crate::pack_registry::resolve_registry_url(None);
        let spec = match crate::pack_resolve::parse_pack_spec(coordinate)? {
            parsed if operation == "update" && parsed.version.is_none() => {
                let client = crate::pack_registry::RegistryClient::new(registry_url.clone());
                let latest = crate::pack_registry::resolve_pack_coordinate(
                    &client,
                    parsed.namespace,
                    parsed.name,
                    None,
                )
                .await
                .with_context(|| {
                    format!(
                        "the newest version of {coordinate} could not be looked up on the registry at {registry_url}"
                    )
                })?;
                format!("{}/{}@{}", parsed.namespace, parsed.name, latest.version)
            }
            _ => coordinate.to_owned(),
        };
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let installed = crate::pack::installed_packs(
            self.home.as_deref(),
            Some((&access, self.core.node_did())),
        )
        .await?;
        let resolved = crate::pack_resolve::resolve_named(
            &spec,
            &crate::pack_resolve::ResolveOptions {
                home: self.home.as_deref(),
                registry_url,
                installed: &installed,
            },
        )
        .await?;
        let distribution = ConfigPackDistribution(resolved);
        anyhow::ensure!(
            operation == "get"
                || matches!(
                    distribution.manifest().metadata.kind,
                    crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
                ),
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        );
        Ok(distribution)
    }

    async fn installation_status(
        &self,
        manifest: &crate::pack::PackManifest,
    ) -> anyhow::Result<Option<Value>> {
        if manifest.metadata.kind == crate::pack::PackKind::Plugins {
            return self
                .home
                .as_deref()
                .map(|home| {
                    crate::pack::read_home_install(
                        home,
                        &format!("{}/{}", manifest.metadata.namespace, manifest.name),
                    )
                })
                .transpose()?
                .flatten()
                .map(serde_json::to_value)
                .transpose()
                .map_err(Into::into);
        }
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        crate::pack::read_installed_pack(&access, self.core.node_did(), &coordinate)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn documents(
        &self,
        operation: &str,
        args: &PackInstallParams,
        distribution: &ConfigPackDistribution,
        commit: bool,
    ) -> anyhow::Result<String> {
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let owner = self.core.node_did();
        let prior = self.installation_status(distribution.manifest()).await?;
        anyhow::ensure!(
            operation != "update" || prior.is_some(),
            "pack {:?} is not installed; use pack install",
            distribution.manifest().name
        );
        let mut dependencies = Vec::new();
        for coordinate in &distribution.manifest().metadata.dependencies {
            dependencies.push(
                self.resolve(
                    "get",
                    &PackInstallParams {
                        package: coordinate.clone(),
                        variables: args.variables.clone(),
                        inference_slots: BTreeMap::new(),
                        expected_digest: None,
                    },
                )
                .await?,
            );
        }
        let mut blockers = self.plugin_install_blockers(distribution.manifest());
        for dependency in &dependencies {
            blockers.extend(self.plugin_install_blockers(dependency.manifest()));
        }
        for slot in args.inference_slots.keys() {
            anyhow::ensure!(
                std::iter::once(distribution.manifest())
                    .chain(dependencies.iter().map(|d| d.manifest()))
                    .any(|manifest| manifest
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| &declared.name == slot)),
                "pack and its dependencies have no inference slot {slot:?}"
            );
        }
        let mut missing_slots = std::collections::BTreeSet::new();
        let mut inspected = Vec::new();
        for manifest in std::iter::once(distribution.manifest())
            .chain(dependencies.iter().map(|d| d.manifest()))
        {
            let requested = args
                .inference_slots
                .iter()
                .filter(|(slot, _)| {
                    manifest
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| &declared.name == *slot)
                })
                .map(|(slot, profile)| (slot.clone(), profile.clone()))
                .collect();
            let preview =
                crate::pack::inspect_pack_inference_bindings(&access, manifest, owner, &requested)
                    .await?;
            for slot in &preview.slots {
                if !slot.optional && !args.inference_slots.contains_key(&slot.name) {
                    missing_slots.insert(slot.name.clone());
                }
            }
            inspected.push(preview);
        }
        if !commit && !missing_slots.is_empty() {
            // Incomplete previews still must reject unauthorized supplied bindings.
            // Complete previews and installs retain the prepared-binding fence below.
            for preview in &inspected {
                self.fence_bindings(&preview.bindings).await?;
            }
            return Ok(serde_json::to_string_pretty(&json!({
                "committed": false, "ready": false, "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "inference": inspected, "missing_inference_slots": missing_slots, "installed": prior,
                "next": "repeat --inference-slot NAME=PROFILE_ID for every missing slot, using exact eligible profile IDs",
            }))?);
        }
        let scope = crate::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        };
        let environment = |name: &str| args.variables.get(name).cloned();
        let authored = crate::pack::load_pack_config(
            distribution.manifest(),
            &scope,
            &|path| {
                distribution
                    .0
                    .archive
                    .asset(path)
                    .map(|asset| asset.to_vec())
            },
            &environment,
        )?;
        let prepared = crate::pack::prepare_document_pack_install(
            &access,
            owner,
            distribution.manifest(),
            &authored,
            &args.inference_slots,
            &dependencies
                .iter()
                .map(|d| &d.0.archive)
                .collect::<Vec<_>>(),
            crate::pack::InstalledVia::SelfConfig,
            self.home.as_deref(),
            Some(&distribution.0.archive),
            &environment,
        )
        .await?;
        // Inference slot bindings pick provider accounts exactly like the
        // install lifecycle: every selected profile must be account-free or on
        // its provider's default account. Fence before the preview return or
        // any installation side effect so a documents pack cannot publish
        // records or plugins through a binding an install would refuse.
        self.fence_bindings(&prepared.inference.bindings).await?;
        for dependency in &prepared.dependencies {
            self.fence_bindings(&dependency.inference.bindings).await?;
        }
        let mut schemas = Vec::new();
        for path in crate::pack::document_pack_schema_paths(distribution.manifest())? {
            let sdl = std::str::from_utf8(distribution.0.archive.asset(path)?)?;
            schemas.push(crate::config_client::preview_schema_install(&access, sdl).await?);
        }
        let dependency_receipts = dependencies.iter().map(|dependency| json!({
            "coordinate": format!("{}/{}", dependency.manifest().metadata.namespace, dependency.manifest().name),
            "version": dependency.manifest().version, "artifact_digest": dependency.digest(),
        })).collect::<Vec<_>>();
        if !commit {
            return Ok(serde_json::to_string_pretty(&json!({
                "committed": false, "ready": blockers.is_empty(), "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "registry_artifact_digest": distribution.registry_artifact_digest(),
                "inference": prepared.inference, "dependency_inference": prepared.dependency_inference,
                "dependencies": dependency_receipts, "schemas": schemas, "installation_blockers": blockers,
                "installed": prior,
                "materialized_ids": crate::pack::pack_document_digests(&prepared.config)?.into_keys().map(|(collection, id)| json!({"collection":collection,"id":id})).collect::<Vec<_>>(),
                "apply_with": blockers.is_empty().then(|| json!({
                    "argv_prefix": ["pack", operation, distribution.pinned_spec(&args.package), "--digest", distribution.digest()],
                    "repeat_inference_slots": args.inference_slots, "repeat_variables": args.variables,
                })),
            }))?);
        }
        anyhow::ensure!(
            blockers.is_empty(),
            "pack plugins require operator authorization: {blockers:?}"
        );
        let receipt = crate::pack::install_prepared_document_pack(
            &access,
            owner,
            &distribution.0.archive,
            &dependencies
                .iter()
                .map(|d| &d.0.archive)
                .collect::<Vec<_>>(),
            &prepared,
            self.home.as_deref(),
            false,
            crate::pack::InstalledVia::SelfConfig,
            crate::pack::DriftPolicy::Refuse,
        )
        .await?;
        let effective = self
            .installation_status(distribution.manifest())
            .await?
            .context("installed document pack is not discoverable")?;
        anyhow::ensure!(
            effective["digest"].as_str() == Some(distribution.digest()),
            "installed document pack digest does not match requested artifact"
        );
        Ok(serde_json::to_string_pretty(&json!({
            "operation": operation, "source": distribution.source(), "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(), "install": receipt,
            "inference": prepared.inference, "dependency_inference": prepared.dependency_inference,
            "dependencies": dependency_receipts, "effective": effective,
            "effect": "The document pack is installed.",
        }))?)
    }

    async fn list(&self, limit: usize, cursor: Option<&str>) -> anyhow::Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let store = self.home.as_deref().map(crate::pack_store::PackStore::new);
        let names = match &store {
            Some(store) => store.names()?,
            None => Vec::new(),
        };
        let total = names.len();
        let mut selected = names
            .into_iter()
            .filter(|(coordinate, _)| cursor.is_none_or(|cursor| coordinate.as_str() > cursor))
            .take(limit + 1)
            .collect::<Vec<_>>();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        let next_cursor = truncated
            .then(|| selected.last().map(|(coordinate, _)| coordinate.clone()))
            .flatten();
        let mut items = Vec::with_capacity(selected.len());
        for (coordinate, versions) in selected {
            let (Some(store), Some(preferred)) = (&store, versions.first()) else {
                continue;
            };
            let archive = match store.open(&preferred.digest) {
                Ok(archive) => archive,
                Err(error) => {
                    tracing::warn!(pack = %coordinate, %error, "a stored pack archive could not be opened");
                    items.push(json!({
                        "name": coordinate,
                        "version": preferred.version,
                        "versions": versions.iter().map(|stored| stored.version.as_str()).collect::<Vec<_>>(),
                        "artifact_digest": preferred.digest,
                        "installable": false,
                        "error": format!("the stored archive could not be opened: {error:#}"),
                    }));
                    continue;
                }
            };
            let manifest = archive.manifest();
            let installable = matches!(
                manifest.metadata.kind,
                crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
            );
            let installed = if installable {
                self.installation_status(manifest).await?
            } else {
                None
            };
            items.push(json!({
                "name": coordinate,
                "version": preferred.version,
                "versions": versions.iter().map(|stored| stored.version.as_str()).collect::<Vec<_>>(),
                "description": manifest.description,
                "kind": manifest.metadata.kind,
                "artifact_digest": preferred.digest,
                "inference_slots": manifest.metadata.inference_slots,
                "installable": installable,
                "installed": installed,
            }));
        }
        Ok(serde_json::to_string_pretty(&json!({
            "source": "store",
            "page": {
                "limit": limit,
                "total": total,
                "returned": items.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            },
            "items": items,
            "registry_lookup": "Use pack search to discover registry packages, then pack get NAMESPACE/NAME to inspect one. List shows only packs this home's store already holds.",
        }))?)
    }

    async fn search(&self, query: &str, page: u32) -> anyhow::Result<String> {
        let registry = crate::pack_registry::resolve_registry_url(None);
        let response = crate::pack_registry::RegistryClient::new(registry.clone())
            .search(query, page)
            .await?;
        let packs = response["packs"]
            .as_array()
            .context("registry search returned no packs array")?;
        let items = packs
            .iter()
            .map(|pack| {
                let mut item = pack.clone();
                if let (Some(namespace), Some(name)) =
                    (pack["namespace"].as_str(), pack["name"].as_str())
                {
                    item["inspect"] = json!({"argv":["pack","get",format!("{namespace}/{name}")]});
                }
                item
            })
            .collect::<Vec<_>>();
        let has_more = response["has_more"].as_bool().unwrap_or(false);
        ordered! {
            "items": items,
            "next_call": if has_more { page.checked_add(1).map(|next| json!({"argv":["pack","search"],"options":{"query":query,"page":next}})) } else { None },
            "guidance": "Inspect a candidate with pack get before installation: it reports its kind, inference slots and installed state. This installer supports document and plugins-only packs; search includes other kinds too.",
            "registry": registry,
            "page": page,
            "has_more": has_more,
        }.pretty()
    }

    async fn get(&self, package: &str) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        let distribution = self.resolve("get", &args).await?;
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let inference = crate::pack::inspect_pack_inference_bindings(
            &access,
            distribution.manifest(),
            self.core.node_did(),
            &BTreeMap::new(),
        )
        .await?;
        let installable = matches!(
            distribution.manifest().metadata.kind,
            crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
        );
        let installed = if installable {
            self.installation_status(distribution.manifest()).await?
        } else {
            None
        };
        Ok(serde_json::to_string_pretty(&json!({
            "source": distribution.source(),
            "manifest": distribution.manifest(),
            "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "installed": installed,
            "installable": installable,
            "installation_blockers": self.plugin_install_blockers(distribution.manifest()),
            "supported_operations": if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
                Some(vec!["preview install", "install", "preview update", "update"])
            } else { installable.then_some(vec!["preview install", "install", "preview update", "update", "remove"]) },
        }))?)
    }

    /// Removes an installed pack's `PackInstallation` record and everything
    /// it lists. Never resolves the package (no
    /// store or registry lookup); releases its home and plugin store records
    /// but no plugin bytes or archive; use `gents pack remove` for that.
    async fn remove(&self, package: &str) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        self.validate(&args)?;
        let spec = crate::pack_resolve::parse_pack_spec(package.trim())?;
        anyhow::ensure!(
            spec.version.is_none(),
            "pack remove takes a coordinate without a version; remove {}/{}",
            spec.namespace,
            spec.name
        );
        let coordinate = format!("{}/{}", spec.namespace, spec.name);
        if let Some(home) = self.home.as_deref() {
            if let Some(record) = crate::pack::read_home_install(home, &coordinate)? {
                anyhow::ensure!(!matches!(record.kind, crate::pack::PackKind::Assets | crate::pack::PackKind::Plugins), "native removal of home-installed plugin/assets packs is unavailable; the existing pack removal owner must release plugin and home records together. Current installation is unchanged");
            }
        }
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let report = crate::pack::remove_pack(
            &access,
            self.core.node_did(),
            &coordinate,
            crate::pack::DriftPolicy::Refuse,
        )
        .await?;
        if let Some(home) = self.home.as_deref() {
            crate::pack::forget_home_install(home, &coordinate)?;
            for removed in std::iter::once(&report).chain(&report.dependencies) {
                crate::pack::plugin_lifecycle::release_plugin_records(
                    home,
                    &removed.pack,
                    &removed.documents.plugins,
                )?;
            }
        }
        ordered! {
            "pack": coordinate,
            "removed": report,
            "effect": "The package's documents were removed. Package SDL schemas stay.",
        }
        .pretty()
    }

    fn plugin_install_blockers(&self, manifest: &crate::pack::PackManifest) -> Vec<Value> {
        manifest
            .metadata
            .plugins
            .iter()
            .filter_map(|plugin| {
                let result = self
                    .home
                    .as_deref()
                    .context("this runtime has no plugin home")
                    .and_then(|home| {
                        crate::plugin::store::grant_on_install(
                            home,
                            &manifest.metadata.namespace,
                            plugin,
                            false,
                        )
                    });
                result
                    .err()
                    .map(|error| json!({"plugin":plugin.name,"reason":format!("{error:#}")}))
            })
            .collect()
    }

    /// Installs the plugins `distribution` ships into the home's plugin store,
    /// sealed: a plugin that asks for authority is refused, because granting
    /// it is the operator's call. Local plugin records are the publication
    /// boundary; later failures do not roll them back.
    /// Pack slots have no current backend: each bound profile must be
    /// account-free or on its provider's default account.
    async fn fence_bindings(
        &self,
        bindings: &crate::pack::PackInferenceBindings,
    ) -> anyhow::Result<()> {
        let owner = self.core.node_did();
        crate::config_client::ConfigAccess::transact_local_write(
            &self.node,
            Some(self.core.identity()?),
            "self_config.pack_profile_choice",
            |txn| {
                Box::pin(async move {
                    for profile in bindings.values() {
                        let backend = ops::profile_backend_id(txn, owner, profile).await?;
                        ops::guard_backend_choice_in_txn(txn, owner, None, &backend).await?;
                    }
                    Ok(())
                })
            },
        )
        .await
    }

    async fn preview(&self, operation: &str, args: PackInstallParams) -> anyhow::Result<String> {
        let distribution = self.resolve(operation, &args).await?;
        if let Some(expected) = args.expected_digest.as_deref() {
            anyhow::ensure!(
                expected == distribution.digest(),
                "pack digest changed: requested {expected:?}, resolved {:?}; preview again",
                distribution.digest()
            );
        }
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
            return self
                .plugins_pack(operation, &args, &distribution, false)
                .await;
        }
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Documents {
            return self.documents(operation, &args, &distribution, false).await;
        }
        anyhow::bail!(
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        )
    }

    async fn apply(&self, operation: &str, args: PackInstallParams) -> anyhow::Result<String> {
        let distribution = self.resolve(operation, &args).await?;
        let expected = args.expected_digest.as_deref().context(
            "pack install/update requires --digest from config pack preview; preview pins the exact artifact being authorized",
        )?;
        anyhow::ensure!(
            expected == distribution.digest(),
            "pack digest changed: preview authorized {expected:?}, resolved {:?}; preview again",
            distribution.digest()
        );
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
            return self
                .plugins_pack(operation, &args, &distribution, true)
                .await;
        }
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Documents {
            return self.documents(operation, &args, &distribution, true).await;
        }
        anyhow::bail!(
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        )
    }
}

pub fn build_self_config_tools(
    usage_cache: Arc<crate::usage_observation::UsageCache>,
    node: Arc<EmbeddedNode>,
    node_did: String,
    identity: Option<Arc<dyn NodeIdentity>>,
    config: &SelfConfigToolConfig,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) -> Vec<Box<dyn ToolDyn>> {
    if !config.enabled {
        return Vec::new();
    }
    let core = match SelfConfigCore::new(node.clone(), node_did.clone(), config.agent_id.clone()) {
        Ok(core) => core
            .with_no_lockout(config.no_lockout)
            .with_process_ceiling(config.process_ceiling.clone())
            .with_held_grants(OperatorGrants {
                pack_install: config.enable_pack_install,
                ..Default::default()
            }),
        Err(error) => {
            tracing::warn!(
                agent_id = %config.agent_id,
                %error,
                "self-config tools requested but not registrable; failing closed"
            );
            return Vec::new();
        }
    };

    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    if !config.enabled {
        return tools;
    }
    tools.push(Box::new(command::ConfigCommandTool {
        usage_cache,
        node,
        node_did,
        identity,
        core,
        categories: config.categories.clone(),
        no_lockout: config.no_lockout,
        preview: config.preview,
        process_ceiling: config.process_ceiling.clone(),
        execution: Arc::new(execution::ExecutionObservation::default()),
        plugins,
    }));
    tools
}

pub fn self_config_tool_names(config: &SelfConfigToolConfig) -> Vec<String> {
    let mut names = Vec::new();
    if !config.enabled {
        return names;
    }
    names.push(CONFIG_TOOL_NAME.to_string());
    names
}
