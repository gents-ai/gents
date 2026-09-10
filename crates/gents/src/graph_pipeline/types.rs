use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const COMPILER_VERSION: &str = "graph-intent-v3";

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

/// Operator-approved interface around an existing Task document.
///
/// The model can select a capability revision, but cannot author the Task's
/// behavior, prompt, tools, model, or output permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct StageCapability {
    /// Owner of this capability and its referenced task. Every caller,
    /// including this owner, needs explicit allowed_callers admission and ACP.
    pub agent_did: String,
    pub capability_id: String,
    pub revision: String,
    pub task_id: String,
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
    /// Optional graph execution ceiling; does not select a different behavior
    /// or inference profile from the referenced task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub workspace_authority: Option<WorkspaceAuthority>,
    /// UI/discovery metadata; explicit task/capability references define topology.
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

// Graph edges compile into the same event-group configuration used by ordinary
// triggers. No graph-only count or delivery-policy model. Compilation enforces
// graph cardinality/count bounds and rejects LatestOnly; it does not widen semantics.
pub type GroupCount = crate::document_config::EventGroupCount;
pub type DeliveryMode = Option<crate::document_config::EventGroup>;
pub type DeliveryConcurrency = crate::document_config::ConcurrencyMode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphNode {
    pub node_id: String,
    pub capability_id: String,
    pub capability_revision: String,
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
    /// Whole-run ceiling enforced by the durable GraphRun reconciler.
    pub max_total_invocations: u32,
    /// Wall-clock run bound enforced from the durable `GraphRun.started_at`.
    pub max_runtime_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GraphIntent {
    pub agent_did: String,
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
    /// UI/discovery metadata; explicit task/capability references define topology.
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    /// Stable JSON-pointer-like location in the submitted intent.
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedNode {
    pub node_id: String,
    pub capability_id: String,
    pub capability_revision: String,
    pub task_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedEdge {
    pub from: PortRef,
    pub to: PortRef,
    pub source_collection: String,
    pub target_task_id: String,
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
    pub target_task_id: String,
    pub correlation_field: String,
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
    /// Canonical collection and owner-scoped authored identity. Resolve the
    /// physical Defra document through the shared configuration owner.
    pub collection: crate::Collection,
    pub logical_id: String,
    pub content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredSchemaDigest {
    pub namespace: String,
    pub digest: String,
    /// Canonical full-contract digest for each collection in the pinned SDL.
    /// Peers can compare their active DefraDB collection versions without
    /// needing the originating binary's bundled catalog.
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
    pub task_id: String,
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
