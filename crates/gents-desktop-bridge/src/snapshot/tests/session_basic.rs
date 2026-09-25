use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

fn session(id: &str, agent: &str, latest: Option<(&str, RequestLifecycleState)>) -> AgentSession {
    AgentSession {
        session_id: id.to_string(),
        agent_did: agent.to_string(),
        requester_did: None,
        behavior_id: "default".to_string(),
        created_at: "2026-04-21T12:00:00Z".to_string(),
        closed_at: None,
        title: Some(SessionTitle {
            text: format!("{agent} run"),
            source: SessionTitleSource::User,
        }),
        tags: Vec::new(),
        provenance: None,
        observation: latest.map(|(request_id, lifecycle_state)| SessionObservation {
            last_activity_at: "2026-04-21T12:01:00Z".to_string(),
            preview: Some("follow up question".to_string()),
            latest_request: Some(SessionRequestObservation {
                request_doc_id: request_id.to_string(),
                request_id: request_id.to_string(),
                lifecycle_state,
            }),
        }),
    }
}

fn request(id: &str, session_id: &str, state: RequestLifecycleState) -> AgentRequestRow {
    AgentRequestRow {
        doc_id: Some(id.to_string()),
        request_id: id.to_string(),
        agent_did: Some("did:test:amy".to_string()),
        behavior_id: Some("default".to_string()),
        session_id: Some(session_id.to_string()),
        content: Some("follow up question".to_string()),
        lifecycle_state: Some(state),
        execution_origin: Some("interactive".to_string()),
        created_at: Some("2026-04-21T12:00:00Z".to_string()),
        ..Default::default()
    }
}

#[test]
fn session_snapshot_is_agent_scoped_when_session_ids_match() {
    let mut rows = ClientStoreRows {
        sessions: vec![
            session("shared-session", "did:test:mini-1", None),
            session("shared-session", "did:test:mini-2", None),
        ],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message_for_agent(
        &mut rows,
        "msg-mini-1",
        "shared-session",
        None,
        1,
        MessageRole::User,
        "mini 1 only",
        "did:test:mini-1",
        None,
    );
    push_canonical_text_message_for_agent(
        &mut rows,
        "msg-mini-2",
        "shared-session",
        None,
        2,
        MessageRole::User,
        "mini 2 only",
        "did:test:mini-2",
        None,
    );
    let snapshot = build_session_snapshot_from_store_for_agent(
        &ClientStore::from_rows(rows),
        Some("did:test:mini-1"),
        "shared-session",
        None,
    )
    .expect("session snapshot");
    assert_eq!(snapshot.agent_did.as_deref(), Some("did:test:mini-1"));
    assert_eq!(snapshot.title.as_deref(), Some("did:test:mini-1 run"));
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_key, "msg-mini-1");
}

#[test]
fn session_snapshot_exposes_provider_context_pressure_and_compaction_history() {
    let behavior = serde_json::from_value(serde_json::json!({"behavior_id":"default","agent_did":"did:test:amy","inference_profile_id":"large-context","context_id":"large-context-input"})).expect("behavior");
    let profile = serde_json::from_value(serde_json::json!({"agent_did":"did:test:amy","profile_id":"large-context","backend_id":"backend","model_name":"model","context_window":10000})).expect("profile");
    let context = serde_json::from_value(serde_json::json!({"agent_did":"did:test:amy","context_id":"large-context-input","compaction_id":"compact"})).expect("context");
    let compaction = serde_json::from_value(serde_json::json!({"agent_did":"did:test:amy","compaction_id":"compact","strategy":"StripThenSummarize","threshold":0.57})).expect("compaction");
    let entry = serde_json::from_value(serde_json::json!({"compaction_key":"session-context:1","session_id":"session-context","sequence":1,"summary":"first turn","messages_compacted":1,"compacted_through_sequence":1,"original_tokens":1000,"compacted_tokens":200,"created_at":"2026-08-24T12:00:00Z"})).expect("compaction entry");
    let mut rows = ClientStoreRows {
        sessions: vec![session("session-context", "did:test:amy", None)],
        requests: vec![request(
            "context-request",
            "session-context",
            RequestLifecycleState::Completed,
        )],
        behaviors: vec![behavior],
        inference_profiles: vec![profile],
        contexts: vec![context],
        compactions: vec![compaction],
        compaction_entries: vec![entry],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "context-1",
        "session-context",
        Some("context-request"),
        1,
        MessageRole::User,
        &"old turn ".repeat(300),
    );
    push_canonical_text_message(
        &mut rows,
        "context-2",
        "session-context",
        Some("context-request"),
        2,
        MessageRole::User,
        "retained turn",
    );
    push_canonical_text_message(
        &mut rows,
        "context-3",
        "session-context",
        Some("context-request"),
        3,
        MessageRole::User,
        "latest turn",
    );
    let snapshot = build_session_snapshot_from_store_for_agent(
        &ClientStore::from_rows(rows),
        Some("did:test:amy"),
        "session-context",
        None,
    )
    .expect("snapshot");
    assert_eq!(snapshot.context.context_window, 10_000);
    assert_eq!(snapshot.context.compaction_threshold_tokens, 5_700);
    assert_eq!(snapshot.context.durable_message_count, 3);
    assert_eq!(snapshot.context.provider_message_count, 2);
    assert_eq!(snapshot.context.total_compacted_messages, 1);
    assert!(
        snapshot.context.estimated_durable_tokens > snapshot.context.estimated_conversation_tokens
    );
}

