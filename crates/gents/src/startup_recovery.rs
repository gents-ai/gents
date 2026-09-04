//! Ordered startup recovery sweeps.
//!
//! Lean requires inference-call recovery at startup and periodically
//! (`Proofs/Recovery/Sweeps/Inference.lean`), and its implementation
//! (`InferenceCall::recover_all`) is parent-gated: a queued/running call whose
//! linked request is not interrupted or terminal is skipped, because a live
//! parent may still own the call. That gate makes the sweep's convergence
//! depend on ordering: request repair must run first (issue #1001;
//! `Proofs/Recovery/StartupOrder.lean`). A still-live execution lease defers
//! both repairs; the same order on later periodic ticks converges the pair
//! after lease expiry (`Recovery.deferred_startup_then_expired_periodic_converges`).
//!
//! Keeping the ordered sequence in one place makes it drivable by conformance
//! tests instead of burying the order as ad hoc calls in the startup observer,
//! mirroring `periodic_recovery`.

use defra_node::EmbeddedNode;

use crate::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::lifecycle::{RecoveryReport, RequestLifecycle};
use crate::tool_call_lifecycle::{ToolCallLifecycle, ToolCallRecoveryReport};

/// Per-sweep results of one ordered startup recovery pass. Each sweep runs
/// even when an earlier one failed — startup recovery is best-effort per
/// collection and retried on the next startup.
#[derive(Debug)]
pub struct StartupRecoveryOutcome {
    pub tool_calls: anyhow::Result<ToolCallRecoveryReport>,
    pub requests: anyhow::Result<RecoveryReport>,
    pub inference_calls: anyhow::Result<InferenceCallRecoveryReport>,
}

/// Run the startup recovery sweeps in dependency order:
///
/// 1. **Tool calls** — the restart-disposition classifier (#937) must observe
///    parent liveness as persisted at the crash, before request repair
///    terminalizes those parents.
/// 2. **Requests/responses/conversations** — terminalizes crash-stuck
///    `claimed`/`processing` requests whose execution leases have expired.
///    Still-live leases are preserved for a later periodic recovery pass.
/// 3. **Inference calls** — parent-gated; runs last so crash-orphaned
///    queued/running rows observe terminal parents and are terminalized in
///    this same pass, or the later periodic pass that repairs a deferred parent
///    (`Recovery.request_before_inference_converges`).
pub async fn run_startup_recovery(node: &EmbeddedNode, agent_did: &str) -> StartupRecoveryOutcome {
    let tool_calls = ToolCallLifecycle::recover_all(node, agent_did).await;
    let requests = RequestLifecycle::recover_all(node, agent_did).await;
    let inference_calls = InferenceCall::recover_all(node, agent_did).await;
    StartupRecoveryOutcome {
        tool_calls,
        requests,
        inference_calls,
    }
}
