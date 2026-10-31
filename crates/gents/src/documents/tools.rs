//! Canonical tool configuration with typed nested capability groups.
//!
//! Optional timeouts use the documented owner default when unset. Tool
//! enable/allow/background flags default false; presence or references alone do
//! not enable those flags. Foreground execution remains bounded by the
//! enclosing request deadline. Background executions retain their own lifetime
//! and cancellation owner. Wait timeouts return a running snapshot; they do not
//! cancel the underlying work.

use serde::{Deserialize, Serialize};

use crate::tools::native::{CommandExecutionMode, CommandNetworkMode};
use crate::tools::surface::{BashMode, FileToolMode};
pub use gents_protocol::row::RetiredKey;

/// Tool capabilities, execution policies, and dependencies referenced by a context.
/// Groups are embedded in this document and share its ownership. Missing groups
/// expose no capabilities. External service, surface, and target refs remain explicit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Tools {
    pub tools_id: String,
    pub node_did: String,
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
    pub agents: Option<AgentTools>,
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
/// File operations are bounded by the enclosing request deadline. There is no
/// per-operation timer: a write cannot be abandoned mid-flight without
/// misreporting whether it landed.
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
    /// UTF-8 bytes of content `read_file` returns (cut on a character
    /// boundary) when a call omits `max_chars`, and the most a call can
    /// request. Unset uses 32,000. Must be between 1 and 1,000,000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_read_chars: Option<i64>,
    /// Entries `list_files` returns when a call omits `max_entries`, and the
    /// most a call can request. Unset uses 200. Must be between 1 and 5,000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_list_entries: Option<i64>,
    /// Matches `glob` and `grep` return when a call omits `max_matches`, and
    /// the most a call can request. Unset uses 200. Must be between 1 and
    /// 5,000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_matches: Option<i64>,
}

/// Bash capability and command execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct BashTools {
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub background_enabled: bool,
    /// Foreground timeout when a call omits `timeout_secs`. Unset uses the
    /// host's `--command-timeout-secs` (120s unless the host sets it). Clamped
    /// to the effective maximum below.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Longest foreground timeout a call can request. Unset follows
    /// `timeout_secs` when that is set, otherwise the host's maximum. Clamped
    /// to the host's `--command-timeout-max-secs` (which defaults to its
    /// `--command-timeout-secs`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_timeout_secs: Option<i64>,
    /// Lifetime of a bash run started with `spawn_process`; the run is timed
    /// out when it expires. Unset uses 36,000s (10 hours); larger values are
    /// clamped to 36,000s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub background_timeout_secs: Option<i64>,
    /// `wait_process` wait on a bash run when the call omits `timeout_secs`.
    /// Unset uses 30s. Clamped to the wait maximum. Waiting never stops the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub wait_timeout_secs: Option<i64>,
    /// Longest `wait_process` wait a call can request on a bash run. Unset
    /// uses 600s; larger values are clamped to 600s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_wait_timeout_secs: Option<i64>,
    /// Output budget for stdout and for stderr, each, in what a command shows
    /// the model; the rest is truncated. Counted in UTF-8 bytes, cut on a
    /// character boundary. Unset uses 16,000. Must be between 1 and 1,000,000.
    /// It bounds a completed command's result and an interrupted command's
    /// output tail, and a background completion notification summarizes at
    /// most this much (never more than 4,000 bytes), using the value configured
    /// when the output is presented.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_output_chars: Option<i64>,
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
    /// Execution timeout. Unset keeps the host registration's timeout (10s for
    /// every built-in registration). A set value replaces it, clamped to the
    /// host's foreground maximum (`--command-timeout-max-secs`, which defaults
    /// to `--command-timeout-secs`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Output budget for stdout and for stderr, each, in what a call shows the
    /// model; the rest is truncated. Counted in UTF-8 bytes, cut on a
    /// character boundary. Unset keeps the host CLI registration's budget
    /// (16,000 for every built-in registration). Must be between 1 and
    /// 1,000,000. It bounds a completed call's result and an interrupted
    /// call's output tail, using the value configured when the result is
    /// presented, not at dispatch.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_output_chars: Option<i64>,
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

