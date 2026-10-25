use super::*;

#[test]
fn generated_title_text_cases_reach_history_and_transcript_without_rewriting_user_titles() {
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    for case in contract["title_text_cases"]["cases"].as_array().unwrap() {
        let session = AgentSession {
            session_id: "session".into(),
            node_did: "owner".into(),
            requester_did: None,
            agent_id: "agent".into(),
            created_at: "2026-10-18T00:00:00Z".into(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: Some(SessionTitle {
                text: case["raw"].as_str().unwrap().to_owned(),
                source: if case["generated_source"].as_bool().unwrap() {
                    SessionTitleSource::Generated
                } else {
                    SessionTitleSource::User
                },
            }),
            tags: Vec::new(),
            provenance: None,
            observation: None,
        };
        let before = session.clone();
        let summaries = session_summaries(std::slice::from_ref(&session), &[], "owner", &[], &[]);
        let expected = case["expected_display"].as_str().unwrap();
        assert_eq!(
            summaries[0].title.as_deref(),
            Some(expected),
            "{}",
            case["name"]
        );
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session],
            ..Default::default()
        });
        let snapshot = build_session_snapshot_from_store(&store, "session", None).unwrap();
        assert_eq!(
            snapshot.title,
            crate::types::normalize_optional(Some(expected)),
            "{}",
            case["name"]
        );
        assert_eq!(store.sessions[0], before);
    }
}
