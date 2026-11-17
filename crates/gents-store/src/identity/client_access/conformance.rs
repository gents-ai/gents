use gents_protocol::client_access::{accepts_request, ClientAccessDecision, ClientAccessState};

#[test]
fn signed_runtime_requests_and_decisions_follow_the_model() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::client_delegation_access_request::cases() {
        let input = &case.input;
        case.check(accepts_request(
            input.caller_matches,
            input.node_matches,
            input.signature_verified,
            input.retained_exists,
            input.retained_same,
            input.fresh,
        ));
    }
    for case in gents_lean_vocab::families::client_delegation_access_decision::cases() {
        let input = &case.input;
        let before: ClientAccessState = serde_json::from_value(serde_json::json!(input.before))?;
        let decision: ClientAccessDecision =
            serde_json::from_value(serde_json::json!(input.decision))?;
        let settled = before.settle(decision, input.native_authority, input.effects_complete);
        case.check(
            serde_json::to_value(settled)?
                .as_str()
                .expect("state token")
                .to_owned(),
        );
    }
    Ok(())
}