impl RemoteToolStyle {
    pub const ALL: [Self; 2] = [Self::Flat, Self::Discovery];
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
    /// Required services must be available before agent admission of new work.
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
    /// Lifetime of a call to this service started with `spawn_process`. Unset
    /// uses 36,000s; larger values are clamped to 36,000s. The per-call
    /// `timeout_secs` still applies inside it; backgrounding bypasses neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub background_timeout_secs: Option<i64>,
    /// `wait_process` wait on a background call to this service when the call
    /// omits `timeout_secs`. Unset uses 30s. Clamped to the wait maximum.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub wait_timeout_secs: Option<i64>,
    /// Longest `wait_process` wait a call can request on a background call to
    /// this service. Unset uses 600s; larger values are clamped to 600s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_wait_timeout_secs: Option<i64>,
}

/// Session-message targets: the allowlist agent_new/agent_message address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentTools {
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    /// References to same-owner AgentTarget documents. Empty selects no targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub target_ids: Vec<String>,
    /// Exposes agent_new/agent_message over the allowlisted targets. Every
    /// started session is a background tool row; there is no foreground wait,
    /// workspace inheritance, cascade or cross-node switch. A target on
    /// another node is admitted there as a Peer request under its ACP.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enabled: Option<bool>,
}

/// Agent runtime capabilities independent of host access or external integrations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct BuiltInTools {
    /// Tools documents stored before graph tools were removed carry this key;
    /// it is accepted and ignored, and the canonical serializer drops it on next write.
    #[serde(default, rename = "enable_graph_tools", skip_serializing)]
    #[cfg_attr(feature = "typescript", ts(skip))]
    pub retired_graph_tools: RetiredKey,
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
    /// The `sessions` read tool. Unset is enabled: compaction stubs name its
    /// `output` read as the way to recover a trimmed tool result.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_session_history_tool: Option<bool>,
    /// Node-wide schema administration, independent of config and document access.
    /// DefraDB remains the schema authorization owner. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    /// Tools writers may configure this capability like other built-ins; an
    /// operator that must forbid it must clamp the tool-policy ceiling.
    pub enable_schema_tool: Option<bool>,
    /// Native node P2P observations. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_p2p_tool: Option<bool>,
    /// Native P2P changes also require enable_p2p_tool. Enrollment and DefraDB
    /// remain the authorization owners. Unset is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_p2p_mutations: Option<bool>,
    /// Exact collection names allowed for application pairing overlays and sync.
    /// Missing or empty grants no collections; the policy ceiling can only narrow.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub p2p_collections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_context_budget: Option<bool>,
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
    /// Exact application collections granted to the generic write endpoint. Empty denies all.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub write_collections: Option<Vec<String>>,
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
    /// Installed plugins offered to the model as tools, each named after its
    /// plugin. Empty/absent = no plugin tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub plugins: Option<Vec<PluginToolRef>>,
}

/// One installed plugin offered to the model as a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginToolRef {
    /// `namespace/name` of the installed plugin; the tool is called `name`.
    pub plugin: String,
    /// `sha256:<hex>` of the artifact to run. When the installed plugin is a
    /// different artifact, the tool is unavailable until the pin is updated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub digest: Option<String>,
    /// Permit this installed plugin to call the current request's selected
    /// tools through the owned loop. Child calls do not inherit this grant.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub tool_calls: bool,
    /// Trigger-owned String inputs hidden from the model. Resolution uses the
    /// same correlation/source-field owner as bounded datastore tools; a caller
    /// cannot supply or replace one of these fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<super::WriteToolField>>", optional = nullable))]
    pub input_fields: Vec<super::WriteToolField>,
    /// Lets this plugin run job steps within the block's ceiling; the installed
    /// plugin must declare `job: true`. Absent means non-job verbs only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub job: Option<gents_protocol::job::PluginJobSettings>,
}

impl PluginToolRef {
    /// The name the model calls this tool by.
    pub fn tool_name(&self) -> &str {
        self.plugin
            .split_once('/')
            .map_or(self.plugin.as_str(), |(_, name)| name)
    }
}

/// Language-server settings embedded in `IntegrationTools`.
/// The operator payload is validated with the rest of the tool document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct LspTools {
    /// Existing language-server flags and catalog overrides. Absent uses defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub config: Option<String>,
    /// Action timeout, including indexing retries, when a call omits
    /// `timeout`. Unset uses 20s. Clamped to the maximum below.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Longest action timeout a call can request; calls ask for at least 5s.
    /// Unset uses 300s; larger values are clamped to 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_timeout_secs: Option<i64>,
}

