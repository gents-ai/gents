//! Principal-scoped self-configuration through canonical context and inference documents.
//! Patches name explicit writable fields and commit through the common desired-state
//! transaction after validating the complete retained configuration. Nested tool
//! permissions and auth references retain their existing typed owners. Raw API keys
//! cannot be changed or returned. Optional no-lockout checks the candidate config
//! chain. Persona requests reuse the existing signed admission and reconciliation path.

mod command;
pub(crate) use command::{is_help_call, ConfigCommandParams};
mod execution;
pub use execution::ConfigExecutionReceipt;
mod graph_preview;
mod ops;
mod read;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;

pub use ops::{
    apply_tool_grant_selection, guard_behavior_keeps_reach, guard_tools_keep_control,
    validate_tool_network_selection, PatchOutcome, SelfConfigCore, EFFECT_TIMING_NOTE,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

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
use ops::{decode_merged, validate_merged_selection, ApplyRequest};

pub const CONFIG_TOOL_NAME: &str = "config";
pub const PREVIEW_GRAPH_TOOL_NAME: &str = "preview_graph";
pub const LIST_GRAPHS_TOOL_NAME: &str = "list_graphs";
pub const RUN_GRAPH_TOOL_NAME: &str = "run_graph";
pub const GET_GRAPH_RUN_TOOL_NAME: &str = "get_graph_run";
pub const GET_GRAPH_RESULT_TOOL_NAME: &str = "get_graph_result";
pub const CANCEL_GRAPH_RUN_TOOL_NAME: &str = "cancel_graph_run";

/// Model-facing names reserved by the runtime. Configuration is one coherent
/// argv-style surface; graph execution remains a separate operational surface.
pub const SELF_CONFIG_TOOL_NAMES: [&str; 7] = [
    CONFIG_TOOL_NAME,
    PREVIEW_GRAPH_TOOL_NAME,
    LIST_GRAPHS_TOOL_NAME,
    RUN_GRAPH_TOOL_NAME,
    GET_GRAPH_RUN_TOOL_NAME,
    GET_GRAPH_RESULT_TOOL_NAME,
    CANCEL_GRAPH_RUN_TOOL_NAME,
];

/// A JSON object that keeps its keys in the order given, for model-facing
/// results that must read top to bottom: the answer, then how to proceed, then
/// metadata. This build's `serde_json::Map` sorts keys (`preserve_order` is
/// off, and canonical digests such as `desired_state_document_digest` rely on
/// sorted keys), so `json!` alone would alphabetize them. Values are kept
/// pre-serialized so a nested `Ordered` keeps its order too. Null values are
/// omitted.
pub(crate) struct Ordered(
    Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
);

impl serde::Serialize for Ordered {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let present = self.0.iter().filter(|(_, value)| value.get() != "null");
        let mut map = serializer.serialize_map(Some(present.clone().count()))?;
        for (key, value) in present {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Ordered {
    pub(crate) fn entry(
        key: impl Into<std::borrow::Cow<'static, str>>,
        value: &impl serde::Serialize,
    ) -> (
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    ) {
        (
            key.into(),
            serde_json::value::to_raw_value(value).expect("model-facing results serialize"),
        )
    }

    pub(crate) fn new(
        entries: Vec<(
            std::borrow::Cow<'static, str>,
            Box<serde_json::value::RawValue>,
        )>,
    ) -> Self {
        Self(entries)
    }

    /// An object's keys with `first` in that order, then the rest.
    pub(crate) fn reading_order(value: Value, first: &[&'static str]) -> Self {
        let Value::Object(mut object) = value else {
            return Self(vec![Self::entry("value", &value)]);
        };
        let mut entries = first
            .iter()
            .filter_map(|key| object.remove(*key).map(|value| Self::entry(*key, &value)))
            .collect::<Vec<_>>();
        entries.extend(
            object
                .into_iter()
                .map(|(key, value)| Self::entry(key, &value)),
        );
        Self(entries)
    }

    pub(crate) fn pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|error| anyhow!("serialize result: {error}"))
    }
}

/// `ordered!{"key": expr, ...}`: each value is any `Serialize` expression
/// (use `json!` for literal objects).
macro_rules! ordered {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::self_config::Ordered::new(vec![$($crate::self_config::Ordered::entry($key, &$value)),*])
    };
}
pub(crate) use ordered;

/// Reading order of an effective configuration: the behavior and its chain,
/// then what it may do, then grants and timing.
const EFFECTIVE_ORDER: &[&str] = &[
    "execution_settings",
    "behavior_id",
    "behavior",
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
    request.guard = Box::new(|_, stored, merged| guard_behavior_keeps_reach(stored, merged));
    request
}

/// Model-facing patches may target any owned behavior, including the invoking
/// configurator itself; the no-lockout guard is its only self-protection
/// (Lean `SelfConfig.keepsControl`). Keep the shared-reference check inside the
/// same transaction as validation/publication so a stale preflight cannot
/// authorize a write.
fn protect_working_behavior(mut request: ApplyRequest<'static>) -> ApplyRequest<'static> {
    let target = request.target;
    let validate = request.validate;
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let validation = validate(txn, anchor, stored, merged);
        Box::pin(async move {
            // Context and Tools are reusable documents. A targeted edit must
            // not mutate another behavior (especially Setup) through a shared
            // reference. Keep the observation and rejection in the same
            // transaction as the canonical patch publication, matching the
            // Lean siblingToolsAllowed contract.
            let owner = anchor
                .doc
                .get("agent_did")
                .and_then(Value::as_str)
                .context("selected behavior is missing agent_did")?;
            let behavior_id = anchor
                .doc
                .get("behavior_id")
                .and_then(Value::as_str)
                .context("selected behavior is missing behavior_id")?;
            let context_id = anchor
                .context
                .get("context_id")
                .and_then(Value::as_str)
                .context("selected behavior context is missing context_id")?;
            let require_only_referrer = |response: &Value,
                                         collection: &str,
                                         unique: &str,
                                         expected: &str| {
                let rows = response
                    .get("data")
                    .and_then(|data| data.get(collection))
                    .and_then(Value::as_array)
                    .with_context(|| format!("{collection} reference query missing rows"))?;
                anyhow::ensure!(
                    rows.len() == 1
                        && rows[0].get(unique).and_then(Value::as_str) == Some(expected),
                    "targeted configuration requires an unshared Context and Tools; clone the working behavior before editing shared configuration"
                );
                Ok::<_, anyhow::Error>(())
            };
            if target == SelfConfigTarget::AgentContext || target == SelfConfigTarget::Tools {
                let escaped_owner = escape_graphql_string(owner);
                let escaped_context = escape_graphql_string(context_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{escaped_owner}"}}, context_id: {{_eq: "{escaped_context}"}}}}) {{behavior_id}} }}"#
                    ))
                    .await?;
                require_only_referrer(&response, "AgentBehavior", "behavior_id", behavior_id)?;
            }
            if target == SelfConfigTarget::Tools {
                let tools_id = stored
                    .get("tools_id")
                    .and_then(Value::as_str)
                    .context("selected Tools is missing tools_id")?;
                let escaped_owner = escape_graphql_string(owner);
                let escaped_tools = escape_graphql_string(tools_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentContext(filter: {{agent_did: {{_eq: "{escaped_owner}"}}, tools_id: {{_eq: "{escaped_tools}"}}}}) {{context_id}} }}"#
                    ))
                    .await?;
                require_only_referrer(&response, "AgentContext", "context_id", context_id)?;
            }
            validation.await
        })
    });
    request
}
fn tools_request(
    core: &SelfConfigCore,
    patch: SelfConfigPatch,
    allow_pack_install: bool,
) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    let ceiling_root = core.process_ceiling().root.clone();
    request.normalize = Box::new(move |txn, _, _, merged| {
        let ceiling_root = ceiling_root.clone();
        Box::pin(async move {
            let policy = crate::tool_surface::load_workspace_root_policy_in_txn(
                txn,
                ceiling_root.as_deref(),
            )
            .await?;
            let mut tools = decode_merged::<crate::document_config::Tools>("Tools", merged)?;
            crate::tool_surface::canonicalize_tools_root(&mut tools, &policy)?;
            let canonical = serde_json::to_value(tools)?
                .as_object()
                .context("canonical Tools document must be an object")?
                .clone();
            *merged = canonical;
            Ok(())
        })
    });
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
    request.guard = Box::new(|_, stored, merged| guard_tools_keep_control(stored, merged));
    request
}
/// A set of a nested Tools group replaces the whole group, so a partial group
/// silently drops what it omits. On the invoker's own Tools that loses its
/// working surfaces (the mailbox surface, defra_query) without a lockout the
/// no-lockout guard would catch, so such a patch is refused and names what it
/// would drop unless `allow_drop` names the group. This confirms intent; it
/// narrows no legal transition (Lean `SelfConfig.keepsControl` stays the only
/// self-protection). Like that guard it applies only to the invoker under
/// no_lockout, which the Engineer's grant sets.
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
    // Rides the invoker-only guard slot, after the lockout guard so a lockout
    // is reported as one.
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

