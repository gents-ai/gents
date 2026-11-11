use anyhow::Result;
use rapidhash::{HashSetExt, RapidHashSet};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Callback {
    pub callback_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub handler: CallbackHandler,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub capabilities: Vec<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum CallbackHandler {
    BuiltIn {
        emitter: BuiltInCallback,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        workspace_agents: Option<WorkspaceAgentSelection>,
    },
    Module {
        module_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        workspace_agents: Option<WorkspaceAgentSelection>,
    },
    Plugin {
        // The artifact is referenced by content hash: any other installed
        // artifact is refused.
        plugin: String,
        digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        correlation_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        max_attempts: Option<u32>,
        #[serde(
            default,
            deserialize_with = "super::serde_helpers::deserialize_default_on_null",
            skip_serializing_if = "Vec::is_empty"
        )]
        #[cfg_attr(
            feature = "typescript",
            ts(as = "Option<Vec<crate::documents::PortSpec>>", optional = nullable)
        )]
        #[schemars(transform = crate::accepts_null)]
        outputs: Vec<crate::PortSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct WorkspaceAgentSelection {
    pub writer_agent_did: String,
    pub integrator_agent_did: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<WorkspaceReaderSelection>>", optional = nullable))]
    pub readers: Vec<WorkspaceReaderSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct WorkspaceReaderSelection {
    pub agent_did: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum PortCardinality {
    One,
    Many,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PortSpec {
    pub name: String,
    /// Existing DefraDB collection carried on this port.
    pub collection: String,
    /// Stable schema reference used for compile-time compatibility checks.
    pub schema: String,
    /// Source field used by event-trigger correlation and fan-in.
    pub correlation_field: String,
    pub cardinality: PortCardinality,
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BuiltInCallback {
    CreateWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CallbackBinding {
    pub agent_did: String,
    pub binding_id: String,
    pub node_did: String,
    pub event_source_id: String,
    pub callback_id: String,
    // Projection applies to every member of a grouped delivery, preserving
    // group identity.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub input_fields: Vec<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

impl CallbackBinding {
    pub fn projected_fields(&self) -> Result<Vec<String>> {
        validate_callback_binding(self)?;
        Ok(self.input_fields.clone())
    }
}

pub fn validate_callback_binding(binding: &CallbackBinding) -> Result<()> {
    for (name, value) in [
        ("binding_id", &binding.binding_id),
        ("node_did", &binding.node_did),
        ("agent_did", &binding.agent_did),
        ("callback_id", &binding.callback_id),
        ("event_source_id", &binding.event_source_id),
    ] {
        anyhow::ensure!(
            !value.trim().is_empty(),
            "CallbackBinding {name} must be non-empty"
        );
    }
    identity::Did::new(binding.agent_did.clone())?;
    anyhow::ensure!(
        binding.agent_did != binding.node_did,
        "callback agent cannot be its routing node"
    );
    let mut unique = RapidHashSet::new();
    for field in &binding.input_fields {
        gents_store::graphql::validate_graphql_name(field)?;
        anyhow::ensure!(
            unique.insert(field),
            "duplicate callback input field {field}"
        );
        anyhow::ensure!(
            !crate::callback::is_secret_env_name(field),
            "CallbackBinding {} source field `{field}` is secret-bearing",
            binding.binding_id
        );
    }
    Ok(())
}

// Resource limits retain their executor defaults when absent; signer_did is
// provenance, not execution owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CallbackModule {
    pub module_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub abi_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub artifact_bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub canonical_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub signer_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub provenance: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fuel_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub memory_pages: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_input_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_output_bytes: Option<i64>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum CallbackInvocationOrigin {
    Event {
        binding_id: String,
        source_collection: String,
        source_doc_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_version: Option<String>,
    },
    // One invocation for one sealed delivery group, never an arbitrary member;
    // the delivery owner deduplicates the group.
    EventGroup {
        binding_id: String,
        group_key: String,
    },
}

impl CallbackInvocationOrigin {
    pub fn binding_id(&self) -> &str {
        match self {
            Self::Event { binding_id, .. } | Self::EventGroup { binding_id, .. } => binding_id,
        }
    }
}

/// Configuration validation and command environments share this predicate, so
/// no configured field may carry a name a command environment would strip.
pub fn is_secret_env_name(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key.contains("KEY")
        || key.contains("SECRET")
        || key.contains("TOKEN")
        || key.contains("PASSWORD")
}
