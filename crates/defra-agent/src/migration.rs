//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> v2 by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!
//! Idempotent: re-running on a v2 deployment is a no-op (collection already
//! patched, migration already registered).

use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

/// Run all pending tool-call migrations against the embedded node.
/// Called from the daemon startup path before any AgentToolCall reads.
#[allow(dead_code)] // wired in Task 9
pub(crate) async fn ensure_tool_call_migrations(
    _node: Arc<EmbeddedNode>,
) -> Result<()> {
    // Real implementation lands in Task 8.
    Ok(())
}
