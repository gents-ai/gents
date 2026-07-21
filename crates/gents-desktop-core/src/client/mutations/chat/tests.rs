use super::binding::default_behavior_id_for_agent;

#[test]
fn default_behavior_id_is_agent_scoped() {
    assert_eq!(
        default_behavior_id_for_agent("did:test:test"),
        "did:test:test:default"
    );
}
