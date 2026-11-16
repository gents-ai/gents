use super::*;

#[test]
fn pending_request_satisfaction_matches_lean() {
    gents_lean_vocab::families::enrollment_pending_requests_satisfaction::replay(|case| {
        let authority =
            |value: &gents_lean_vocab::families::enrollment_pending_requests_satisfaction::RequestAuthority| {
                EnrollmentRequestAuthority {
                    network_id: value.network_id.clone(),
                    admin_did: value.admin_did.clone(),
                    server_peer: value.server_peer.clone(),
                    candidate_did: value.candidate_did.clone(),
                    candidate_peer: value.candidate_peer.clone(),
                    owner_node: value.owner_node.clone(),
                    profile: value.profile.clone(),
                }
            };
        gents_lean_vocab::families::enrollment_pending_requests_satisfaction::SatisfactionOutput {
            satisfied: pending_request_satisfied(
                &authority(&case.requested),
                &authority(&case.current),
                case.has_decision,
                case.current_approval,
                case.route_applied,
            ),
        }
    });
}

#[test]
fn active_request_visibility_matches_lean() {
    gents_lean_vocab::families::enrollment_pending_requests_visibility::replay(|case| {
        gents_lean_vocab::families::enrollment_pending_requests_visibility::VisibilityOutput {
            visible: active_request_visible(
                case.request_fresh,
                case.has_decision,
                case.current_approval,
                case.route_applied,
            ),
        }
    });
}
