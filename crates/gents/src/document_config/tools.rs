//! One tool configuration document with typed nested groups (structural draft).
//!
//! Optional timeout fields use the documented owner default when unset. Configured
//! Tool enable/allow/background flags default false; presence or references alone
//! do not enable those flags. Configured durations must be positive, and configurable maxima must not be below defaults.
//! Foreground execution remains bounded by the enclosing deadline and deployment
//! ceilings. Background executions retain their own lifetime and cancellation owner.
//! Wait timeouts return a running snapshot; they do not cancel the underlying work.

use serde::{Deserialize, Serialize};

use crate::tool_surface::{BashMode, FileToolMode};
use crate::toolset::{CommandExecutionMode, CommandNetworkMode};

/// Tool capabilities, execution policies, and dependencies referenced by a context.
/// Groups are embedded in this document and share its ownership. Missing groups
/// expose no capabilities. External service, surface, and target refs remain explicit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub tools_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<HostTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<SubagentTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built_ins: Option<BuiltInTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datastore: Option<DatastoreTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrations: Option<IntegrationTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_config: Option<SelfConfigTools>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Access to the host running the agent, with nested capability settings.
/// Missing groups and an empty CLI list expose no tools in that category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct HostTools {
    /// Unbound default cwd for files, bash, CLI tools, and LSP. A bound request
    /// uses the validated IsolatedWorkspace overlay through its existing owner.
    /// Absent uses runtime cwd; relative paths resolve against runtime cwd.
    /// Task commands use this configured cwd. Existing tool limits and workspace authority
    /// still apply; cwd itself is not a sandbox. Hook-driven workspace switching
    /// remains a design TODO, not an implicit effect of command output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<FileTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bash: Option<BashTools>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cli: Vec<CliTool>,
}

/// File access. The mode owns enablement; there is no separate enable flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct FileTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    pub mode: FileToolMode,
    /// Optional execution cap per file operation. Unset retains the enclosing
    /// tool-call/request deadline without adding an independent file timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// Bash capability and command execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct BashTools {
    /// Off, ReadOnly, or Unrestricted. Selects the exposed bash tool.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    pub mode: BashMode,
    /// Execution restrictions, independent of which bash tool is exposed.
    /// Unset retains the existing mode-derived execution policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<CommandExecutionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<CommandNetworkMode>,
    /// Argv arrays, not shell strings. Nonempty means every command must match
    /// an allowed prefix; in read-only execution this can also extend the base
    /// executable allowlist. Empty/absent imposes no additional prefix gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_argv_prefixes: Option<Vec<Vec<String>>>,
    /// Always denied, even if an allowed prefix matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forbidden_argv_prefixes: Option<Vec<Vec<String>>>,
    /// Nonempty replaces the existing read-only executable list. Empty/absent
    /// preserves that default, matching the existing contract.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_commands: Option<Vec<String>>,
    /// Permit background execution of this selected bash capability.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    pub background_enabled: bool,
    /// Foreground default when a call omits timeout_secs; current default 120s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    /// Maximum foreground timeout a call can request. Unset follows timeout_secs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_timeout_secs: Option<i64>,
    /// Separate background lifetime ceiling; current default 36,000s (10 hours).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_timeout_secs: Option<i64>,
    /// Default wait duration for a background process; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested wait duration; current default 600s. Does not kill work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Select one existing host-registered CLI tool. CLI tools currently do not
/// support background execution; only bash does among native host tools.
/// Binary, environment, and argument constraints remain owned by its existing
/// host registration; this document does not introduce a second executor config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct CliTool {
    pub name: String,
    /// Execution timeout. Unset uses the existing host registration's timeout
    /// (10s for the runtime's default CLI registration), within deployment limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// How selected MCP tools are presented to the model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteToolStyle {
    /// Include each selected tool and its schema in the provider request.
    Flat,
    /// Expose discovery, description, and invocation tools instead of the full catalog.
    #[default]
    Discovery,
}

/// Select MCP services with per-service settings. Empty means no remote tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<RemoteServiceTools>,
}