/// Existing self-configuration controls, grouped for a separate design review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SelfConfigTools {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_self_config: Option<bool>,
    /// Self-config category allowlist; unset means the core spine
    /// (agent, tools, profile). See `config_client::patch`.
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
    /// Grants the `preview` verb: `config` may validate a change and return
    /// its diff without writing. It does not restrict writes; `edit` applies
    /// whether or not previews are granted.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub self_config_preview: Option<bool>,
    /// Opt-in authority to install packs from the home store or registry for
    /// this node. Disabled by absence and never implied by
    /// general self-config.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub enable_pack_install: Option<bool>,
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
        fn at_most(errors: &mut Vec<String>, field: &str, value: Option<i64>, limit: usize) {
            positive(errors, field, value);
            if value.is_some_and(|value| value > limit as i64) {
                errors.push(format!("{field} must be at most {limit}"));
            }
        }
        fn output_chars(errors: &mut Vec<String>, field: &str, value: Option<i64>) {
            at_most(
                errors,
                field,
                value,
                crate::tools::native::MAX_CONFIGURED_COMMAND_CHARS,
            );
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
            let mut seen = rapidhash::RapidHashSet::default();
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
        if let Some(built_ins) = &self.built_ins {
            for collection in &built_ins.p2p_collections {
                if let Err(error) =
                    crate::store::graphql::validate_collection_identifier(collection)
                {
                    errors.push(format!("built_ins.p2p_collections: {error}"));
                }
            }
        }
        if let Some(host) = &self.host {
            if let Some(files) = &host.files {
                at_most(
                    &mut errors,
                    "host.files.max_read_chars",
                    files.max_read_chars,
                    crate::tools::native::MAX_CONFIGURED_FILE_CHARS,
                );
                for (field, value) in [
                    ("host.files.max_list_entries", files.max_list_entries),
                    ("host.files.max_matches", files.max_matches),
                ] {
                    at_most(
                        &mut errors,
                        field,
                        value,
                        crate::tools::native::MAX_CONFIGURED_FILE_RESULTS,
                    );
                }
            }
            if let Some(bash) = &host.bash {
                bounded(
                    &mut errors,
                    "host.bash.timeout_secs",
                    bash.timeout_secs,
                    bash.max_timeout_secs,
                    crate::tools::native::DEFAULT_COMMAND_TIMEOUT_SECS as i64,
                    None,
                );
                bounded(
                    &mut errors,
                    "host.bash.wait_timeout_secs",
                    bash.wait_timeout_secs,
                    bash.max_wait_timeout_secs,
                    crate::tools::background::status::DEFAULT_WAIT_PROCESS_TIMEOUT_SECS as i64,
                    Some(crate::tools::background::status::MAX_WAIT_PROCESS_TIMEOUT_SECS as i64),
                );
                positive(
                    &mut errors,
                    "host.bash.background_timeout_secs",
                    bash.background_timeout_secs,
                );
                output_chars(
                    &mut errors,
                    "host.bash.max_output_chars",
                    bash.max_output_chars,
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
                output_chars(
                    &mut errors,
                    &format!("host.cli[{}].max_output_chars", cli.name),
                    cli.max_output_chars,
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
                    crate::tools::background::status::DEFAULT_WAIT_PROCESS_TIMEOUT_SECS as i64,
                    Some(crate::tools::background::status::MAX_WAIT_PROCESS_TIMEOUT_SECS as i64),
                );
            }
        }
        if let Some(agents) = &self.agents {
            names(
                &mut errors,
                "agents.target_ids",
                agents.target_ids.iter().map(String::as_str),
                false,
            );
        }
        if let Some(datastore) = &self.datastore {
            names(
                &mut errors,
                "datastore.write_collections",
                datastore
                    .write_collections
                    .iter()
                    .flatten()
                    .map(String::as_str),
                false,
            );
            for collection in datastore.write_collections.iter().flatten() {
                if let Err(error) =
                    crate::store::graphql::validate_collection_identifier(collection)
                {
                    errors.push(format!("datastore.write_collections: {error}"));
                }
            }
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
            let mut plugin_tool_names = std::collections::BTreeSet::new();
            for plugin in integrations.plugins.iter().flatten() {
                let mut inputs = std::collections::BTreeSet::new();
                for field in &plugin.input_fields {
                    if crate::store::graphql::validate_graphql_name(&field.name).is_err()
                        || field.fill.is_none()
                        || field.required
                        || !inputs.insert(&field.name)
                    {
                        errors.push(format!(
                            "integrations.plugins: {:?} input_fields must be distinct runtime-filled fields with required omitted",
                            plugin.plugin
                        ));
                    }
                }
                if let Err(error) = crate::plugin::store::parse_coordinate(&plugin.plugin) {
                    errors.push(format!("integrations.plugins: {error}"));
                } else if !plugin_tool_names.insert(plugin.tool_name()) {
                    errors.push(format!(
                        "integrations.plugins names two plugins called {:?}; a tool name must be unique",
                        plugin.tool_name()
                    ));
                }
                if let Some(job) = &plugin.job {
                    for error in crate::tools::background::job::settings::validate(job) {
                        errors.push(format!("integrations.plugins[{}].{error}", plugin.plugin));
                    }
                }
                if let Some(digest) = &plugin.digest {
                    let hex = digest.strip_prefix("sha256:").unwrap_or_default();
                    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                        errors.push(format!(
                            "integrations.plugins: {:?} pins {digest:?}, which is not sha256:<64 hex>",
                            plugin.plugin
                        ));
                    }
                }
            }
            if let Some(lsp) = &integrations.lsp {
                bounded(
                    &mut errors,
                    "integrations.lsp.timeout_secs",
                    lsp.timeout_secs,
                    lsp.max_timeout_secs,
                    crate::tools::native::lsp::DEFAULT_ACTION_TIMEOUT_SECS as i64,
                    Some(crate::tools::native::lsp::MAX_ACTION_TIMEOUT_SECS as i64),
                );
                if let Err(error) = crate::tools::native::lsp::LspConfigDocument::parse_operator(
                    lsp.config.as_deref(),
                ) {
                    errors.push(format!("invalid integrations.lsp.config: {error}"));
                }
            }
        }
        if let Some(self_config) = &self.self_config {
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
                if !crate::store::config_client::patch::SELF_CONFIG_CATEGORIES
                    .contains(&category.as_str())
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

/// The Tools document an agent selects, under its owner: agent ->
/// context -> `tools_id`. `None` when the agent has no context or the
/// context selects no Tools; a missing referenced document is an error.
pub async fn load_agent_tools_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent_id: &str,
) -> anyhow::Result<Option<Tools>> {
    Ok(load_agent_context_and_tools_in_txn(txn, owner, agent_id)
        .await?
        .1)
}

/// [`load_agent_tools_in_txn`] that also returns the agent's context: the
/// context is `None` when the agent has none, the Tools when the context
/// selects none; a missing referenced document is an error.
pub async fn load_agent_context_and_tools_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent_id: &str,
) -> anyhow::Result<(Option<super::AgentContext>, Option<Tools>)> {
    use crate::store::collection::Collection;
    use crate::store::config_client::read_desired_state_document_in_txn as read;
    use anyhow::Context as _;
    let agent: super::Agent = serde_json::from_value(
        read(txn, Collection::Agent, owner, agent_id)
            .await?
            .with_context(|| format!("Agent {agent_id} not found for {owner}"))?,
    )?;
    let Some(context_id) = agent.context_id else {
        return Ok((None, None));
    };
    let context: super::AgentContext = serde_json::from_value(
        read(txn, Collection::AgentContext, owner, &context_id)
            .await?
            .with_context(|| format!("AgentContext {context_id} not found for {owner}"))?,
    )?;
    let Some(tools_id) = context.tools_id.as_deref() else {
        return Ok((Some(context), None));
    };
    let tools = serde_json::from_value(
        read(txn, Collection::Tools, owner, tools_id)
            .await?
            .with_context(|| format!("Tools {tools_id} not found for {owner}"))?,
    )?;
    Ok((Some(context), Some(tools)))
}

