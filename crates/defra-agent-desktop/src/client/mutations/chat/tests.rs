use super::binding::default_behavior_id_for_agent;

#[test]
fn default_behavior_id_uses_agent_did_suffix() {
    assert_eq!(
        default_behavior_id_for_agent("did:defra:test"),
        "did:defra:test:default".to_string()
    );
}
