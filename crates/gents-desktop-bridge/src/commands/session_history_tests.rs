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

fn active_row(session_id: &str, updated_at: &str) -> SessionSummary {
    let mut session = summary(session_id, Some("default"), None);
    session.updated_at = Some(updated_at.into());
    session.created_at = Some("2026-01-01T00:00:00Z".into());
    session
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

#[test]
fn keyset_cursor_survives_archiving_its_anchor() {
    let mut first_page = request(SessionArchiveVisibility::Active);
    let page = select_page(rows(), first_page.clone()).unwrap();
    assert_eq!(page.sessions[0].session_id, "first");
    first_page.cursor = page.next_cursor;

    let mut changed = rows();
    changed
        .iter_mut()
        .find(|session| session.session_id == "first")
        .unwrap()
        .archived_at = Some("2026-02-01T00:00:00Z".into());
    let next = select_page(changed, first_page).unwrap();
    assert_eq!(next.sessions[0].session_id, "second");
    assert!(next.next_cursor.is_none());
}

#[test]
fn keyset_cursor_does_not_repeat_rows_when_its_anchor_moves_up() {
    let initial = vec![
        active_row("newest", "2026-03-03T00:00:00Z"),
        active_row("anchor", "2026-03-02T00:00:00Z"),
        active_row("older", "2026-03-01T00:00:00Z"),
    ];
    let mut first_request = request(SessionArchiveVisibility::Active);
    first_request.limit = Some(2);
    let first = select_page(initial, first_request.clone()).unwrap();
    assert_eq!(
        first
            .sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        ["newest", "anchor"]
    );
    first_request.cursor = first.next_cursor;

    // The anchor's new activity moves it before the cursor's saved key.
    let changed = vec![
        active_row("anchor", "2026-03-04T00:00:00Z"),
        active_row("newest", "2026-03-03T00:00:00Z"),
        active_row("older", "2026-03-01T00:00:00Z"),
    ];
    let next = select_page(changed, first_request).unwrap();
    assert_eq!(next.sessions[0].session_id, "older");
    assert!(next.next_cursor.is_none());
}

#[test]
fn a_page_two_row_moved_ahead_of_the_boundary_appears_after_refresh() {
    let initial = vec![
        active_row("first", "2026-03-03T00:00:00Z"),
        active_row("later", "2026-03-02T00:00:00Z"),
        active_row("oldest", "2026-03-01T00:00:00Z"),
    ];
    let first = select_page(initial, request(SessionArchiveVisibility::Active)).unwrap();
    assert_eq!(first.sessions[0].session_id, "first");

    // Live history keeps the current projection order. A row that gains
    // activity and moves ahead of the saved boundary is found by refreshing.
    let changed = vec![
        active_row("later", "2026-03-04T00:00:00Z"),
        active_row("first", "2026-03-03T00:00:00Z"),
        active_row("oldest", "2026-03-01T00:00:00Z"),
    ];
    let mut next_request = request(SessionArchiveVisibility::Active);
    next_request.cursor = first.next_cursor;
    let next = select_page(changed.clone(), next_request).unwrap();
    assert_eq!(next.sessions[0].session_id, "oldest");

    let refreshed = select_page(changed, request(SessionArchiveVisibility::Active)).unwrap();
    assert_eq!(refreshed.sessions[0].session_id, "later");
}
