use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const COMPILER_VERSION: &str = "graph-intent-v4";

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
    pub collection: String,
    pub schema: String,
    pub correlation_field: String,
    pub cardinality: PortCardinality,
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub required: bool,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum StageTarget {
    Task { task_id: String },
    Plugin {
        /// `namespace/name` of the installed plugin; a pack's own plugin is
        /// qualified to `ns/name` when the pack loads.
        plugin: String,
        /// `sha256:<hex>` of the artifact that runs; a pack's own plugin is
        /// pinned when the pack is loaded, and compiling an unpinned one fails.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        digest: Option<String>,
        /// Total attempts a failed call may get; a retry never follows a call
        /// whose results were written.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        max_attempts: Option<u32>,
    },
}

impl StageTarget {
    pub fn max_attempts(&self) -> u32 {
        match self {
            Self::Plugin { max_attempts, .. } => crate::plugin::attempts_allowed(*max_attempts),
            Self::Task { .. } => 1,
        }
    }
}

impl StageTarget {
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Task { task_id } => Some(task_id),
            Self::Plugin { .. } => None,
        }
    }
}

/// Operator-approved interface around a Task or an installed plugin. The
/// model can select a capability revision but cannot author its agent, prompt,
/// tools, model, output permissions, or the plugin that runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct StageCapability {
    /// Every caller, including the owner, needs explicit allowed_callers
    /// admission and ACP.
    pub node_did: String,
    pub capability_id: String,
    pub revision: String,
    pub target: StageTarget,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<PortSpec>>", optional = nullable))]
    pub input_ports: Vec<PortSpec>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<PortSpec>>", optional = nullable))]
    pub output_ports: Vec<PortSpec>,
    /// Empty means nobody, not everybody.
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub allowed_callers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub workspace_authority: Option<WorkspaceAuthority>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PortRef {
    pub node_id: String,
    pub port: String,
}

pub type GroupCount = crate::document_config::EventGroupCount;
pub type DeliveryMode = Option<crate::document_config::EventGroup>;
/// Trigger concurrency on a graph edge. Compilation refuses `latest_only` on
/// every edge (`DiagnosticCode::InvalidEdgeConcurrency`; Lean
/// `GraphPipeline.graphEdgeConcurrencyValid`): on a per-document edge a
/// `latest_only` trigger supersedes the same trigger's in-flight requests,
/// cancelling earlier fan-out items; on a grouped edge the trigger engine
/// never applies the mode.
pub type DeliveryConcurrency = crate::document_config::ConcurrencyMode;

