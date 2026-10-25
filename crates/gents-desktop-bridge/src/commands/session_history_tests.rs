use super::*;

fn summary(session_id: &str, agent_id: Option<&str>, turn_state: Option<&str>) -> SessionSummary {
    SessionSummary {
        started_by: None,
        node_did: "did:test:owner".into(),
        requester_did: None,
        latest_request_doc_id: None,
        closed_at: None,
        archived_at: None,
        archive_revision: "0".into(),
        tags: vec![],
        provenance: None,
        session_id: session_id.to_string(),
        title: None,
        preview_text: None,
        status: None,
        agent_id: agent_id.map(str::to_string),
        latest_request_id: None,
        task_id: None,
        task_name: None,
        trigger_id: None,
        trigger_kind: None,
        created_at: None,
        updated_at: None,
        turn_state: turn_state.map(str::to_string),
        message_count: None,
        tool_call_count: None,
        unreadable_reason: None,
        unsendable_reason: None,
    }
}

fn request(visibility: SessionArchiveVisibility) -> SessionHistoryRequest {
    SessionHistoryRequest {
        node_did: "did:test:owner".into(),
        visibility,
        query: None,
        cursor: None,
        limit: Some(1),
    }
}

fn rows() -> Vec<SessionSummary> {
    let mut archived = summary("archived", Some("default"), Some("processing"));
    archived.title = Some("Release notes".into());
    archived.archived_at = Some("2026-01-01T00:00:00Z".into());
    archived.archive_revision = "9007199254740993".into();
    let mut foreign = summary("foreign", Some("default"), None);
    foreign.node_did = "did:test:other-runtime".into();
    let mut first = summary("first", Some("default"), None);
    first.title = Some("Release review".into());
    let mut second = summary("second", Some("default"), None);
    second.preview_text = Some("Release ready".into());
    vec![archived, foreign, first, second]
}

#[test]
fn archive_filter_precedes_pagination_and_preserves_projection_order_and_running_facts() {
    let first = select_page(rows(), request(SessionArchiveVisibility::Active)).unwrap();
    assert_eq!(first.sessions[0].session_id, "first");
    let mut next = request(SessionArchiveVisibility::Active);
    next.cursor = first.next_cursor;
    let second = select_page(rows(), next).unwrap();
    assert_eq!(second.sessions[0].session_id, "second");
    assert!(second.next_cursor.is_none());
    let archived = select_page(rows(), request(SessionArchiveVisibility::Archived)).unwrap();
    assert_eq!(archived.sessions[0].session_id, "archived");
    assert_eq!(
        archived.sessions[0].turn_state.as_deref(),
        Some("processing")
    );
    assert_eq!(archived.sessions[0].archive_revision, "9007199254740993");
    assert!(archived.next_cursor.is_none());
    let all = select_page(rows(), request(SessionArchiveVisibility::All)).unwrap();
    assert_eq!(all.sessions[0].session_id, "archived");
}

#[test]
fn history_search_and_cursors_keep_the_runtime_and_archive_filter() {
    let mut search = request(SessionArchiveVisibility::Active);
    search.query = Some(" RELEASE ".into());
    let first = select_page(rows(), search.clone()).unwrap();
    assert_eq!(first.sessions[0].session_id, "first");
    search.cursor = first.next_cursor;
    assert_eq!(
        select_page(rows(), search.clone()).unwrap().sessions[0].session_id,
        "second"
    );
    search.visibility = SessionArchiveVisibility::Archived;
    assert!(select_page(rows(), search.clone()).is_err());
    search.visibility = SessionArchiveVisibility::Active;
    search.node_did = "did:test:other-runtime".into();
    assert!(select_page(rows(), search.clone()).is_err());
    search.node_did = "did:test:owner".into();
    search.query = Some("different".into());
    assert!(select_page(rows(), search.clone()).is_err());
    search.cursor = None;
    search.limit = Some(101);
    assert!(select_page(rows(), search).is_err());
}
