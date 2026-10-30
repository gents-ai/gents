use super::is_degraded_startup_unavailable_reason;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;

#[test]
fn startup_unavailability_classification_is_complete() {
    for reason in [
        Reason::AgentDisabled,
        Reason::BackendNotConfigured,
        Reason::BackendDisabled,
        Reason::BackendTemporarilyUnavailable,
        Reason::CredentialsRequired,
    ] {
        assert!(
            is_degraded_startup_unavailable_reason(reason),
            "{reason:?} must allow degraded startup"
        );
    }
    assert!(!is_degraded_startup_unavailable_reason(
        Reason::ToolConfigurationInvalid
    ));
}