/// Selects a singleton Task invocation in the same pinned graph correlation.
/// Runtime resolution requires exactly one authenticated root; node names alone
/// never authorize a session or transfer the firing node's attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphSessionSelection {
    #[serde(rename = "continue")]
    pub continue_node_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphNode {
    pub node_id: String,
    pub capability_id: String,
    pub capability_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub session: Option<GraphSessionSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphEdge {
    pub from: PortRef,
    pub to: PortRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<crate::document_config::EventGroup>", optional = nullable))]
    pub delivery: DeliveryMode,
    #[serde(default)]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<DeliveryConcurrency>", optional)
    )]
    pub concurrency: DeliveryConcurrency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EntryBinding {
    pub name: String,
    pub collection: String,
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub input_contract: Option<String>,
    pub to: PortRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "unknown", optional = nullable))]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub prepare: Option<EntryPrepare>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EntryPrepare {
    pub host: Vec<HostInput>,
    /// This pack's plugin by name; qualified to `ns/name` when the pack loads.
    pub plugin: String,
    /// `sha256:<hex>` of the artifact that runs; pinned when the pack loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub digest: Option<String>,
    pub writes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum HostInput {
    GitDiff {
        repository_field: String,
        base_field: String,
        head_field: String,
        unified_context_lines: u32,
        rename_similarity_percent: u8,
    },
    /// Read-only workspace over the `GitDiff` head; requires an earlier
    /// `GitDiff` in the same `host` list.
    ReadOnlyWorkspace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ResultCardinality {
    Exactly { count: u32 },
    AtMost { count: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ResultContract {
    pub name: String,
    pub from: PortRef,
    pub cardinality: ResultCardinality,
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphLimits {
    pub max_nodes: u32,
    pub max_edges: u32,
    pub max_depth: u32,
    pub max_fan_out: u32,
    pub max_total_invocations: u32,
    pub max_runtime_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphIntent {
    pub node_did: String,
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<GraphEdge>>", optional = nullable))]
    pub edges: Vec<GraphEdge>,
    pub entries: Vec<EntryBinding>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ResultContract>>", optional = nullable))]
    pub results: Vec<ResultContract>,
    pub limits: GraphLimits,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    EmptyGraphId,
    EmptyGraph,
    DuplicateNode,
    DuplicateEntry,
    DuplicateCapability,
    DuplicatePort,
    DuplicateOutputCollection,
    UnknownCapability,
    CapabilityRevisionMismatch,
    UnauthorizedCapability,
    UnknownNode,
    InvalidSessionSelection,
    UnknownSessionTarget,
    NonTaskSessionTarget,
    NonSingletonSessionTarget,
    UnknownPort,
    InvalidCollection,
    InvalidCorrelationField,
    InvalidPredicate,
    SchemaMismatch,
    CorrelationMismatch,
    CardinalityMismatch,
    InvalidGroupSize,
    InvalidGroupCountField,
    InvalidGroupTimeout,
    InvalidEdgeConcurrency,
    DuplicateResult,
    MissingTerminalResult,
    InvalidResultCardinality,
    MultipleInputBindings,
    MissingInputBinding,
    UnreachableNode,
    Cycle,
    NodeLimitExceeded,
    EdgeLimitExceeded,
    DepthLimitExceeded,
    FanOutLimitExceeded,
    PlatformLimitExceeded,
    UnpinnedPlugin,
    ReadOnlyWorkspaceRequiresGitDiff,
    InvalidPrepareWrites,
    InvalidPrepareLimits,
    InvalidInputSchema,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedNode {
    pub node_id: String,
    pub capability_id: String,
    pub capability_revision: String,
    pub target: StageTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<GraphSessionSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_ports: Vec<PortSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedEdge {
    pub from: PortRef,
    pub to: PortRef,
    pub source_collection: String,
    pub target: StageTarget,
    pub correlation_field: String,
    pub delivery: DeliveryMode,
    pub concurrency: DeliveryConcurrency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedEntry {
    pub name: String,
    pub collection: String,
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_contract: Option<String>,
    pub to: PortRef,
    pub target: StageTarget,
    pub correlation_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare: Option<EntryPrepare>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedResult {
    pub name: String,
    pub from: PortRef,
    pub collection: String,
    pub schema: String,
    pub correlation_field: String,
    pub cardinality: ResultCardinality,
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledProvenance {
    pub binary_version: String,
    pub build_commit: String,
}

pub use crate::toolset::WorkspaceAuthority;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedPackageArtifact {
    pub collection: crate::Collection,
    pub logical_id: String,
    pub content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredSchemaDigest {
    pub namespace: String,
    pub digest: String,
    /// Peers compare their active DefraDB collection versions through these
    /// without the originating binary's bundled catalog.
    pub collection_contract_digests: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePlan {
    pub name: String,
    pub version: String,
    pub package_digest: String,
    pub bundled_provenance: BundledProvenance,
    #[serde(default)]
    pub workspace_authority: BTreeMap<String, WorkspaceAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_revision_digest: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<PlannedPackageArtifact>,
    #[serde(default)]
    pub required_schema_digests: Vec<RequiredSchemaDigest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifestEntry {
    pub capability_id: String,
    pub revision: String,
    pub target: StageTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPlan {
    pub compiler_version: String,
    pub graph_id: String,
    pub digest: String,
    pub nodes: Vec<PlannedNode>,
    pub edges: Vec<PlannedEdge>,
    pub entries: Vec<PlannedEntry>,
    #[serde(default)]
    pub results: Vec<PlannedResult>,
    pub capability_manifest: Vec<CapabilityManifestEntry>,
    pub limits: GraphLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackagePlan>,
}