#[test]
fn session_context_window_follows_the_runtime_resolution_of_the_profile() {
    let behavior = serde_json::json!({"behavior_id":"default","agent_did":"did:test:amy","inference_profile_id":"profile"});
    let backend = serde_json::json!({"agent_did":"did:test:amy","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost/v1","auth":{"kind":"unauthenticated"}});
    let observation = serde_json::json!({"backend_id":"backend","catalogs":[{"agent_did":null,"observed_at":"now","models":[{"model_name":"model","context_window":272000,"max_context_window":872000}]}]});
    let snapshot_for = |context_window: Option<i64>| {
        let mut profile = serde_json::json!({"agent_did":"did:test:amy","profile_id":"profile","backend_id":"backend","model_name":"model"});
        if let Some(value) = context_window {
            profile["context_window"] = value.into();
        }
        let rows = ClientStoreRows {
            sessions: vec![session("session-window", "did:test:amy", None)],
            behaviors: vec![serde_json::from_value(behavior.clone()).expect("behavior")],
            inference_backends: vec![serde_json::from_value(backend.clone()).expect("backend")],
            backend_observations: vec![serde_json::from_value(observation.clone()).expect("observation")],
            backend_observation_source_agent_dids: vec![Some("did:test:amy".into())],
            inference_profiles: vec![serde_json::from_value(profile).expect("profile")],
            ..ClientStoreRows::default()
        };
        build_session_snapshot_from_store_for_agent(
            &ClientStore::from_rows(rows),
            Some("did:test:amy"),
            "session-window",
            None,
        )
        .expect("snapshot")
        .context
        .context_window
    };
    // An unset profile limit is the advertised model's, as the runtime runs it.
    assert_eq!(snapshot_for(None), 272_000);
    // An edited profile limit is the next request's window.
    assert_eq!(snapshot_for(Some(500_000)), 500_000);
}

#[test]
fn session_snapshot_exposes_pending_turn_when_latest_request_is_not_materialized() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(
            "session-1",
            "did:test:amy",
            Some(("req-2", RequestLifecycleState::Processing)),
        )],
        requests: vec![
            request("req-1", "session-1", RequestLifecycleState::Completed),
            request("req-2", "session-1", RequestLifecycleState::Processing),
        ],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "msg-1",
        "session-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "first question",
    );
    push_canonical_text_message(
        &mut rows,
        "msg-3",
        "session-1",
        Some("req-2"),
        3,
        MessageRole::Assistant,
        "continued after the partially replicated request",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "session-1", None)
            .expect("snapshot");
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
    assert!(snapshot.timeline_items.iter().any(|item| matches!(item, RenderedTimelineItem::PendingUserTurn { request_id, .. } if request_id == "req-2")));
}

