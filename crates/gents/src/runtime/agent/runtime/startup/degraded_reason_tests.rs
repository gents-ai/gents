use super::snapshot::is_degraded_startup_unavailable_reason;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

#[test]
fn generated_startup_degraded_reasons_cover_every_public_reason() {
    gents_lean_vocab::families::runtime_startup_degraded_allows_startup::replay::<
        AgentReadinessUnavailableReason,
    >(|input| is_degraded_startup_unavailable_reason(input.reason));
}

#[test]
fn generated_node_degrading_reasons_match_the_protocol_owner() {
    gents_lean_vocab::families::runtime_startup_degraded_degrades_node::replay::<
        AgentReadinessUnavailableReason,
    >(|input| input.reason.degrades_node());
}

/// Lean `Runtime.StartupDegraded.toolSurfaceReason` drives the reason the
/// runtime reports for an unbuilt tool surface: a root the node publishes and
/// enables that the Agent cannot read is the missing reader, every other denial
/// the unadmitted surface.
#[test]
fn generated_tool_surface_reasons_match_the_rust_classifier() {
    gents_lean_vocab::families::runtime_startup_degraded_tool_surface_reason::replay::<
        AgentReadinessUnavailableReason,
        gents_tools::surface::RootDenialKind,
        gents_tools::surface::RootPublishing,
    >(|input| gents_tools::surface::tool_surface_reason(input.publishing, input.denial));
}
