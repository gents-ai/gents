use super::support::*;
use super::*;

#[tokio::test]
async fn resolve_agent_rejects_missing_or_blank_explicit_selection() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_agent_id(&node, "session-bound", "did:test:test", "code")
        .await
        .unwrap();
    for session in ["session-unbound", "session-bound"] {
        for agent_config in [None, Some(""), Some("   ")] {
            match resolve_agent_for_request(node.as_ref(), &request(agent_config, session)).await {
                Err(_) => {}
                Ok(resolved) => assert!(
                    resolved.rejection_reason.is_some(),
                    "missing/blank selection must not inherit default or session agent_config: {session}, {agent_config:?}"
                ),
            }
        }
    }
}

#[tokio::test]
async fn explicit_agent_resolution_matches_lean_binding_cases() {
    for name in [
        "default-A-explicit-B-binds-B",
        "existing-A-requested-B-rejected",
        "existing-A-requested-A-accepted",
    ] {
        let case = crate::lean_vocab_test::lean_runtime_reconcile_case(name);
        let node = test_node().await;
        ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let selected = case
            .requested_agent
            .expect("explicit fixture selection")
            .to_string();
        let _default = case.pre_default_agent.to_string();
        if let Some(bound) = case.pre_session_agent {
            crate::session::create_session_with_agent_id(
                &node,
                "session-binding",
                "did:test:test",
                &bound.to_string(),
            )
            .await
            .unwrap();
        }
        let resolved =
            resolve_agent_for_request(node.as_ref(), &request(Some(&selected), "session-binding"))
                .await
                .unwrap();
        assert_eq!(resolved.agent_id, selected, "{name}");
        assert_eq!(resolved.rejection_reason.is_none(), case.legal, "{name}");
    }
}
