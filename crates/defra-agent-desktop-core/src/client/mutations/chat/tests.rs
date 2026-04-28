use super::binding::default_behavior_id_for_agent;

#[test]
fn default_behavior_id_is_human_keyed() {
    assert_eq!(default_behavior_id_for_agent("did:defra:test"), "default");
}
