use serde::{Deserialize, Serialize};

/// Reusable event-driven action-plan callback. Existing modules, host actions,
/// and journals remain supported. General task command hooks do not require this
/// workspace-oriented planner; their integration with workspace callbacks is TODO.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Callback {
    pub callback_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub handler: CallbackHandler,
    /// Empty grants no host-action capabilities.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallbackHandler {
    BuiltIn { emitter: BuiltInCallback },
    Module { module_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInCallback {
    CreateWorkspace,
}

/// Event delivery selects a reusable callback, without duplicating its handler.
/// Uses the same EventSource configuration as task triggers. Event grouping
/// support must follow the shared delivery contract rather than be ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallbackBinding {
    pub binding_id: String,
    pub agent_did: String,
    pub event_source_id: String,
    pub callback_id: String,
    /// Exact source field names projected for this invocation. Empty passes no
    /// source data. Apply the existing schema-safe field validation. For grouped
    /// delivery, apply this projection to every member, preserving group identity.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub input_fields: Vec<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Existing WASM callback module configuration. Resource limits retain their
/// executor defaults when absent; signer_did is provenance, not execution owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallbackModule {
    pub module_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abi_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wasm_bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuel_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_pages: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<i64>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Durable event callback origin. General task commands use request execution
/// ownership, not a fabricated event or a workspace callback invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallbackInvocationOrigin {
    Event {
        binding_id: String,
        source_collection: String,
        source_doc_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_version: Option<String>,
    },
    /// One invocation for one sealed delivery group, never an arbitrary member.
    /// The existing delivery owner persists membership and deduplicates the group.
    EventGroup {
        binding_id: String,
        group_key: String,
    },
}
