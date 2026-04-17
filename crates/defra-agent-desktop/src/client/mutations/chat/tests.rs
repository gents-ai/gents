use super::binding::default_behavior_id_for_agent;
use super::conversation::derive_conversation_title;

#[test]
fn default_behavior_id_uses_agent_did_suffix() {
    assert_eq!(
        default_behavior_id_for_agent("did:defra:test"),
        "did:defra:test:default".to_string()
    );
}

#[test]
fn conversation_title_defaults_for_empty_content() {
    assert_eq!(derive_conversation_title(""), "New Conversation");
}
