use super::binding::resolve_agent_binding;
use crate::client::store::ClientStore;

#[test]
fn missing_db_behavior_binding_fails_closed() {
    let error = resolve_agent_binding(&ClientStore::default(), "did:test:test", None, None)
        .err()
        .expect("missing behavior authority must fail");
    assert!(error.to_string().contains("has no default_behavior_id"));
}
