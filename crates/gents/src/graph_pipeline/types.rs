use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const COMPILER_VERSION: &str = "graph-intent-v3";

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
pub enum GroupCount {
    Static { count: u32 },
    SourceField { field: String },
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryConcurrency {
    #[default]
    Parallel,
    Serial,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DeliveryMode {
    PerDocument,
    PerGroup {
        expected: GroupCount,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
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
    #[serde(default)]
    pub concurrency: DeliveryConcurrency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryBinding {
    pub name: String,
    pub collection: String,
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_contract: Option<String>,
    pub to: PortRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ResultCardinality {
    Exactly { count: u32 },
    AtMost { count: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ResultPredicate {
    Distinct {
        field: String,
    },
    AllEqual {
        field: String,
    },
    CountEqualsField {
        field: String,
    },
    AllMatch {
        field: String,
        value: String,
    },
    SameMembers {
        field: String,
        result: String,
        result_field: String,
    },
    SubsetOf {
        field: String,
        result: String,
        result_field: String,
    },
    FieldEqualsResultCount {
        field: String,
        result: String,
    },
    FieldEqualsSum {
        field: String,
        terms: Vec<String>,
    },
    FieldEqualsField {
        field: String,
        result: String,
        result_field: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultContract {
    pub name: String,
    pub from: PortRef,
    pub cardinality: ResultCardinality,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub predicates: Vec<ResultPredicate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
pub struct GraphIntent {
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    pub entries: Vec<EntryBinding>,
    #[serde(default)]
    pub results: Vec<ResultContract>,
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
    InvalidGroupCountField,
    InvalidGroupTimeout,
    DuplicateResult,
    InvalidResultCardinality,
    InvalidResultPredicate,
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
    #[serde(default)]
    pub predicates: Vec<ResultPredicate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    tag = "type",
    content = "value",
    deny_unknown_fields
)]
pub enum PackageBindingValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    DocumentRef(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRoleBinding {
    pub principal_did: String,
    pub deployment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledProvenance {
    pub binary_version: String,
    pub build_commit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAuthorityCeiling {
    None,
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAuthorityCeiling {
    Disabled,
    Enabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageAuthorityCeiling {
    pub workspace: WorkspaceAuthorityCeiling,
    pub network: NetworkAuthorityCeiling,
    pub max_invocations: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageArtifactKind {
    Behavior,
    ToolSelection,
    ToolSurface,
    Task,
    Trigger,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedPackageArtifact {
    /// Stable package-local identity. The physical document ID is derived
    /// from this value and the final revision digest.
    pub logical_id: String,
    /// Immutable configuration-scoped document identity. This is derived
    /// before graph compilation from package digest plus typed bindings, so it
    /// may participate in the final graph digest without a circular hash.
    pub physical_id: String,
    pub kind: PackageArtifactKind,
    pub content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredSchemaDigest {
    pub namespace: String,
    pub digest: String,
    /// Minimal runtime readiness shape derived from the pinned SDL. The digest
    /// keeps provenance exact; this map lets peers fail closed without needing
    /// the originating binary's bundled catalog.
    pub collections: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePlan {
    pub name: String,
    pub version: String,
    pub package_digest: String,
    pub catalog_digest: String,
    pub bundled_provenance: BundledProvenance,
    #[serde(default)]
    pub bindings: BTreeMap<String, PackageBindingValue>,
    #[serde(default)]
    pub roles: BTreeMap<String, PackageRoleBinding>,
    #[serde(default)]
    pub effective_authority_ceiling: BTreeMap<String, StageAuthorityCeiling>,
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