/// Settings present in `before` and absent from `after`: removed keys, removed
/// array items, and values cleared to null. A changed value is a choice, not a
/// drop.
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
    anchored_request(
        SelfConfigTarget::InferenceProfile,
        "inference_profile_id",
        patch,
    )
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
        merged.insert("agent_did".into(), json!(owner));
        Ok(())
    });
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
        merged.insert("agent_did".into(), json!(owner));
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
                    "task belongs to behavior {:?}, not {:?}; retry with options.behavior set to its owner; to run a different behavior, create a separate task",
                    merged.get("behavior_id").and_then(Value::as_str),
                    core.behavior_id()
                );
            }
            // Packs may publish an event source before installing its schema;
            // the schema tool installs schemas first, so a
            // collection it cannot see here is a typo that would never fire.
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
                        "Trigger.task_id {task:?} must reference a Task owned by selected behavior {:?}. Inspect it with {{\"argv\":[\"task\",\"get\"],\"target_id\":{task:?}}}. Create a missing Task first with options.behavior set to that owner. An existing Trigger cannot switch task owners; create a separate Trigger for a different owner and preserve the current workflow until its replacement is valid",
                        core.behavior_id()
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
// Behavior request transport
// ---------------------------------------------------------------------------

/// Model-facing tri-state for behavior edits. Serde invokes `Default` only
/// when the property is absent; a present JSON null reaches the deserializer
/// and becomes `Clear`.
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

