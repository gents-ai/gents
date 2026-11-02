use super::*;

#[tokio::test]
async fn sessions_marks_only_the_callers_own_session_in_concurrent_tool_scopes() {
    use crate::store::config_client::ConfigAccess;
    use crate::tools::call::lifecycle::runtime::{
        scope_request_tool_execution_with_session, scope_tool_request_identity,
    };

    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let other = KeyIdentity::load_or_create(identities.path().join("other.key"), None).unwrap();
    for (identity, session_id, request_id) in [
        (&owner, "session-a", "latest-request-a"),
        (&owner, "session-b", "latest-request-b"),
        (&other, "session-a", "foreign-latest-request"),
    ] {
        let mutation = format!(
            r#"mutation {{ create_AgentSession(input: {{
                    node_did: "{}", requester_did: "{}", session_id: "{}",
                    agent_id: "shared-agent", created_at: "2026-09-01T00:00:00Z"
                }}) {{ _docID }} }}"#,
            escape_graphql_string(identity.did()),
            escape_graphql_string(identity.did()),
            escape_graphql_string(session_id),
        );
        ConfigAccess::write_local(&node, "test.sessions.current.session", &mutation)
            .await
            .unwrap();
        let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            "shared-agent",
            session_id,
            "sessions current caller fixture",
            "interactive",
            "2026-09-01T00:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                identity.did(),
            ),
        );
        crate::sign_agent_request_create(identity, &mut request)
            .await
            .unwrap();
        ConfigAccess::write_local(
            &node,
            "test.sessions.current.request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    }
    let tool = SessionHistoryTool::new(node, owner.did());
    let list = || async {
        let output = Tool::call(
            &tool,
            SessionHistoryParams {
                action: Some("list".into()),
                limit: Some(10),
                session_id: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let snapshot: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(snapshot["scope"]["node_did"], owner.did());
        snapshot["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["is_current"] == true)
            .map(|row| row["session_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let scoped_list = |node_did: String, session_id: &str| {
        scope_tool_request_identity(
            Some(node_did.clone()),
            Some(node_did),
            Some("shared-agent".into()),
            Some("caller-request-not-the-latest-request".into()),
            scope_request_tool_execution_with_session(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                Some(session_id.to_owned()),
                list(),
            ),
        )
    };
    let (a, b) = tokio::join!(
        scoped_list(owner.did().to_owned(), "session-a"),
        scoped_list(owner.did().to_owned(), "session-b"),
    );
    assert_eq!(a, vec!["session-a"]);
    assert_eq!(b, vec!["session-b"]);
    let unauthorized = scope_tool_request_identity(
        Some(other.did().to_owned()),
        Some(other.did().to_owned()),
        None,
        None,
        scope_request_tool_execution_with_session(
            None,
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            Some("session-a".to_owned()),
            Tool::call(&tool, SessionHistoryParams::default()),
        ),
    )
    .await
    .unwrap_err();
    assert!(unauthorized.to_string().contains("running node"));
    assert!(list().await.is_empty());
}

#[tokio::test]
async fn session_history_snapshot_reports_recent_agent_sessions() {
    let node = seeded_node().await;
    let tool = SessionHistoryTool::new(
        node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
    );

    let snapshot = load_session_history_snapshot(&tool.node, &tool.node_did, Some(2))
        .await
        .unwrap();

    assert_eq!(
        snapshot.node_did,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"
    );
    assert_eq!(snapshot.limit, 2);
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["session-b", "session-a"]
    );

    let session_a = snapshot
        .sessions
        .iter()
        .find(|session| session.session_id == "session-a")
        .unwrap();
    assert_eq!(
        session_a.title.as_ref().map(|title| title.text.as_str()),
        Some("OpenAI Agent")
    );
    assert_eq!(session_a.tags, vec!["review"]);
    assert_eq!(session_a.agent_id.as_deref(), Some("agent-a"));
    assert_eq!(session_a.closed_at, None);
    assert_eq!(
        session_a.created_at.as_deref(),
        Some("2026-06-03T09:55:00Z")
    );
    assert_eq!(
        session_a.latest_request_id.as_deref(),
        Some("request-a-new")
    );
    assert_eq!(
        session_a.latest_request_lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(session_a.request_count, 2);
    assert_eq!(session_a.message_count, 2);
    assert_eq!(
        session_a.latest_message_at.as_deref(),
        Some("2026-06-03T10:06:00Z")
    );
    assert_eq!(session_a.compaction_count, 1);
    assert_eq!(
        session_a.last_compacted_at.as_deref(),
        Some("2026-06-03T10:07:00Z")
    );
    let error = Tool::call(
        &tool,
        SessionHistoryParams {
            action: Some("read".to_string()),
            limit: None,
            session_id: None,
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("unsupported sessions action"));
}
