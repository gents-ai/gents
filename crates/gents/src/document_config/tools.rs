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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Tools {
    pub tools_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub host: Option<HostTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub remote: Option<RemoteTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub subagents: Option<SubagentTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub built_ins: Option<BuiltInTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub datastore: Option<DatastoreTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub integrations: Option<IntegrationTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub self_config: Option<SelfConfigTools>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Access to the host running the agent, with nested capability settings.
/// Missing groups and an empty CLI list expose no tools in that category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct HostTools {
    /// Unbound default cwd for files, bash, CLI tools, and LSP. A bound request
    /// uses the validated IsolatedWorkspace overlay through its existing owner.
    /// Absent uses runtime cwd; relative paths resolve against runtime cwd.
    /// Task commands use this configured cwd. Existing tool limits and workspace authority
    /// still apply; cwd itself is not a sandbox. Hook-driven workspace switching
    /// remains a design TODO, not an implicit effect of command output.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub files: Option<FileTools>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub bash: Option<BashTools>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CliTool>>", optional = nullable))]
    pub cli: Vec<CliTool>,
}

/// File access. The mode owns enablement; there is no separate enable flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct FileTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<FileToolMode>", optional = nullable))]
    pub mode: FileToolMode,
    /// Optional execution cap per file operation. Unset retains the enclosing
    /// tool-call/request deadline without adding an independent file timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

/// Bash capability and command execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct BashTools {
    /// Off, ReadOnly, or Unrestricted. Selects the exposed bash tool.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<BashMode>", optional = nullable))]
    pub mode: BashMode,
    /// Execution restrictions, independent of which bash tool is exposed.
    /// Unset retains the existing mode-derived execution policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub execution_mode: Option<CommandExecutionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub network_mode: Option<CommandNetworkMode>,
    /// Argv arrays, not shell strings. Nonempty means every command must match
    /// an allowed prefix; in read-only execution this can also extend the base
    /// executable allowlist. Empty/absent imposes no additional prefix gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub allowed_argv_prefixes: Option<Vec<Vec<String>>>,
    /// Always denied, even if an allowed prefix matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub forbidden_argv_prefixes: Option<Vec<Vec<String>>>,
    /// Nonempty replaces the existing read-only executable list. Empty/absent
    /// preserves that default, matching the existing contract.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub read_only_commands: Option<Vec<String>>,
    /// Permit background execution of this selected bash capability.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub background_enabled: bool,
    /// Foreground default when a call omits timeout_secs; current default 120s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Maximum foreground timeout a call can request. Unset follows timeout_secs.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_timeout_secs: Option<i64>,
    /// Separate background lifetime ceiling; current default 36,000s (10 hours).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub background_timeout_secs: Option<i64>,
    /// Default wait duration for a background process; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested wait duration; current default 600s. Does not kill work.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Select one existing host-registered CLI tool. CLI tools currently do not
/// support background execution; only bash does among native host tools.
/// Binary, environment, and argument constraints remain owned by its existing
/// host registration; this document does not introduce a second executor config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CliTool {
    pub name: String,
    /// Execution timeout. Unset uses the existing host registration's timeout
    /// (10s for the runtime's default CLI registration), within deployment limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

/// How selected MCP tools are presented to the model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct RemoteTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<RemoteServiceTools>>", optional = nullable))]
    pub services: Vec<RemoteServiceTools>,
}