fn persona_edit_fields(args: &ConfigurePersonaParams) -> Vec<String> {
    [
        ("display_name", &args.display_name),
        ("description", &args.description),
        ("system_prompt", &args.system_prompt),
        ("root", &args.root),
        ("preset", &args.preset),
        ("profile_id", &args.profile_id),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_present())
    .map(|(field, _)| field.to_owned())
    .collect()
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurePersonaParams {
    /// `list` | `inspect` | `preview` | `create` | `edit` | `clone` | `disable`.
    pub action: String,
    /// Proposed mutation for `action: "preview"`.
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub display_name: StringUpdate,
    /// User-facing summary for the working behavior and its context.
    #[serde(default)]
    pub description: StringUpdate,
    /// Complete operating instructions for the working behavior. Required
    /// when creating from a permission preset; optional overrides a clone or
    /// an existing behavior.
    #[serde(default)]
    pub system_prompt: StringUpdate,
    /// Exact behavior_id of the sibling behavior (required for edit/disable).
    #[serde(default)]
    pub behavior_id: Option<String>,
    /// Exact sibling behavior_id to clone; a supplied preset requests a new behavior.
    #[serde(default)]
    pub clone_from: Option<String>,
    #[serde(default)]
    pub root: StringUpdate,
    #[serde(default)]
    pub preset: StringUpdate,
    /// Exact owner-scoped inference profile ID.
    #[serde(default)]
    pub profile_id: StringUpdate,
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

/// How long the `config behavior` command polls a freshly-authored
/// `PersonaConfigRequest` row before returning it still-`pending`: the
/// in-process reconciler sweeps on every `Update` event, so a healthy node
/// converges well inside this window.
const PERSONA_REQUEST_POLL_TIMEOUT: Duration = Duration::from_secs(5);
const PERSONA_REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(200);

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
    edit_fields: Option<Vec<String>>,
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
                description
                system_prompt
                root
                preset
                profile_id
                edit_fields
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
    limit: usize,
    cursor: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        (1..=50).contains(&limit),
        "--limit must be between 1 and 50"
    );
    let store =
        GraphqlPersonaRequestStore::with_ceiling(node.clone(), process_ceiling.root.clone());
    let catalog = store.load_catalog_view(agent_did).await?;
    let default_behavior_id = principal_default_behavior(node, agent_did).await?;
    let total = catalog.behaviors.len();
    let selected = catalog
        .behaviors
        .iter()
        .filter(|(behavior_id, _)| cursor.is_none_or(|cursor| behavior_id.as_str() > cursor))
        .take(limit + 1)
        .collect::<Vec<_>>();
    let truncated = selected.len() > limit;
    let mut behaviors = Vec::new();
    for (behavior_id, reference) in selected.into_iter().take(limit) {
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
            Ok(snapshot) => {
                let effective = &snapshot["effective_config"];
                json!({
                    "behavior_id": behavior_id,
                    "display_name": effective.pointer("/behavior/display_name"),
                    "description": effective.pointer("/behavior/description"),
                    "enabled": reference.enabled,
                    "protected": reference.protected,
                    "is_default": default_behavior_id.as_deref() == Some(behavior_id.as_str()),
                    "context_id": effective.pointer("/behavior/context_id"),
                    "profile_id": effective.pointer("/behavior/inference_profile_id"),
                    "backend_id": effective.pointer("/inference_profile/backend_id"),
                    "model_name": effective.pointer("/inference_profile/model_name"),
                    "reasoning_effort": effective.pointer("/inference_profile/reasoning_effort"),
                })
            }
            Err(error) => json!({
                "behavior_id": behavior_id,
                "enabled": reference.enabled,
                "protected": reference.protected,
                "is_default": default_behavior_id.as_deref() == Some(behavior_id.as_str()),
                "configuration_error": format!("{error:#}"),
            }),
        };
        behaviors.push(snapshot);
    }
    let next_cursor = truncated
        .then(|| {
            behaviors
                .last()?
                .get("behavior_id")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .flatten();
    let snapshot = json!({
        "page": {
            "limit": limit,
            "total": total,
            "returned": behaviors.len(),
            "truncated": truncated,
            "next_cursor": next_cursor,
        },
        "process_ceiling": process_ceiling,
        "allowed_roots": catalog.allowed_roots,
        "permission_presets": crate::agent::persona_presets::builtin_preset_names(),
        "available_profile_ids": catalog.available_profile_ids,
        "default_behavior_id": default_behavior_id,
        "behaviors": behaviors,
        "activation": {
            "durable_config": "after the admitted transaction commits",
            "running_generation": "after the runtime reconciler generation swap",
            "session": "select the behavior in a new session; existing sessions remain bound to their original behavior",
            "pairing_and_restart": "canonical documents and request outcomes replicate to authorized paired clients and are reloaded after restart",
        },
    });
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
            "unknown behavior_id {behavior_id:?}; copy an exact ID from [\"behavior\",\"list\"]"
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
    let Value::Object(mut snapshot) = snapshot else {
        bail!("behavior snapshot must be an object");
    };
    let effective = snapshot.remove("effective_config").unwrap_or_default();
    let mut result = Ordered::reading_order(
        Value::Object(snapshot),
        &["behavior_id", "is_default", "protected"],
    );
    result.0.push(Ordered::entry(
        "effective_config",
        &Ordered::reading_order(effective, EFFECTIVE_ORDER),
    ));
    result.pretty()
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
        persona_name: args.display_name.owned_value(),
        description: args.description.owned_value(),
        system_prompt: args.system_prompt.owned_value(),
        root: args.root.owned_value(),
        preset: args.preset.owned_value(),
        profile_id: args.profile_id.owned_value(),
        edit_fields: (operation == "edit")
            .then(|| persona_edit_fields(args))
            .unwrap_or_default(),
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
            .value()
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
    let preset_requested = args.preset.value().and_then(|preset| {
        crate::agent::persona_presets::preset_fields(preset).map(|fields| {
            json!({
                "file_mode": fields.file_tools_mode,
                "bash_mode": fields.bash_mode,
                "self_configuration": fields.enable_self_config,
            })
        })
    });
    ordered! {
        "admitted": matches!(verdict, PersonaVerdict::Admit),
        "rejection": rejection,
        "committed": false,
        "operation": operation,
        "proposed_ids": json!({
            "behavior_id": behavior_id,
            "context_id": matches!(&op, PersonaOp::Create { .. }).then(|| format!("context-{request_key}")),
            "tools_id": matches!(&op, PersonaOp::Create { .. }).then(|| format!("tools-{request_key}")),
            "profile_id": args.profile_id.value(),
        }),
        "proposed_values": json!({
            "display_name": args.display_name.value(),
            "description": args.description.value(),
            "context_description": args.description.value(),
            "system_prompt": args.system_prompt.value(),
            "root": args.root.value(),
            "preset": args.preset.value(),
            "edit_fields": (operation == "edit").then(|| persona_edit_fields(args)),
            "make_default": args.make_default,
        }),
        "note": "Preview checks request admission without writing; it does not verify materialization or runtime readiness. Preset values are requested authority, narrowed by the process ceiling at runtime. Inspect the applied behavior for effective authority. Applied create IDs use the admitted request key and will differ from these preview-only IDs.",
        "preset_requested": preset_requested,
        "inherited_config": inherited_config,
        "process_ceiling": process_ceiling,
    }
    .pretty()
}

async fn persona_mutate(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    identity: &dyn AgentIdentity,
    args: &ConfigurePersonaParams,
    process_ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    anyhow::ensure!(
        identity.did() == agent_did,
        "behavior writes require the exact local principal signer; signer {:?} cannot configure principal {agent_did:?}",
        identity.did()
    );
    let resolved_behavior_id = args.behavior_id.as_deref().map(str::to_owned);
    let resolved_profile_id = args.profile_id.owned_value();

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
                .value()
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
        persona_name: args.display_name.owned_value(),
        description: args.description.owned_value(),
        system_prompt: args.system_prompt.owned_value(),
        root: args.root.owned_value(),
        preset: args.preset.owned_value(),
        profile_id: resolved_profile_id,
        edit_fields: (args.action == "edit")
            .then(|| persona_edit_fields(args))
            .unwrap_or_default(),
        make_default: args.make_default,
        created_at: now,
        local_signature: Vec::new(),
    };
    record.local_signature = identity.sign(&record.signing_payload()).await?;
    record.validate_shape()?;
    let mutation = local_persona_request_mutation(&record);
    let actor = ::identity::Did::new(identity.did().to_owned())
        .context("self-config principal DID is not ACP-addressable")?;
    crate::config_client::ConfigAccess::transact_local(
        node,
        Some(actor),
        "self_config.create_persona_request",
        |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute_local_response(mutation).await.map(|_| ()) })
        },
    )
    .await?;

    let row = poll_persona_request(node, &request_key, agent_did).await?;
    let status = json!(row.status);
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
            let is_default = principal_default_behavior(node, agent_did)
                .await?
                .as_deref()
                == Some(applied_behavior_id);
            return ordered! {
                "status": status,
                "behavior_id": applied_behavior_id,
                "activation": json!({
                    "durable": "confirmed",
                    "runtime": "new requests cannot select the disabled behavior after reconciliation",
                    "current_turn": "unchanged",
                }),
                "effective": json!({"behavior": behavior, "is_default": is_default}),
                "request": row,
            }
            .pretty();
        }
        let inspected =
            persona_inspect(node, agent_did, applied_behavior_id, process_ceiling).await?;
        let effective: Value = serde_json::from_str(&inspected)?;
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
            .value()
            .is_some_and(|preset| !preset.trim().is_empty())
            || args
                .root
                .value()
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
        let verify_string = |pointer: &str, update: &StringUpdate, field: &str| -> Result<()> {
            if update.is_present() {
                anyhow::ensure!(
                    effective_config.pointer(pointer).and_then(Value::as_str) == update.value(),
                    "applied behavior request reported success but {field} does not match the requested value"
                );
            }
            Ok(())
        };
        verify_string("/behavior/display_name", &args.display_name, "display_name")?;
        verify_string(
            "/behavior/description",
            &args.description,
            "behavior description",
        )?;
        verify_string(
            "/context/description",
            &args.description,
            "context description",
        )?;
        verify_string(
            "/context/system_prompt",
            &args.system_prompt,
            "system_prompt",
        )?;
        if args.root.is_present() {
            let effective_root = effective_config
                .pointer("/documents/Tools/host/root")
                .and_then(Value::as_str);
            match args.root.value() {
                Some(requested_root) => {
                    let canonical_requested = crate::tool_surface::resolve_configured_tool_root(
                        std::path::Path::new(requested_root),
                    )?;
                    anyhow::ensure!(
                        effective_root == Some(canonical_requested.to_string_lossy().as_ref()),
                        "applied behavior request reported success but root does not match the canonical requested value"
                    );
                }
                None => anyhow::ensure!(
                    effective_root.is_none(),
                    "applied behavior request reported success but did not clear root"
                ),
            }
        }
        verify_string(
            "/behavior/inference_profile_id",
            &args.profile_id,
            "profile_id",
        )?;
        if let Some(preset) = args.preset.value() {
            let fields = crate::agent::persona_presets::preset_fields(preset)
                .context("applied behavior request used an unknown preset")?;
            anyhow::ensure!(
                effective_config
                    .pointer("/documents/Tools/host/files/mode")
                    .and_then(Value::as_str)
                    == Some(fields.file_tools_mode.as_str())
                    && effective_config
                        .pointer("/documents/Tools/host/bash/mode")
                        .and_then(Value::as_str)
                        == Some(fields.bash_mode.as_str()),
                "applied behavior request reported success but effective Tools do not match preset {preset:?}"
            );
        }
        if args.make_default {
            anyhow::ensure!(
                effective["is_default"].as_bool() == Some(true),
                "applied behavior request reported success but did not select the behavior as principal default"
            );
        }
        return ordered! {
            "status": status,
            "behavior_id": applied_behavior_id,
            "materialized_ids": json!({
                "behavior_id": applied_behavior_id,
                "context_id": context_id,
                "tools_id": tools_id,
                "profile_id": profile_id,
            }),
            "activation": json!({
                "durable": "confirmed",
                "runtime": "applies to requests dispatched after the reconciler publishes the new generation",
                "current_turn": "unchanged",
                "test": "start a new session explicitly selecting applied_behavior_id",
                "restart_and_pairing": "durable canonical documents and the terminal request outcome survive restart and replicate to authorized paired clients",
            }),
            "effective": serde_json::value::RawValue::from_string(inspected)?,
            "request": row,
        }
        .pretty();
    }
    let recovery = (row.status.as_deref() == Some("rejected")).then(|| {
        json!({
            "guidance": row.status_detail,
            "next": "list or inspect behavior configuration and retry only with published profile/root/preset choices",
        })
    });
    ordered! {"status": status, "recovery": recovery, "request": row}.pretty()
}