#[test]
fn session_snapshot_hides_pending_turn_once_user_message_is_materialized() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(
            "session-1",
            "did:test:amy",
            Some(("req-2", RequestLifecycleState::Processing)),
        )],
        requests: vec![request(
            "req-2",
            "session-1",
            RequestLifecycleState::Processing,
        )],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "authored:req-2:prompt",
        "session-1",
        Some("req-2"),
        2,
        MessageRole::User,
        "follow up question",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "session-1", None)
            .expect("snapshot");
    assert!(snapshot.pending_turn.is_none());
}

#[test]
fn pending_input_reconciles_only_with_exact_physical_request_owner() {
    for state in [
        RequestLifecycleState::Pending,
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    ] {
        let mut session_row = session("session-1", "did:test:amy", Some(("logical-2", state)));
        session_row
            .observation
            .as_mut()
            .and_then(|observation| observation.latest_request.as_mut())
            .expect("latest request observation")
            .request_doc_id = "physical-2".to_string();
        let mut request_row = request("logical-2", "session-1", state);
        request_row.doc_id = Some("physical-2".to_string());
        let base = ClientStoreRows {
            sessions: vec![session_row],
            requests: vec![request_row],
            ..ClientStoreRows::default()
        };

        let before_publication = build_session_snapshot_from_store(
            &ClientStore::from_rows(base.clone()),
            "session-1",
            None,
        )
        .expect("snapshot before canonical publication");
        assert!(before_publication.pending_turn.is_some(), "state={state:?}");

        let mut assistant_first = base.clone();
        push_canonical_text_message(
            &mut assistant_first,
            "assistant-2",
            "session-1",
            Some("physical-2"),
            3,
            MessageRole::Assistant,
            "replicated assistant output",
        );
        let assistant_first_snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(assistant_first),
            "session-1",
            None,
        )
        .expect("snapshot with assistant arriving before input header");
        assert!(
            assistant_first_snapshot.pending_turn.is_some(),
            "state={state:?}"
        );

        for (doc_id, message_key, content) in [
            (
                "context-2",
                "authored:physical-2:context",
                "request context",
            ),
            (
                "notification-2",
                "tool-delivery:physical-tool",
                "background completion",
            ),
        ] {
            let mut non_prompt_first = base.clone();
            push_canonical_text_message(
                &mut non_prompt_first,
                doc_id,
                "session-1",
                Some("physical-2"),
                1,
                MessageRole::User,
                content,
            );
            non_prompt_first
                .transcript_messages
                .last_mut()
                .expect("non-prompt header")
                .message
                .message_key = message_key.to_string();
            let snapshot = build_session_snapshot_from_store(
                &ClientStore::from_rows(non_prompt_first),
                "session-1",
                None,
            )
            .expect("snapshot with non-prompt user header arriving first");
            assert!(snapshot.pending_turn.is_some(), "state={state:?}");
        }

        let mut published = base.clone();
        push_canonical_text_message(
            &mut published,
            "user-2",
            "session-1",
            Some("physical-2"),
            2,
            MessageRole::User,
            "follow up question",
        );
        published
            .transcript_messages
            .last_mut()
            .expect("prompt header")
            .message
            .message_key = "authored:physical-2:prompt".to_string();
        let published_snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(published),
            "session-1",
            None,
        )
        .expect("snapshot after canonical input publication");
        assert!(published_snapshot.pending_turn.is_none(), "state={state:?}");

        let mut logical_id_twin = base.clone();
        push_canonical_text_message(
            &mut logical_id_twin,
            "wrong-owner",
            "session-1",
            Some("logical-2"),
            2,
            MessageRole::User,
            "same content cannot own the projection",
        );
        let twin_snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(logical_id_twin),
            "session-1",
            None,
        )
        .expect("snapshot with logical-id twin");
        assert!(twin_snapshot.pending_turn.is_some(), "state={state:?}");
    }
}

#[test]
fn unbound_replica_forks_do_not_swallow_a_later_pending_turn() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(
            "session-1",
            "did:test:amy",
            Some(("req-2", RequestLifecycleState::Processing)),
        )],
        requests: vec![
            request("req-1", "session-1", RequestLifecycleState::Completed),
            request("req-2", "session-1", RequestLifecycleState::Processing),
        ],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "unbound",
        "session-1",
        None,
        1,
        MessageRole::User,
        "same prompt",
    );
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
}