/// Tool selection and presentation for one MCP service.
/// Service connectivity remains owned by the existing MCP service document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteServiceTools {
    pub mcp_service_id: String,
    /// Exact MCP tool names, never wildcards. Empty permits no tools, and newly
    /// discovered tools are not automatically selected. Selection is enforced at
    /// invocation in both presentation styles, not just when building the catalog.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_names: Vec<String>,
    /// Presentation does not change permissions. Discovery is the default.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    pub style: RemoteToolStyle,
    /// Required services must be available before the behavior admits new work.
    /// Optional service outages do not block admission; calls still require the
    /// selected service and tool to be available. Malformed refs remain errors.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    pub required: bool,
    /// Exact service-local names permitted to run in the background. Must be a
    /// subset of tool_names; never the generic discovery invocation wrapper name.
    /// Defaults to no background execution, independently of presentation style.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub background_tool_names: Vec<String>,
    /// Connection/handshake timeout for this service; current default 15s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<i64>,
    /// Tool catalog discovery/preflight timeout; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_timeout_secs: Option<i64>,
    /// Per-call execution cap in either presentation style; current default 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    /// Additional cap when service health is stale; current default 120s.
    /// Cannot extend the normal per-call cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_timeout_secs: Option<i64>,
    /// Background lifetime ceiling; current process default 36,000s. The service
    /// call timeout still applies; backgrounding does not bypass either limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_timeout_secs: Option<i64>,
    /// Default observation wait for background work; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested observation wait; current default 600s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Child-agent targets and lifecycle controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SubagentTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    /// References to same-owner SubagentTarget documents. Empty selects no targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub target_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steering_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent uses foreground; explicit values are foreground or background.
    pub default_await_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_cross_principal: Option<bool>,
    /// Time for a peer to claim a remote spawn, not its execution lifetime.
    /// Current default 60s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_principal_spawn_timeout_secs: Option<i64>,
    /// Default observation wait for a background child; current default 30s.
    /// Child execution lifetime remains owned by its request/inference settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested observation wait; current default 600s. Does not cancel child.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Agent runtime capabilities independent of host access or external integrations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct BuiltInTools {
    /// Independent goal get/update capability. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_goal_tools: Option<bool>,
    /// Additional opt-in for model-facing goal creation. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_goal_creation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_memory: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_session_history_tool: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_context_budget: Option<bool>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// Schema-bounded access to the DefraDB datastore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct DatastoreTools {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_defra_query: Option<bool>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defra_query_collections: Option<Vec<String>>,
    /// Bare `surface_id` refs to same-agent `DatastoreToolSurface` docs.
    /// Expanded into create and query tools at snapshot build (fail-closed).
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datastore_tool_surface_ids: Option<Vec<String>>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// Domain-specific external integrations. Extension design remains to be settled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTools {
    /// Presence selects the LSP integration; absent means disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp: Option<LspTools>,
    /// Bare `tool_id` refs to same-agent `EthTool` docs. Expanded at snapshot
    /// build (fail-closed). Empty/absent = no eth tools.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eth_tool_ids: Option<Vec<String>>,
}

/// Language-server settings embedded in IntegrationTools.
/// Retains the existing operator payload and its validator for this draft.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LspTools {
    /// Existing language-server flags and catalog overrides. Absent uses defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Default action timeout, including indexing retries; current default 20s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    /// Maximum model-requested action timeout; current maximum 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_timeout_secs: Option<i64>,
    /// Lower-level LSP request timeout; current default 30s. Action deadlines
    /// can shorten it. Idle and per-server warmup limits remain in config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_timeout_secs: Option<i64>,
}

/// Existing self-configuration controls, grouped for a separate design review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SelfConfigTools {
    /// Self-configuration gate (#654): opt-in, never backfilled true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_self_config: Option<bool>,
    /// Self-config category allowlist; unset means the core spine
    /// (behavior, tools, profile). See `config_client::patch`.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_config_categories: Option<Vec<String>>,
    /// Opt-in guardrail: refuse self-config patches that would strip the
    /// agent's own reconfigure ability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_config_no_lockout: Option<bool>,
    /// Opt-in guardrail: `get_my_config` accepts a patch preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_config_dry_run: Option<bool>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// Compact full-document serialization. Explicit sparse patches use a separate
/// representation so an omitted field is not confused with clearing a field.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minimal_tools_round_trip_without_disabled_group_boilerplate() {
        let authored = json!({
            "tools_id": "coding", "agent_did": "did:key:example",
            "host": {"root": ".", "files": {"mode": "ReadWrite"}}
        });
        let tools: Tools = serde_json::from_value(authored.clone()).unwrap();
        assert!(tools.remote.is_none());
        assert!(tools.host.as_ref().unwrap().bash.is_none());
        assert_eq!(serde_json::to_value(&tools).unwrap(), authored);
        assert_eq!(
            serde_json::from_value::<Tools>(serde_json::to_value(&tools).unwrap()).unwrap(),
            tools
        );
    }

    #[test]
    fn missing_and_null_remote_defaults_grant_no_tools() {
        let tools: Tools = serde_json::from_value(json!({
            "tools_id": "research", "agent_did": "did:key:example",
            "remote": {"services": [{"mcp_service_id": "web", "tool_names": null, "style": null}]}
        }))
        .unwrap();
        let service = &tools.remote.as_ref().unwrap().services[0];
        assert!(service.tool_names.is_empty());
        assert!(service.background_tool_names.is_empty());
        assert!(!service.required);
        assert_eq!(service.style, RemoteToolStyle::Discovery);
        assert_eq!(
            serde_json::to_value(tools).unwrap(),
            json!({
                "tools_id": "research", "agent_did": "did:key:example",
                "remote": {"services": [{"mcp_service_id": "web"}]}
            })
        );
    }

    #[test]
    fn explicit_overrides_survive_and_required_references_do_not_default() {
        let authored = json!({
            "tools_id": "research", "agent_did": "did:key:example",
            "remote": {"services": [{"mcp_service_id": "web", "tool_names": ["search"],
                "style": "flat", "required": true, "timeout_secs": 45}]},
            "built_ins": {"enable_goal_tools": false}
        });
        let tools: Tools = serde_json::from_value(authored.clone()).unwrap();
        assert_eq!(serde_json::to_value(tools).unwrap(), authored);
        assert!(serde_json::from_value::<Tools>(json!({"agent_did": "did:key:example"})).is_err());
        assert!(
            serde_json::from_value::<RemoteServiceTools>(json!({"tool_names": ["search"]}))
                .is_err()
        );
    }
}