/// Install graph packs from the home's pack store or the registry through the
/// canonical resolver, package publication, and activation owners. The running
/// principal is always the install owner; callers cannot select another DID,
/// filesystem distribution, registry endpoint, or control-plane endpoint.
struct PackInstaller {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
    /// The runtime's gents home, which holds the pack store and the plugin
    /// store; `None` resolves from the registry in memory and installs no plugins.
    home: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
struct PackInstallParams {
    pub package: String,
    pub variables: BTreeMap<String, String>,
    pub inference_slots: crate::pack::PackInferenceBindings,
    pub expected_digest: Option<String>,
}

/// What [`crate::plugin::install::snapshot_pack_plugin_records`] observed, to restore on failure.
type PluginRollback = Vec<(
    String,
    String,
    Option<crate::plugin::store::InstalledPlugin>,
)>;

/// A resolved pack and where it came from.
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

    /// The exact `namespace/name@version` this distribution resolved to, so
    /// applying a preview resolves the same pack again (from the store the
    /// preview filled) rather than whatever is newest by then. `requested`
    /// stands when the version is not one a store index can hold.
    fn pinned_spec(&self, requested: &str) -> String {
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

    fn load_graph(
        &self,
        options: &crate::pack::PackInstallOptions,
        environment: &dyn Fn(&str) -> Option<String>,
    ) -> anyhow::Result<crate::graph_package::LoadedGraphPackage> {
        crate::graph_package::load_archive_graph_package_with_environment(
            &self.0.archive,
            options,
            environment,
        )
    }
}

impl PackInstaller {
    fn validate(&self, args: &PackInstallParams) -> anyhow::Result<()> {
        let coordinate = args.package.trim();
        anyhow::ensure!(!coordinate.is_empty(), "pack name must not be blank");
        crate::pack_resolve::parse_pack_spec(coordinate)?;
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

    /// Resolves the pack named by `args`: an installed record whose archive
    /// the home's store still holds, then the store, then the registry. An
    /// `update` asks the registry for the newest version first, so it never
    /// resolves back to the version it is meant to replace.
    async fn resolve(
        &self,
        operation: &str,
        args: &PackInstallParams,
    ) -> anyhow::Result<ConfigPackDistribution> {
        self.validate(args)?;
        let coordinate = args.package.trim();
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
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let installed = crate::pack::installed_packs(
            self.home.as_deref(),
            Some((&access, self.core.agent_did())),
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
            distribution.manifest().metadata.kind == crate::pack::PackKind::Graph,
            "pack {:?} is {:?}; the model-facing installer supports graph packs only",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        );
        Ok(distribution)
    }

    /// The active graph of the installed pack with `manifest`'s namespace and
    /// name. The install record is keyed by `namespace/name`, so a pack of the
    /// same name from another namespace is never reported as installed.
    // vertexia: graphs are looked up by bare package name, so two namespaces
    // installed at once with the same name fail loud as ambiguous.
    async fn installed(
        &self,
        manifest: &crate::pack::PackManifest,
    ) -> anyhow::Result<Option<crate::graph_pipeline::GraphPlan>> {
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        if crate::pack::read_installed_pack(&access, self.core.agent_did(), &coordinate)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        crate::graph_package::load_installed_package_plan(
            &access,
            &manifest.name,
            self.core.agent_did(),
        )
        .await
    }

    /// One page of the packs the home's pack store holds, by `namespace/name`.
    /// Bounded by `limit`; the store's name index is read whole (names only,
    /// no archives), and only the page's manifests are opened.
    // vertexia: the name index is listed whole, a cursor-seeked directory walk
    // if a home ever holds thousands of pack names.
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
            let installable = manifest.metadata.kind == crate::pack::PackKind::Graph;
            let installed = if installable {
                self.installed(manifest).await?
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
                "installed": installed.as_ref().map(|plan| json!({
                    "graph_id": plan.graph_id,
                    "revision_digest": plan.digest,
                })),
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
            "registry_lookup": "Use pack get NAMESPACE/NAME for registry discovery; list shows only the packs this home's store already holds.",
        }))?)
    }

    async fn get(&self, package: &str) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        let distribution = self.resolve("get", &args).await?;
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let inference = crate::pack::inspect_pack_inference_bindings(
            &access,
            distribution.manifest(),
            self.core.agent_did(),
            &BTreeMap::new(),
        )
        .await?;
        let installable = distribution.manifest().metadata.kind == crate::pack::PackKind::Graph;
        let installed = if installable {
            self.installed(distribution.manifest()).await?
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
            "supported_operations": installable.then_some(["preview install", "install", "preview update", "update", "remove"]),
        }))?)
    }

    /// Removes an installed graph package's `PackInstallation` record and
    /// everything it lists: its `GraphDefinition`, every revision it
    /// produced and their derived triggers, and the package's own
    /// documents. Refused while any of its graphs has a run that has not
    /// reached a terminal status. The package is named by coordinate alone,
    /// so removing never resolves it (no store or registry lookup). It
    /// releases no plugin bytes or archive; use `gents pack remove` for that.
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
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let report = crate::pack::remove_pack(
            &access,
            self.core.agent_did(),
            &coordinate,
            crate::pack::DriftPolicy::Refuse,
        )
        .await?;
        ordered! {
            "pack": coordinate,
            "removed": report,
            "effect": "The package's graph and documents were removed. Package SDL schemas and run history stay.",
        }
        .pretty()
    }

    /// Installs the plugins `distribution` ships into the home's plugin store,
    /// sealed: a plugin that asks for authority is refused, because granting
    /// it is the operator's call. Returns the records to restore if the
    /// graph install that follows fails, and the plugins now installed.
    fn install_plugins(
        &self,
        distribution: &ConfigPackDistribution,
        package_digest: &str,
    ) -> anyhow::Result<(PluginRollback, Vec<crate::plugin::store::InstalledPlugin>)> {
        let manifest = distribution.manifest();
        if manifest.metadata.plugins.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let home = self.home.as_deref().with_context(|| {
            format!(
                "pack {:?} ships plugins, which install into the node's plugin home, and this runtime has none; install it with `gents pack install`",
                manifest.name
            )
        })?;
        let rollback = crate::plugin::install::snapshot_pack_plugin_records(home, manifest);
        let installed = crate::plugin::install::install_pack_plugins(
            home,
            manifest,
            package_digest,
            |path| distribution.0.archive.asset(path),
            false,
        )
        .with_context(|| {
            format!(
                "pack {:?} ships a plugin this installer cannot authorize; install it with `gents pack install --grant-authority`",
                manifest.name
            )
        })
        .inspect_err(|_| crate::plugin::install::rollback_pack_plugin_records(home, &rollback))?;
        Ok((rollback, installed))
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
        let installed = self.installed(distribution.manifest()).await?;
        anyhow::ensure!(
            operation != "update" || installed.is_some(),
            "pack {:?} is not installed; preview install instead",
            distribution.manifest().name
        );
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let inspected = crate::pack::inspect_pack_inference_bindings(
            &access,
            distribution.manifest(),
            self.core.agent_did(),
            &args.inference_slots,
        )
        .await?;
        let missing_slots = inspected
            .slots
            .iter()
            .filter(|slot| !slot.optional && !args.inference_slots.contains_key(&slot.name))
            .map(|slot| slot.name.clone())
            .collect::<Vec<_>>();
        if !missing_slots.is_empty() {
            return Ok(serde_json::to_string_pretty(&json!({
                "committed": false,
                "ready": false,
                "operation": operation,
                "package": distribution.manifest().name,
                "version": distribution.manifest().version,
                "source": distribution.source(),
                "artifact_digest": distribution.digest(),
                "registry_artifact_digest": distribution.registry_artifact_digest(),
                "inference": inspected,
                "missing_inference_slots": missing_slots,
                "installed": installed,
                "next": "repeat --inference-slot NAME=PROFILE_ID for every missing slot, using exact eligible profile IDs",
            }))?);
        }
        let inference = crate::pack::preview_pack_inference_bindings(
            &access,
            distribution.manifest(),
            self.core.agent_did(),
            &args.inference_slots,
        )
        .await?;
        let scope = crate::pack::PackInstallOptions {
            agent_did: self.core.agent_did().to_owned(),
        };
        let environment = |name: &str| args.variables.get(name).cloned();
        let package = distribution.load_graph(&scope, &environment)?;
        let bindings = crate::graph_package::GraphPackageInstallBindings {
            agent_did: self.core.agent_did().to_owned(),
            inference_slots: inference.bindings.clone(),
        };
        let prepared = crate::graph_package::prepare_loaded_graph_package_install(
            &access, &package, &bindings,
        )
        .await?;
        let materialized_ids = prepared
            .desired_state
            .documents()
            .iter()
            .map(|document| {
                json!({
                    "collection": document.collection.graphql_type(),
                    "id": document.add[document.collection.unique_field()],
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::to_string_pretty(&json!({
            "committed": false,
            "ready": true,
            "operation": operation,
            "package": package.manifest.name,
            "version": package.manifest.version,
            "source": distribution.source(),
            "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "external_dependencies": package.manifest.external_dependencies,
            "plugins": package.manifest.metadata.plugins,
            "plan": {
                "graph_id": prepared.plan.graph_id,
                "revision_digest": prepared.plan.digest,
                "predecessor_revision_digest": prepared.plan.package.as_ref().and_then(|package| package.predecessor_revision_digest.as_ref()),
                "materialized_ids": materialized_ids,
                "schema_digests": prepared.schema_digests,
            },
            "installed": installed,
            "apply_with": {
                "argv_prefix": ["pack", operation, distribution.pinned_spec(&args.package), "--digest", distribution.digest()],
                "repeat_inference_slots": inference.bindings,
                "repeat_variables": args.variables,
            },
        }))?)
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
        let previous = self.installed(distribution.manifest()).await?;
        anyhow::ensure!(
            operation != "update" || previous.is_some(),
            "pack {:?} is not installed; use pack install",
            distribution.manifest().name
        );
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let missing_slots = distribution
            .manifest()
            .metadata
            .inference_slots
            .iter()
            .filter(|slot| !slot.optional && !args.inference_slots.contains_key(&slot.name))
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            missing_slots.is_empty(),
            "pack {:?} requires explicit --inference-slot bindings for: {}; preview the pack and bind every declared role",
            distribution.manifest().name,
            missing_slots.join(", ")
        );
        let inference = crate::pack::preview_pack_inference_bindings(
            &access,
            distribution.manifest(),
            self.core.agent_did(),
            &args.inference_slots,
        )
        .await?;
        let scope = crate::pack::PackInstallOptions {
            agent_did: self.core.agent_did().to_owned(),
        };
        let environment = |name: &str| args.variables.get(name).cloned();
        let package = distribution.load_graph(&scope, &environment)?;
        anyhow::ensure!(
            package.package_digest == expected,
            "resolved package content does not match the previewed artifact digest"
        );
        let bindings = crate::graph_package::GraphPackageInstallBindings {
            agent_did: self.core.agent_did().to_owned(),
            inference_slots: inference.bindings.clone(),
        };
        let external_dependencies = package.manifest.external_dependencies.clone();
        let plugins = package.manifest.metadata.plugins.clone();
        let (plugin_rollback, installed_plugins) =
            self.install_plugins(&distribution, &package.package_digest)?;
        if let Some(home) = self.home.as_deref() {
            crate::plugin::install::bind_plugin_slots(
                home,
                distribution.manifest(),
                self.core.agent_did(),
                &bindings.inference_slots,
            )
            .inspect_err(|_| {
                crate::plugin::install::rollback_pack_plugin_records(home, &plugin_rollback)
            })?;
        }
        let record = crate::graph_package::GraphInstallRecord {
            plugins: installed_plugins
                .iter()
                .map(|plugin| crate::pack::InstalledPackPlugin {
                    name: plugin.name.clone(),
                    digest: plugin.digest.clone(),
                })
                .collect(),
            explicit: operation == "install",
        };
        // A failure past this point must not leave the plugins installed
        // above orphaned: undo them along with the graph install that never
        // landed.
        let receipt = match crate::graph_package::install_loaded_graph_package(
            &access,
            self.core.agent_did(),
            &package,
            &bindings,
            None,
            &record,
        )
        .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Some(home) = self.home.as_deref() {
                    crate::plugin::install::rollback_pack_plugin_records(home, &plugin_rollback);
                }
                return Err(error);
            }
        };
        let activation = crate::graph_pipeline::activate_graph_revision_with_access(
            &access,
            self.core.agent_did(),
            &receipt.graph_id,
            &receipt.revision_digest,
            previous.as_ref().map(|plan| plan.digest.as_str()),
        )
        .await?;
        let effective = self
            .installed(distribution.manifest())
            .await?
            .context("installed package is not discoverable after activation")?;
        anyhow::ensure!(
            effective.digest == receipt.revision_digest
                && effective
                    .package
                    .as_ref()
                    .is_some_and(|package| package.package_digest == expected),
            "installed package failed effective digest verification"
        );
        Ok(serde_json::to_string_pretty(&json!({
            "operation": operation,
            "source": distribution.source(),
            "artifact_digest": expected,
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "install": receipt,
            "activation": activation,
            "external_dependencies": external_dependencies,
            "plugins": plugins,
            "effective": effective,
            "effect": "The graph package is installed and active. Installation did not start a graph run.",
        }))?)
    }
}