/// Tool selection and presentation for one MCP service.
/// Service connectivity remains owned by the existing MCP service document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tool_names: Vec<String>,
    /// Presentation does not change permissions. Discovery is the default.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<RemoteToolStyle>", optional = nullable))]
    pub style: RemoteToolStyle,
    /// Required services must be available before the behavior admits new work.
    /// Optional service outages do not block admission; calls still require the
    /// selected service and tool to be available. Malformed refs remain errors.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub required: bool,
    /// Exact service-local names permitted to run in the background. Must be a
    /// subset of tool_names; never the generic discovery invocation wrapper name.
    /// Defaults to no background execution, independently of presentation style.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub background_tool_names: Vec<String>,
    /// Connection/handshake timeout for this service; current default 15s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub connect_timeout_secs: Option<i64>,
    /// Tool catalog discovery/preflight timeout; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub discovery_timeout_secs: Option<i64>,
    /// Per-call execution cap in either presentation style; current default 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Additional cap when service health is stale; current default 120s.
    /// Cannot extend the normal per-call cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub stale_timeout_secs: Option<i64>,
    /// Background lifetime ceiling; current process default 36,000s. The service
    /// call timeout still applies; backgrounding does not bypass either limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub background_timeout_secs: Option<i64>,
    /// Default observation wait for background work; current default 30s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested observation wait; current default 600s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Child-agent targets and lifecycle controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SubagentTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    /// References to same-owner SubagentTarget documents. Empty selects no targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub target_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub spawn_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub steering_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub background_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent uses foreground; explicit values are foreground or background.
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub default_await_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub allow_cross_principal: Option<bool>,
    /// Time for a peer to claim a remote spawn, not its execution lifetime.
    /// Current default 60s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub cross_principal_spawn_timeout_secs: Option<i64>,
    /// Default observation wait for a background child; current default 30s.
    /// Child execution lifetime remains owned by its request/inference settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub wait_timeout_secs: Option<i64>,
    /// Maximum requested observation wait; current default 600s. Does not cancel child.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Agent runtime capabilities independent of host access or external integrations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct BuiltInTools {
    /// Independent goal get/update capability. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_goal_tools: Option<bool>,
    /// Additional opt-in for model-facing goal creation. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_goal_creation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_memory: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_session_history_tool: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_context_budget: Option<bool>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

/// Schema-bounded access to the DefraDB datastore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DatastoreTools {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_defra_query: Option<bool>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub defra_query_collections: Option<Vec<String>>,
    /// Bare `surface_id` refs to same-agent `DatastoreToolSurface` docs.
    /// Expanded into create and query tools at snapshot build (fail-closed).
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub datastore_tool_surface_ids: Option<Vec<String>>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

/// Domain-specific external integrations. Extension design remains to be settled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct IntegrationTools {
    /// Presence selects the LSP integration; absent means disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub lsp: Option<LspTools>,
    /// Bare `tool_id` refs to same-agent `EthTool` docs. Expanded at snapshot
    /// build (fail-closed). Empty/absent = no eth tools.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub eth_tool_ids: Option<Vec<String>>,
}

/// Language-server settings embedded in IntegrationTools.
/// Retains the existing operator payload and its validator for this draft.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct LspTools {
    /// Existing language-server flags and catalog overrides. Absent uses defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub config: Option<String>,
    /// Default action timeout, including indexing retries; current default 20s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Maximum model-requested action timeout; current maximum 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_timeout_secs: Option<i64>,
    /// Lower-level LSP request timeout; current default 30s. Action deadlines
    /// can shorten it. Idle and per-server warmup limits remain in config.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub rpc_timeout_secs: Option<i64>,
}

/// Existing self-configuration controls, grouped for a separate design review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SelfConfigTools {
    /// Self-configuration gate (#654): opt-in, never backfilled true.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_self_config: Option<bool>,
    /// Self-config category allowlist; unset means the core spine
    /// (behavior, tools, profile). See `config_client::patch`.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub self_config_categories: Option<Vec<String>>,
    /// Opt-in guardrail: refuse self-config patches that would strip the
    /// agent's own reconfigure ability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub self_config_no_lockout: Option<bool>,
    /// Opt-in guardrail: `get_my_config` accepts a patch preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub self_config_dry_run: Option<bool>,
    /// Optional execution cap per tool call. Unset retains the existing enclosing
    /// tool-call/request deadline without introducing an independent timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