/// What wiring changed, answer first: the changes, then the job block the
/// operator consents to, then warnings and the next call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireReport {
    pub changed: Vec<String>,
    /// The plugin's job block after the change; `None` means non-job verbs only.
    pub job: Option<gents_protocol::job::PluginJobSettings>,
    pub warnings: Vec<String>,
    /// Set only when no job block was written (R14).
    pub next_call: Option<String>,
}

fn union_into(list: &mut Vec<String>, items: &[String]) {
    for item in items {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
}

/// Merges `new` into `existing` without widening anything `existing` grants
/// (targets and trusted packs are the additions).
fn merge_job_blocks(
    existing: &gents_protocol::job::PluginJobSettings,
    new: &gents_protocol::job::PluginJobSettings,
) -> gents_protocol::job::PluginJobSettings {
    use gents_protocol::job::DynamicMode;
    fn rank(mode: DynamicMode) -> u8 {
        match mode {
            DynamicMode::Off => 0,
            DynamicMode::Check => 1,
            DynamicMode::Run => 2,
        }
    }
    let mut merged = existing.clone();
    union_into(&mut merged.agent_targets, &new.agent_targets);
    union_into(&mut merged.policy.trusted_packs, &new.policy.trusted_packs);
    merged.max_live = existing.max_live.min(new.max_live);
    merged.max_spawns = existing.max_spawns.min(new.max_spawns);
    merged.max_messages_per_slot = existing
        .max_messages_per_slot
        .min(new.max_messages_per_slot);
    merged.result_bytes = existing.result_bytes.min(new.result_bytes);
    merged.max_wall_secs = existing.max_wall_secs.min(new.max_wall_secs);
    if rank(new.policy.dynamic) < rank(existing.policy.dynamic) {
        merged.policy.dynamic = new.policy.dynamic;
    }
    merged
        .policy
        .dynamic_targets
        .retain(|id| new.policy.dynamic_targets.contains(id));
    merged
}

/// Wires an installed plugin into an agent's Tools and Context: the plugin
/// entry pinned to the installed digest, its job block (the reviewed block, or
/// the defaults over `targets_from`; a merge never widens an existing block),
/// the targets, trusted packs and skills. Each `targets_from` entry is
/// (pack coordinate, the AgentTarget ids that pack ships, its provenance).
/// When the Tools enable the p2p tool, the collections the plugin's data
/// declaration uses from its pack's own SDL join `built_ins.p2p_collections`,
/// so its agent can pair them with enrolled peers; enrollment and ACP still
/// gate every pairing. `agents.enabled` is never written.
pub fn wire_plugin(
    tools: &Tools,
    context: &super::AgentContext,
    plugin: &crate::plugin::store::InstalledPlugin,
    reviewed_job: Option<&gents_protocol::job::PluginJobSettings>,
    skill_ids: &[String],
    targets_from: &[(String, Vec<String>, crate::pack::InstalledVia)],
) -> anyhow::Result<(Tools, super::AgentContext, WireReport)> {
    use crate::tools::background::job::settings;
    use gents_protocol::job::{JobPolicy, PluginJobSettings};

    let coordinate = format!("{}/{}", plugin.namespace, plugin.name);
    let mut tools = tools.clone();
    let mut context = context.clone();
    let mut changed = Vec::new();
    let mut warnings = Vec::new();

    let plugins = tools
        .integrations
        .get_or_insert_with(Default::default)
        .plugins
        .get_or_insert_with(Default::default);
    let index = match plugins.iter().position(|entry| entry.plugin == coordinate) {
        Some(index) => {
            plugins[index].digest = Some(plugin.digest.clone());
            changed.push(format!("pinned plugin {coordinate} to {}", plugin.digest));
            index
        }
        None => {
            plugins.push(PluginToolRef {
                plugin: coordinate.clone(),
                digest: Some(plugin.digest.clone()),
                tool_calls: false,
                input_fields: Vec::new(),
                job: None,
            });
            changed.push(format!("added plugin {coordinate} at {}", plugin.digest));
            plugins.len() - 1
        }
    };

    let agents = tools.agents.get_or_insert_with(Default::default);
    let targets_before = agents.target_ids.clone();
    let mut job = plugins[index].job.clone();
    if plugin.declaration.job.0 {
        let incoming = reviewed_job.cloned().or_else(|| {
            (!targets_from.is_empty()).then(|| PluginJobSettings {
                agent_targets: Vec::new(),
                max_live: PluginJobSettings::DEFAULT_MAX_LIVE,
                max_spawns: PluginJobSettings::DEFAULT_MAX_SPAWNS,
                max_messages_per_slot: PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT,
                result_bytes: PluginJobSettings::DEFAULT_RESULT_BYTES,
                max_wall_secs: PluginJobSettings::DEFAULT_MAX_WALL_SECS,
                policy: JobPolicy {
                    trusted_packs: plugin.owner_pack_coordinate.iter().cloned().collect(),
                    ..JobPolicy::default()
                },
            })
        });
        if let Some(incoming) = incoming {
            let merged = job.is_some();
            let mut block = match &job {
                Some(existing) => merge_job_blocks(existing, &incoming),
                None => incoming,
            };
            for (pack, pack_targets, via) in targets_from {
                (block.policy, block.agent_targets, agents.target_ids) =
                    settings::wire_targets_from(
                        pack,
                        pack_targets,
                        block.policy,
                        &block.agent_targets,
                        &agents.target_ids,
                    );
                changed.push(format!("trusted pack {pack} and wired its targets"));
                if *via != crate::pack::InstalledVia::Operator {
                    warnings.push(format!(
                        "trusted_pack_not_operator: {pack} is not operator-installed; its scripts classify as dynamic"
                    ));
                }
            }
            union_into(&mut agents.target_ids, &block.agent_targets);
            let problems = settings::validate(&block);
            anyhow::ensure!(
                problems.is_empty(),
                "job block for {coordinate} is not valid: {}",
                problems.join("; ")
            );
            changed.push(format!(
                "{} job block for {coordinate}",
                if merged { "merged" } else { "wrote" }
            ));
            job = Some(block);
        }
    }
    plugins[index].job = job.clone();

    let added: Vec<String> = agents
        .target_ids
        .iter()
        .filter(|id| !targets_before.contains(id))
        .cloned()
        .collect();
    if !added.is_empty() {
        changed.push(format!("added agent targets {}", added.join(", ")));
        if agents.enabled == Some(true) {
            warnings.push(format!(
                "agent_new_widened: {} are also reachable by agent_new",
                added.join(", ")
            ));
        }
    }

    let skills: Vec<String> = skill_ids
        .iter()
        .filter(|id| !context.skill_ids.contains(id))
        .cloned()
        .collect();
    if !skills.is_empty() {
        changed.push(format!("added skills {}", skills.join(", ")));
        context.skill_ids.extend(skills);
    }

    if let Some(built_ins) = tools
        .built_ins
        .as_mut()
        .filter(|built_ins| built_ins.enable_p2p_tool == Some(true))
    {
        let granted: Vec<String> = plugin
            .data_collection_fields
            .keys()
            .filter(|collection| !built_ins.p2p_collections.contains(collection))
            .cloned()
            .collect();
        if !granted.is_empty() {
            changed.push(format!("granted P2P pairing of {}", granted.join(", ")));
            built_ins.p2p_collections.extend(granted);
        }
    }

    let next_call = (plugin.declaration.job.0 && job.is_none())
        .then(|| format!("gents pack wire {coordinate} --agent <agent> --targets-from <pack>"));
    Ok((
        tools,
        context,
        WireReport {
            changed,
            job,
            warnings,
            next_call,
        },
    ))
}

/// Inverse of [`wire_plugin`]. With no `targets_from`, drops the plugin entry
/// and then its job targets and the `skill_ids`, each only when no other
/// plugin entry still needs it (skills are not recorded per plugin, so any
/// remaining plugin keeps them). With `targets_from`, keeps the plugin and
/// removes only those packs' targets and trust entries; a block left without
/// targets is dropped. `agents.enabled`, `policy.dynamic` and
/// `built_ins.p2p_collections` are never written: a P2P grant may back a live
/// pairing or another plugin, so narrowing it stays an explicit Tools edit.
pub fn unwire_plugin(
    tools: &Tools,
    context: &super::AgentContext,
    plugin: &crate::plugin::store::InstalledPlugin,
    skill_ids: &[String],
    targets_from: &[(String, Vec<String>, crate::pack::InstalledVia)],
) -> anyhow::Result<(Tools, super::AgentContext, WireReport)> {
    use crate::tools::background::job::settings;

    let coordinate = format!("{}/{}", plugin.namespace, plugin.name);
    let mut tools = tools.clone();
    let mut context = context.clone();
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    let mut next_call = None;

    let mut plugins = tools
        .integrations
        .as_ref()
        .and_then(|integrations| integrations.plugins.clone())
        .unwrap_or_default();
    let Some(index) = plugins.iter().position(|entry| entry.plugin == coordinate) else {
        warnings.push(format!("{coordinate} is not wired; nothing to remove"));
        return Ok((
            tools,
            context,
            WireReport {
                changed,
                job: None,
                warnings,
                next_call,
            },
        ));
    };
    let entry = plugins.remove(index);
    let referenced_elsewhere = |id: &String| {
        plugins
            .iter()
            .filter_map(|other| other.job.as_ref())
            .any(|block| {
                block.agent_targets.contains(id) || block.policy.dynamic_targets.contains(id)
            })
    };

    let mut job = entry.job.clone();
    let mut dropped_targets: Vec<String> = Vec::new();
    if targets_from.is_empty() {
        changed.push(format!("removed plugin {coordinate}"));
        dropped_targets = entry
            .job
            .iter()
            .flat_map(|block| block.agent_targets.clone())
            .collect();
        job = None;
        let skills: Vec<&String> = skill_ids
            .iter()
            .filter(|id| context.skill_ids.contains(id))
            .collect();
        if plugins.is_empty() && !skills.is_empty() {
            changed.push(format!(
                "removed skills {}",
                skills
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            context.skill_ids.retain(|id| !skill_ids.contains(id));
        }
    } else if let Some(block) = job.as_mut() {
        for (pack, pack_targets, _) in targets_from {
            dropped_targets.extend(
                block
                    .agent_targets
                    .iter()
                    .filter(|id| pack_targets.contains(id))
                    .cloned(),
            );
            block.agent_targets.retain(|id| !pack_targets.contains(id));
            if plugin.owner_pack_coordinate.as_deref() != Some(pack.as_str()) {
                block.policy.trusted_packs.retain(|trusted| trusted != pack);
            }
            changed.push(format!("untrusted pack {pack} and unwired its targets"));
        }
        block
            .policy
            .dynamic_targets
            .retain(|id| block.agent_targets.contains(id));
        if block.agent_targets.is_empty() {
            job = None;
            changed.push(format!("dropped the job block for {coordinate}"));
            next_call = Some(format!(
                "{coordinate} now serves non-job verbs only; to run jobs again: gents pack wire {coordinate} --agent <agent> --targets-from <pack>"
            ));
        } else {
            let problems = settings::validate(block);
            anyhow::ensure!(
                problems.is_empty(),
                "job block for {coordinate} is not valid: {}",
                problems.join("; ")
            );
        }
    }

    if let Some(agents) = tools.agents.as_mut() {
        let kept = |id: &String| {
            referenced_elsewhere(id) || job.as_ref().is_some_and(|b| b.agent_targets.contains(id))
        };
        let removed: Vec<String> = dropped_targets
            .into_iter()
            .filter(|id| agents.target_ids.contains(id) && !kept(id))
            .collect();
        if !removed.is_empty() {
            agents.target_ids.retain(|id| !removed.contains(id));
            changed.push(format!("removed agent targets {}", removed.join(", ")));
        }
    }

    if targets_from.is_empty() {
        if plugins.is_empty() {
            if let Some(integrations) = tools.integrations.as_mut() {
                integrations.plugins = None;
                if *integrations == IntegrationTools::default() {
                    tools.integrations = None;
                }
            }
        } else {
            tools
                .integrations
                .get_or_insert_with(Default::default)
                .plugins = Some(plugins);
        }
    } else {
        let mut entry = entry;
        entry.job = job.clone();
        plugins.insert(index, entry);
        tools
            .integrations
            .get_or_insert_with(Default::default)
            .plugins = Some(plugins);
    }

    Ok((
        tools,
        context,
        WireReport {
            changed,
            job,
            warnings,
            next_call,
        },
    ))
}

/// Compact full-document serialization. Explicit sparse patches use a separate
/// representation so an omitted field is not confused with clearing a field.
fn is_default<T>(value: &T) -> bool
where
    T: Default + PartialEq,
{
    value == &T::default()
}

#[cfg(test)]
mod tests;