fn graph_access(node: &Arc<EmbeddedNode>) -> crate::config_client::ConfigAccess {
    crate::config_client::ConfigAccess::Local(node.clone())
}

pub struct ListGraphsTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListGraphsParams {}

impl Tool for ListGraphsTool {
    const NAME: &'static str = LIST_GRAPHS_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ListGraphsParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Discover installed graphs on this managed node for the current principal. Returns exact active revision digests, package attribution, entry schemas/input contracts, results, limits, and activation state; it never searches a home directory or another endpoint.".to_owned(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
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
}

pub struct RunGraphTool {
    core: SelfConfigCore,
    node: Arc<EmbeddedNode>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
}

impl Tool for RunGraphTool {
    const NAME: &'static str = RUN_GRAPH_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = RunGraphParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Start an installed graph on this managed node as the current principal. Supply package (and, if the package has more than one entry, entry) plus input matching that entry's advertised input_schema for a package run; list_graphs returns each entry's input_schema. For another graph, supply the exact graph_id, revision_digest, entry, and input returned by list_graphs. Returns a durable run receipt and observed initial state.".to_owned(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "package":{"type":"string"},
                    "graph_id":{"type":"string"},
                    "revision_digest":{"type":"string"},
                    "entry":{"type":"string"},
                    "input":{"type":"object"}
                },
                "additionalProperties":false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let access = graph_access(&self.node);
        let (graph_id, digest, prepared) = if let Some(package) = args.package.as_deref() {
            if args.graph_id.is_some() || args.revision_digest.is_some() {
                return Err(anyhow!(
                    "package runs must not include generic graph_id/revision_digest selectors"
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
                format!(
                    "package {package:?} is not installed; run config pack install {package} first"
                )
            })?;
            let attribution = plan
                .package
                .as_ref()
                .context("active graph revision has no package attribution")?;
            if attribution.name != package {
                return Err(anyhow!(
                    "active graph package attribution changed; call list_graphs again"
                )
                .into());
            }
            // The same selection `prepare_entry_run` makes below, so the
            // ceiling decision can never disagree with which entry actually
            // runs (a mismatch here would let a model-invoked run reach a
            // `git_diff` host step with no ceiling at all).
            let selected_entry =
                crate::graph_package::select_entry(&plan, args.entry.as_deref()).ok();
            let requires_git_diff_ceiling = selected_entry
                .and_then(|entry| entry.prepare.as_ref())
                .is_some_and(|prepare| {
                    prepare.host.iter().any(|step| {
                        matches!(step, crate::graph_pipeline::HostInput::GitDiff { .. })
                    })
                });
            let host_root = if requires_git_diff_ceiling {
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
                        "this graph's prepare step requires effective read authority on the current behavior"
                    )
                    .into());
                }
                let effective_root = effective
                    .pointer("/runtime_effective/effective/root")
                    .and_then(Value::as_str)
                    .context(
                        "this graph's prepare step requires an explicit effective managed root",
                    )?;
                Some(std::path::PathBuf::from(effective_root))
            } else {
                None
            };
            let prepared = crate::graph_package::prepare_entry_run(
                &access,
                self.core.agent_did(),
                crate::graph_package::EntryRunRequest {
                    plan: &plan,
                    entry: args.entry.as_deref(),
                    input: args.input.unwrap_or_else(|| json!({})),
                    host_root: host_root.as_deref(),
                    plugins: &self.plugins,
                },
            )
            .await?;
            (plan.graph_id, plan.digest, prepared)
        } else {
            (
                args.graph_id
                    .context("run_graph requires package or graph_id")?,
                args.revision_digest
                    .context("generic graph run requires revision_digest from list_graphs")?,
                crate::graph_package::PreparedEntryRun {
                    entry_name: args
                        .entry
                        .context("generic graph run requires entry from list_graphs")?,
                    input: args.input.context(
                        "generic graph run requires input matching the advertised entry contract",
                    )?,
                    origin: crate::graph_pipeline::EntryInputOrigin::Operator,
                    documents: 0,
                },
            )
        };
        let receipt = crate::graph_pipeline::start_graph_run_with_access(
            &access,
            self.core.agent_did(),
            &graph_id,
            Some(&digest),
            &prepared.entry_name,
            prepared.input,
            prepared.origin,
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
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
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
    if previews_graphs(config) {
        tools.push(Box::new(graph_preview::PreviewGraphTool {
            core: core.clone(),
        }));
    }
    if config.enable_graph_tools {
        tools.push(Box::new(ListGraphsTool {
            core: core.clone(),
            node: node.clone(),
        }));
        tools.push(Box::new(RunGraphTool {
            core: core.clone(),
            node: node.clone(),
            plugins: plugins.clone(),
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
    tools.push(Box::new(command::ConfigCommandTool {
        node,
        agent_did,
        identity,
        core,
        categories: config.categories.clone(),
        no_lockout: config.no_lockout,
        preview: config.preview,
        allow_pack_install: config.enable_pack_install,
        process_ceiling: config.process_ceiling.clone(),
        execution: Arc::new(execution::ExecutionObservation::default()),
        plugins,
    }));
    tools
}

/// A graph intent reaches storage only through pack installation, so the
/// preview is offered only with that grant; without it the tool's schema is a
/// per-turn cost with no path to publication.
fn previews_graphs(config: &SelfConfigToolConfig) -> bool {
    config.enable_graph_tools && config.enable_pack_install
}

/// Advertised tool names for a resolved self-config surface.
pub fn self_config_tool_names(config: &SelfConfigToolConfig) -> Vec<String> {
    let mut names = Vec::new();
    if previews_graphs(config) {
        names.push(PREVIEW_GRAPH_TOOL_NAME.to_string());
    }
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
    names.push(CONFIG_TOOL_NAME.to_string());
    names
}
