//! Compiler and publication adapter for model-proposed document graphs.
//!
//! Inputs to this module are untrusted proposals. Compilation resolves only
//! operator-approved wrappers around existing Tasks. Publication creates only
//! ordinary EventTriggers; the existing runtime remains the sole executor.

mod compiler;
mod run;
mod runtime;
mod tools;
mod types;

pub use compiler::{
    bind_package_plan, compile_graph, graph_plan_digest, verify_graph_plan_digest, CompilerPolicy,
    GraphCompileError,
};
pub(crate) use run::run_graph_run_reconciler;
pub use run::{
    load_graph_run_view, load_graph_run_view_with_access, reconcile_graph_run,
    reconcile_graph_run_with_access, reconcile_owned_graph_runs, request_graph_run_cancellation,
    request_graph_run_cancellation_with_access, GraphResultRef, GraphRunGroupView,
    GraphRunRequestView, GraphRunResultView, GraphRunStageView, GraphRunView,
};
pub use runtime::{
    activate_graph_revision, activate_graph_revision_with_access, graph_run_terminal_decision,
    materialize_graph_revision, publish_graph_plan, revision_gate_decision, start_graph_run,
    start_graph_run_with_access, ActivationReceipt, GraphRunReceipt, GraphRunTerminalDecision,
    MaterializedRevision, PublishedGraph, RevisionGateDecision,
};
pub(crate) use runtime::{
    ensure_graph_revision_receipt, materialize_graph_revision_in_txn,
    record_graph_revision_materialization_failure,
};
pub(crate) use runtime::{
    graph_artifact_is_visible, graph_materialization_denial, load_visible_package_artifact_ids,
};
pub use tools::{
    CompileGraphArgs, CompileGraphResponse, CompileGraphTool, GraphPipelineToolError,
    COMPILE_GRAPH_TOOL_NAME, GRAPH_PIPELINE_TOOL_NAMES,
};
pub use types::{
    BundledProvenance, CapabilityManifestEntry, DeliveryConcurrency, DeliveryMode, Diagnostic,
    DiagnosticCode, EntryBinding, GraphEdge, GraphIntent, GraphLimits, GraphNode, GraphPlan,
    GroupCount, NetworkAuthorityCeiling, PackageArtifactKind, PackageBindingValue, PackagePlan,
    PackageRoleBinding, PlannedEdge, PlannedEntry, PlannedNode, PlannedPackageArtifact,
    PlannedResult, PortCardinality, PortRef, PortSpec, RequiredSchemaDigest, ResultCardinality,
    ResultContract, ResultPredicate, StageAuthorityCeiling, StageCapability,
    WorkspaceAuthorityCeiling, COMPILER_VERSION,
};

#[cfg(test)]
mod tests;
