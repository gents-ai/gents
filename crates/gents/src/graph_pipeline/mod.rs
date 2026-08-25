//! Pure compiler for model-proposed document graphs.
//!
//! Inputs to this module are untrusted proposals. Compilation resolves only
//! operator-approved stage capabilities and performs no I/O. Persistence and
//! activation live behind a separate controller boundary.

mod compiler;
mod types;

pub use compiler::{
    compile_graph, graph_plan_digest, verify_graph_plan_digest, CompilerPolicy, GraphCompileError,
};
pub use types::{
    DeliveryMode, Diagnostic, DiagnosticCode, EntryBinding, GraphEdge, GraphIntent, GraphLimits,
    GraphNode, GraphPlan, PlannedEdge, PlannedEntry, PlannedNode, PortCardinality, PortRef,
    PortSpec, StageCapability, COMPILER_VERSION,
};

#[cfg(test)]
mod tests;
