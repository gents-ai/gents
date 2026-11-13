use super::snapshot::is_degraded_startup_unavailable_reason;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

#[test]
fn generated_startup_degraded_reasons_cover_every_public_reason() {
    gents_lean_vocab::families::runtime_startup_degraded_allows_startup::replay::<
        AgentReadinessUnavailableReason,
    >(|input| is_degraded_startup_unavailable_reason(input.reason));
}
