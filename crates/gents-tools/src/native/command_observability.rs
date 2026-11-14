use std::sync::atomic::{AtomicU64, Ordering};

static POLICY_REJECTIONS: AtomicU64 = AtomicU64::new(0);
static CREDENTIAL_INSPECTION_FAILURES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct CommandPolicyMetrics {
    pub policy_rejections: u64,
    pub credential_inspection_failures: u64,
}

pub fn snapshot() -> CommandPolicyMetrics {
    CommandPolicyMetrics {
        policy_rejections: POLICY_REJECTIONS.load(Ordering::Relaxed),
        credential_inspection_failures: CREDENTIAL_INSPECTION_FAILURES.load(Ordering::Relaxed),
    }
}

pub(super) fn policy_denied() {
    POLICY_REJECTIONS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
pub(super) fn credential_inspection_failed(node_did: Option<&str>) {
    record_credential_failure(&CREDENTIAL_INSPECTION_FAILURES, node_did);
}

#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
pub fn record_credential_failure(counter: &AtomicU64, node_did: Option<&str>) {
    let count = counter.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    if count.is_power_of_two() {
        tracing::error!(
            reason = "credential_inspection_failed",
            node_did = node_did.unwrap_or("unknown"),
            failure_count = count,
            "Bounded command credential inspection failed"
        );
    }
}

#[cfg(test)]
mod pushdown_tests;
