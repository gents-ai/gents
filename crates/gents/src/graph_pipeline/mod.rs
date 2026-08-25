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
    compile_graph, graph_plan_digest, verify_graph_plan_digest, CompilerPolicy, GraphCompileError,
};
pub use runtime::{publish_graph_plan, PublishedGraph};
pub use tools::{
    CompileGraphArgs, CompileGraphResponse, CompileGraphTool, GraphPipelineToolError,
    COMPILE_GRAPH_TOOL_NAME, GRAPH_PIPELINE_TOOL_NAMES,
};
pub use types::{
    DeliveryMode, Diagnostic, DiagnosticCode, EntryBinding, GraphEdge, GraphIntent, GraphLimits,
    GraphNode, GraphPlan, PlannedEdge, PlannedEntry, PlannedNode, PortCardinality, PortRef,
    PortSpec, StageCapability, COMPILER_VERSION,
};

#[cfg(test)]
mod tests;
