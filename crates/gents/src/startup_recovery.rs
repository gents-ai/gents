//! Ordered startup recovery sweeps.
//!
//! Lean pins the inference-call recovery sweep at `.startup` cadence
//! (`Proofs/Recovery/Sweeps/Inference.lean`), and its implementation
//! (`InferenceCall::recover_all`) is parent-gated: a queued/running call whose
//! linked request is not interrupted or terminal is skipped, because a live
//! parent may still own the call. That gate makes the sweep's convergence
//! depend on ordering: after a crash the parents of every stale call are
//! exactly the requests stuck in `claimed`/`processing`, so the request sweep
//! must run first (issue #1001; `Proofs/Recovery/StartupOrder.lean`,
//! `Recovery.request_before_inference_converges`).
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
///    `claimed`/`processing` requests from their durable response outcome
///    (creating the missing error response first when the crash predates it).
/// 3. **Inference calls** — parent-gated; runs last so crash-orphaned
///    queued/running rows observe terminal parents and are terminalized in
///    this same pass instead of surviving until the next restart
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