impl Tools {
    /// Validate this document's local settings. Referenced documents and runtime
    /// availability are checked by their scoped resolution owners.
    pub fn validation_violations(&self) -> Vec<String> {
        fn positive(errors: &mut Vec<String>, field: &str, value: Option<i64>) {
            if value.is_some_and(|value| value <= 0) {
                errors.push(format!("{field} must be positive"));
            }
        }
        fn bounded(
            errors: &mut Vec<String>,
            field: &str,
            value: Option<i64>,
            maximum: Option<i64>,
            fallback: i64,
            maximum_fallback: Option<i64>,
        ) {
            positive(errors, field, value);
            positive(errors, &format!("{field} maximum"), maximum);
            let value = value.unwrap_or(fallback);
            let maximum = maximum.unwrap_or_else(|| maximum_fallback.unwrap_or(value));
            if value > maximum {
                errors.push(format!(
                    "{field} maximum {maximum} must cover effective default {value}"
                ));
            }
        }
        fn names<'a>(
            errors: &mut Vec<String>,
            field: &str,
            values: impl IntoIterator<Item = &'a str>,
            exact_tools: bool,
        ) {
            let mut seen = std::collections::HashSet::new();
            for value in values {
                if value.trim().is_empty() {
                    errors.push(format!("{field} contains a blank name"));
                }
                if exact_tools && (value.contains('*') || value.contains('?')) {
                    errors.push(format!(
                        "{field} requires explicit tool names, not wildcard {value:?}"
                    ));
                }
                if !seen.insert(value) {
                    errors.push(format!("{field} contains duplicate name {value:?}"));
                }
            }
        }
        let mut errors = Vec::new();
        if let Some(host) = &self.host {
            if let Some(files) = &host.files {
                positive(&mut errors, "host.files.timeout_secs", files.timeout_secs);
            }
            if let Some(bash) = &host.bash {
                bounded(
                    &mut errors,
                    "host.bash.timeout_secs",
                    bash.timeout_secs,
                    bash.max_timeout_secs,
                    crate::toolset::DEFAULT_COMMAND_TIMEOUT_SECS as i64,
                    None,
                );
                bounded(
                    &mut errors,
                    "host.bash.wait_timeout_secs",
                    bash.wait_timeout_secs,
                    bash.max_wait_timeout_secs,
                    30,
                    Some(600),
                );
                positive(
                    &mut errors,
                    "host.bash.background_timeout_secs",
                    bash.background_timeout_secs,
                );
                for (field, prefixes) in [
                    (
                        "host.bash.allowed_argv_prefixes",
                        &bash.allowed_argv_prefixes,
                    ),
                    (
                        "host.bash.forbidden_argv_prefixes",
                        &bash.forbidden_argv_prefixes,
                    ),
                ] {
                    for prefix in prefixes.iter().flatten() {
                        if prefix
                            .first()
                            .is_none_or(|executable| executable.trim().is_empty())
                        {
                            errors.push(format!(
                                "{field} requires a nonempty argv prefix with an executable"
                            ));
                        }
                    }
                }
                names(
                    &mut errors,
                    "host.bash.read_only_commands",
                    bash.read_only_commands.iter().flatten().map(String::as_str),
                    false,
                );
            }
            names(
                &mut errors,
                "host.cli",
                host.cli.iter().map(|tool| tool.name.as_str()),
                true,
            );
            for cli in &host.cli {
                positive(
                    &mut errors,
                    &format!("host.cli[{}].timeout_secs", cli.name),
                    cli.timeout_secs,
                );
            }
        }
        if let Some(remote) = &self.remote {
            names(
                &mut errors,
                "remote.services",
                remote
                    .services
                    .iter()
                    .map(|service| service.mcp_service_id.as_str()),
                false,
            );
            for service in &remote.services {
                let field = format!("remote.services[{}]", service.mcp_service_id);
                names(
                    &mut errors,
                    &format!("{field}.tool_names"),
                    service.tool_names.iter().map(String::as_str),
                    true,
                );
                names(
                    &mut errors,
                    &format!("{field}.background_tool_names"),
                    service.background_tool_names.iter().map(String::as_str),
                    true,
                );
                for background in &service.background_tool_names {
                    if !service.tool_names.contains(background) {
                        errors.push(format!(
                            "{field}.background_tool_names contains unselected tool {background:?}"
                        ));
                    }
                }
                for (name, value) in [
                    ("connect_timeout_secs", service.connect_timeout_secs),
                    ("discovery_timeout_secs", service.discovery_timeout_secs),
                    ("timeout_secs", service.timeout_secs),
                    ("stale_timeout_secs", service.stale_timeout_secs),
                    ("background_timeout_secs", service.background_timeout_secs),
                ] {
                    positive(&mut errors, &format!("{field}.{name}"), value);
                }
                // Stale/background caps are intersections at invocation, not
                // requirements that authored ceilings be below the normal call cap.
                bounded(
                    &mut errors,
                    &format!("{field}.wait_timeout_secs"),
                    service.wait_timeout_secs,
                    service.max_wait_timeout_secs,
                    30,
                    Some(600),
                );
            }
        }
        if let Some(subagents) = &self.subagents {
            names(
                &mut errors,
                "subagents.target_ids",
                subagents.target_ids.iter().map(String::as_str),
                false,
            );
            positive(
                &mut errors,
                "subagents.cross_principal_spawn_timeout_secs",
                subagents.cross_principal_spawn_timeout_secs,
            );
            bounded(
                &mut errors,
                "subagents.wait_timeout_secs",
                subagents.wait_timeout_secs,
                subagents.max_wait_timeout_secs,
                30,
                Some(600),
            );
            match subagents.default_await_mode.as_deref() {
                None | Some("foreground") => {}
                Some("background") if subagents.background_enabled.unwrap_or(false) => {}
                Some("background") => errors.push(
                    "subagents.default_await_mode background requires background_enabled"
                        .to_owned(),
                ),
                Some(_) => errors.push(
                    "subagents.default_await_mode must be foreground or background".to_owned(),
                ),
            }
        }
        if let Some(built_ins) = &self.built_ins {
            positive(
                &mut errors,
                "built_ins.timeout_secs",
                built_ins.timeout_secs,
            );
        }
        if let Some(datastore) = &self.datastore {
            positive(
                &mut errors,
                "datastore.timeout_secs",
                datastore.timeout_secs,
            );
            names(
                &mut errors,
                "datastore.defra_query_collections",
                datastore
                    .defra_query_collections
                    .iter()
                    .flatten()
                    .map(String::as_str),
                false,
            );
            names(
                &mut errors,
                "datastore.datastore_tool_surface_ids",
                datastore
                    .datastore_tool_surface_ids
                    .iter()
                    .flatten()
                    .map(String::as_str),
                false,
            );
        }
        if let Some(integrations) = &self.integrations {
            names(
                &mut errors,
                "integrations.eth_tool_ids",
                integrations
                    .eth_tool_ids
                    .iter()
                    .flatten()
                    .map(String::as_str),
                false,
            );
            if let Some(lsp) = &integrations.lsp {
                bounded(
                    &mut errors,
                    "integrations.lsp.timeout_secs",
                    lsp.timeout_secs,
                    lsp.max_timeout_secs,
                    20,
                    Some(300),
                );
                positive(
                    &mut errors,
                    "integrations.lsp.rpc_timeout_secs",
                    lsp.rpc_timeout_secs,
                );
                if let Err(error) =
                    crate::toolset::lsp::LspConfigDocument::parse_operator(lsp.config.as_deref())
                {
                    errors.push(format!("invalid integrations.lsp.config: {error}"));
                }
            }
        }
        if let Some(self_config) = &self.self_config {
            positive(
                &mut errors,
                "self_config.timeout_secs",
                self_config.timeout_secs,
            );
            names(
                &mut errors,
                "self_config.self_config_categories",
                self_config
                    .self_config_categories
                    .iter()
                    .flatten()
                    .map(String::as_str),
                false,
            );
            for category in self_config.self_config_categories.iter().flatten() {
                if !crate::config_client::patch::SELF_CONFIG_CATEGORIES.contains(&category.as_str())
                {
                    errors.push(format!("unknown self_config category {category:?}"));
                }
            }
        }
        errors
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let violations = self.validation_violations();
        anyhow::ensure!(
            violations.is_empty(),
            "Tools {}: {}",
            self.tools_id,
            violations.join("; ")
        );
        Ok(())
    }
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
    fn document(groups: serde_json::Value) -> Tools {
        let mut value = groups;
        value["tools_id"] = "tools".into();
        value["agent_did"] = "owner".into();
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn omitted_groups_and_presentation_do_not_grant_or_change_permissions() {
        assert!(document(json!({})).validate().is_ok());
        for style in ["flat", "discovery"] {
            let tools = document(
                json!({"remote":{"services":[{"mcp_service_id":"remote", "style":style}]}, "built_ins":{"enable_goal_creation":true}}),
            );
            let before = serde_json::to_value(&tools).unwrap();
            assert!(tools.validate().is_ok());
            assert_eq!(serde_json::to_value(&tools).unwrap(), before);
            assert!(
                tools.remote.as_ref().unwrap().services[0]
                    .tool_names
                    .is_empty()
            );
            assert_eq!(tools.built_ins.as_ref().unwrap().enable_goal_tools, None);
        }
    }

    #[test]
    fn background_selection_is_a_subset_in_both_presentations() {
        for style in ["flat", "discovery"] {
            let mut tools = document(
                json!({"remote":{"services":[{"mcp_service_id":"remote", "style":style,"tool_names":["search"], "background_tool_names":["other"]}]}}),
            );
            assert!(tools.validate().is_err());
            tools.remote.as_mut().unwrap().services[0].background_tool_names =
                vec!["search".into()];
            assert!(tools.validate().is_ok());
        }
    }

    #[test]
    fn every_configured_tool_timeout_rejects_zero_and_negative_values() {
        let cases = [
            ("host.files", vec!["timeout_secs"]),
            (
                "host.bash",
                vec![
                    "timeout_secs",
                    "max_timeout_secs",
                    "background_timeout_secs",
                    "wait_timeout_secs",
                    "max_wait_timeout_secs",
                ],
            ),
            ("built_ins", vec!["timeout_secs"]),
            ("datastore", vec!["timeout_secs"]),
            (
                "subagents",
                vec![
                    "cross_principal_spawn_timeout_secs",
                    "wait_timeout_secs",
                    "max_wait_timeout_secs",
                ],
            ),
            (
                "integrations.lsp",
                vec!["timeout_secs", "max_timeout_secs", "rpc_timeout_secs"],
            ),
            ("self_config", vec!["timeout_secs"]),
        ];
        for (path, fields) in cases {
            for field in fields {
                for invalid in [0, -1] {
                    let mut value = json!({});
                    let mut group = &mut value;
                    for part in path.split('.') {
                        group[part] = json!({});
                        group = &mut group[part];
                    }
                    group[field] = invalid.into();
                    assert!(
                        document(value).validate().is_err(),
                        "{path}.{field}={invalid}"
                    );
                }
            }
        }
        for field in [
            "connect_timeout_secs",
            "discovery_timeout_secs",
            "timeout_secs",
            "stale_timeout_secs",
            "background_timeout_secs",
            "wait_timeout_secs",
            "max_wait_timeout_secs",
        ] {
            for invalid in [0, -1] {
                let mut service = json!({"mcp_service_id":"remote"});
                service[field] = invalid.into();
                assert!(
                    document(json!({"remote":{"services":[service]}}))
                        .validate()
                        .is_err(),
                    "remote.{field}"
                );
            }
        }
        assert!(
            document(json!({"host":{"cli":[{"name":"git","timeout_secs":0}]}}))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn configured_maxima_cover_effective_defaults_without_inventing_fixed_minima() {
        for value in [
            json!({"host":{"bash":{"max_timeout_secs":5}}}),
            json!({"host":{"bash":{"wait_timeout_secs":601}}}),
            json!({"subagents":{"max_wait_timeout_secs":5}}),
            json!({"remote":{"services":[{"mcp_service_id":"remote","max_wait_timeout_secs":5}]}}),
            json!({"integrations":{"lsp":{"max_timeout_secs":5}}}),
        ] {
            assert!(document(value).validate().is_err());
        }
        assert!(document(json!({"host":{"bash":{"timeout_secs":5,"max_timeout_secs":5,"wait_timeout_secs":700,"max_wait_timeout_secs":700}}})).validate().is_ok());
        assert!(
            document(json!({"host":{"bash":{"timeout_secs":500}}}))
                .validate()
                .is_ok()
        );
        assert!(document(json!({"remote":{"services":[{"mcp_service_id":"remote","timeout_secs":1,"stale_timeout_secs":120,"background_timeout_secs":36000}]}})).validate().is_ok());
    }

    #[test]
    fn ambiguous_names_and_invalid_background_defaults_reject() {
        for value in [
            json!({"host":{"cli":[{"name":"git"},{"name":"git"}]}}),
            json!({"remote":{"services":[{"mcp_service_id":"remote"},{"mcp_service_id":"remote"}]}}),
            json!({"remote":{"services":[{"mcp_service_id":"remote","tool_names":["search","search"]}]}}),
            json!({"remote":{"services":[{"mcp_service_id":"remote","tool_names":["*"]}]}}),
            json!({"subagents":{"target_ids":["target","target"]}}),
            json!({"subagents":{"target_ids":[" "]}}),
            json!({"subagents":{"default_await_mode":"background"}}),
            json!({"subagents":{"default_await_mode":"unknown"}}),
            json!({"self_config":{"self_config_categories":["unknown"]}}),
            json!({"self_config":{"self_config_categories":["tools","tools"]}}),
            json!({"host":{"bash":{"allowed_argv_prefixes":[[]]}}}),
        ] {
            assert!(document(value).validate().is_err());
        }
        assert!(document(json!({"subagents":{"target_ids":["existing-target"],"default_await_mode":"background","background_enabled":true}})).validate().is_ok());
        assert!(
            document(json!({"host":{"bash":{"allowed_argv_prefixes":[["printf", ""]]}}}))
                .validate()
                .is_ok()
        );
    }
}
