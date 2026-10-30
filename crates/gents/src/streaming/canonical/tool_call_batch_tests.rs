use super::*;

#[test]
fn batch_create_rows_bind_to_tool_keys_not_response_order() {
    let response = serde_json::json!({
        "data": {
            "create_AgentToolCall": [
                {"_docID": "doc-b", "tool_call_key": "key-b"},
                {"_docID": "doc-a", "tool_call_key": "key-a"}
            ]
        }
    });
    let keys = vec!["key-a".to_owned(), "key-b".to_owned()];

    assert_eq!(
        created_tool_call_doc_ids(&response, &keys).unwrap(),
        vec!["doc-a".to_owned(), "doc-b".to_owned()]
    );
}

#[test]
fn batch_create_rejects_missing_or_duplicate_tool_keys() {
    let keys = vec!["key-a".to_owned(), "key-b".to_owned()];
    let missing = serde_json::json!({
        "data": {
            "create_AgentToolCall": [
                {"_docID": "doc-a", "tool_call_key": "key-a"}
            ]
        }
    });
    assert!(created_tool_call_doc_ids(&missing, &keys).is_err());

    let duplicate = serde_json::json!({
        "data": {
            "create_AgentToolCall": [
                {"_docID": "doc-a", "tool_call_key": "key-a"},
                {"_docID": "doc-b", "tool_call_key": "key-a"}
            ]
        }
    });
    assert!(created_tool_call_doc_ids(&duplicate, &keys).is_err());
}
