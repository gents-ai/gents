use super::support::*;
use super::*;

/// Missing selection is malformed even when upstream chooser metadata or an
/// existing session could supply a convenient default. This is an admission
/// boundary test, not a test of the UI's pre-request model chooser.
#[tokio::test]
async fn resolve_behavior_rejects_missing_or_blank_explicit_selection() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        "session-bound",
        "general",
        "did:test:test",
        "code",
    )
    .await
    .unwrap();
    for session in ["session-unbound", "session-bound"] {
        for behavior in [None, Some(""), Some("   ")] {
            match resolve_behavior_for_request(node.as_ref(), &request(behavior, session)).await {
                Err(_) => {}
                Ok(resolved) => assert!(resolved.rejection_reason.is_some(),
                    "missing/blank selection must not inherit default or session behavior: {session}, {behavior:?}"),
            }
        }
    }
}

#[tokio::test]
async fn explicit_behavior_resolution_matches_lean_binding_cases() {
    // Generation alignment/runnability are tested at the router admission
    // boundary. This owner resolves only explicit selection and binding.
    for name in [
        "default-A-explicit-B-binds-B",
        "existing-A-requested-B-rejected",
        "existing-A-requested-A-accepted",
    ] {
        let case = crate::lean_vocab_test::lean_runtime_reconcile_case(name);
        let node = test_node().await;
        ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let selected = case
            .requested_behavior
            .expect("explicit fixture selection")
            .to_string();
        let default = case.pre_default_behavior.to_string();
        if let Some(bound) = case.pre_session_behavior {
            crate::session::create_session_with_behavior_id(
                node.as_ref(),
                "session-binding",
                &default,
                "did:test:test",
                &bound.to_string(),
            )
            .await
            .unwrap();
        }
        let resolved = resolve_behavior_for_request(
            node.as_ref(),
            &request(Some(&selected), "session-binding"),
        )
        .await
        .unwrap();
        assert_eq!(resolved.behavior_id, selected, "{name}");
        assert_eq!(resolved.rejection_reason.is_none(), case.legal, "{name}");
    }
}
