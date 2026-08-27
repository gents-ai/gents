//! Compiler and publication adapter for model-proposed document graphs.
//!
//! Inputs to this module are untrusted proposals. Compilation resolves only
//! operator-approved wrappers around existing Tasks. Publication creates only
//! ordinary EventTriggers; the existing runtime remains the sole executor.

mod compiler;
mod runtime;
mod tools;
mod types;

pub use compiler::{
    bind_package_plan, compile_graph, graph_plan_digest, verify_graph_plan_digest, CompilerPolicy,
    GraphCompileError,
};
pub use runtime::{
    graph_run_terminal_decision, publish_graph_plan, revision_gate_decision,
    GraphRunTerminalDecision, PublishedGraph, RevisionGateDecision,
};
pub use tools::{
    CompileGraphArgs, CompileGraphResponse, CompileGraphTool, GraphPipelineToolError,
    COMPILE_GRAPH_TOOL_NAME, GRAPH_PIPELINE_TOOL_NAMES,
};
pub use types::{
    BundledProvenance, CapabilityManifestEntry, DeliveryConcurrency, DeliveryMode, Diagnostic,
    DiagnosticCode, EntryBinding, GraphEdge, GraphIntent, GraphLimits, GraphNode, GraphPlan,
    GroupCount, PackageArtifactKind, PackagePlan, PackageRoleBinding, PlannedEdge, PlannedEntry,
    PlannedNode, PlannedPackageArtifact, PlannedResult, PortCardinality, PortRef, PortSpec,
    RequiredSchemaDigest, ResultCardinality, ResultContract, StageCapability,
    WorkspaceAuthorityCeiling, COMPILER_VERSION,
};

#[cfg(test)]
mod tests;
