use super::super::*;

fn response_row(content: &str, progress_seq: i64) -> AgentResponseRow {
    serde_json::from_value(serde_json::json!({
        "response_key": "response-1",
        "request_id": "request-1",
        "agent_did": "did:agent:1",
        "session_id": "session-1",
        "content": content,
        "status": "streaming",
        "progress_seq": progress_seq
    }))
    .expect("response row")
}

#[test]
fn response_patch_preserves_cold_collection_allocations_and_updates_latest_index() {
    let messages = (0..600)
        .map(|sequence| {
            serde_json::from_value(serde_json::json!({
                "message_key": format!("session-1:{sequence}"),
                "session_id": "session-1",
                "sequence": sequence,
                "role": "assistant",
                "content": format!("durable transcript row {sequence}")
            }))
            .expect("message row")
        })
        .collect();
    let mut store = ClientStore::from_rows(ClientStoreRows {
        responses: vec![response_row("a", 1)],
        messages,
        ..ClientStoreRows::default()
    });
    let messages_ptr = store.messages.as_ptr();
    let messages_capacity = store.messages.capacity();

    store.merge_response_patch_in_place(ClientStore::from_rows(ClientStoreRows {
        responses: vec![response_row("ab", 2)],
        ..ClientStoreRows::default()
    }));

    assert_eq!(store.messages.len(), 600);
    assert_eq!(store.messages.as_ptr(), messages_ptr);
    assert_eq!(store.messages.capacity(), messages_capacity);
    assert_eq!(store.responses.len(), 1);
    assert_eq!(
        store
            .latest_response_for_request("request-1")
            .and_then(|row| row.content.as_deref()),
        Some("ab")
    );
}
