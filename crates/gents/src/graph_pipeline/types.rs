use serde::{Deserialize, Serialize};

pub const COMPILER_VERSION: &str = "graph-intent-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortCardinality {
    One,
    Many,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    pub name: String,
    /// Existing DefraDB collection carried on this port.
    pub collection: String,
    /// Stable schema reference used for compile-time compatibility checks.
    pub schema: String,
    /// Existing field used by EventTrigger correlation and fan-in.
    pub correlation_field: String,
    pub cardinality: PortCardinality,
    #[serde(default)]
    pub required: bool,
}

/// Operator-approved interface around an existing Task document.
///
/// The model can select a capability revision, but cannot author the Task's
/// behavior, prompt, tools, model, or output permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageCapability {
    pub capability_id: String,
    pub revision: String,
    pub task_id: String,
    #[serde(default)]
    pub input_ports: Vec<PortSpec>,
    #[serde(default)]
    pub output_ports: Vec<PortSpec>,
    /// Empty means nobody, not everybody.
    #[serde(default)]
    pub allowed_callers: Vec<String>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct PortRef {
    pub node_id: String,
    pub port: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DeliveryMode {
    PerDocument,
    PerGroup { expected_count: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub node_id: String,
    pub capability_id: String,
    pub capability_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    pub from: PortRef,
    pub to: PortRef,
    pub delivery: DeliveryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryBinding {
    pub name: String,
    pub collection: String,
    pub schema: String,
    pub to: PortRef,
}

/// Structural compiler limits. These bound authoring work only; execution
/// limits remain the responsibility of the existing task/trigger runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphLimits {
    pub max_nodes: u32,
    pub max_edges: u32,
    pub max_depth: u32,
    pub max_fan_out: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphIntent {
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    pub entries: Vec<EntryBinding>,
    pub limits: GraphLimits,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedEntry {
    pub name: String,
    pub collection: String,
    pub schema: String,
    pub to: PortRef,
    pub target_task_id: String,
    pub correlation_field: String,
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
    pub limits: GraphLimits,
}
