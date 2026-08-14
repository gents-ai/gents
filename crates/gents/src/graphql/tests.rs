use super::*;

#[test]
fn validate_graphql_name_accepts_conforming_names() {
    for name in [
        "AgentResponse",
        "_docID",
        "_eq",
        "snake_case_2",
        "a",
        "_",
        "WebhookEvent",
    ] {
        assert!(
            validate_graphql_name(name).is_ok(),
            "{name:?} conforms to the GraphQL Name grammar and must pass"
        );
    }
}

#[test]
fn validate_graphql_name_rejects_nonconforming_names() {
    for name in [
        "",
        "1abc",
        "Msg (limit: 1) { _docID }",
        "a b",
        "a-b",
        "a.b",
        "Msg{",
        "a\"b",
        "a\nb",
        "naïve",
        "名前",
        "a\u{200b}b",
        " AgentResponse",
        "AgentResponse ",
    ] {
        assert!(
            validate_graphql_name(name).is_err(),
            "{name:?} violates the GraphQL Name grammar and must be rejected"
        );
    }
}

#[test]
fn validate_collection_identifier_rejects_introspection_reserved_names() {
    for name in ["__Type", "__schema", "__typename", "__"] {
        assert!(
            validate_collection_identifier(name).is_err(),
            "{name:?} is GraphQL-reserved (__ prefix) and must be rejected as a collection"
        );
    }
    assert!(validate_collection_identifier("AgentResponse").is_ok());
    assert!(
        validate_collection_identifier("_Private").is_ok(),
        "a single leading underscore is a legal Name and not introspection-reserved"
    );
    assert!(validate_collection_identifier("Msg) { x } (").is_err());
}

#[test]
fn test_escape_graphql_string() {
    assert_eq!(escape_graphql_string("hello"), "hello");
    assert_eq!(escape_graphql_string("he\"llo"), "he\\\"llo");
    assert_eq!(escape_graphql_string("line1\nline2"), "line1\\nline2");
    assert_eq!(escape_graphql_string("cr\r"), "cr\\r");
    assert_eq!(escape_graphql_string("tab\there"), "tab\\there");
    assert_eq!(escape_graphql_string("back\\slash"), "back\\\\slash");
    assert_eq!(escape_graphql_string("mixed\"\n\\"), "mixed\\\"\\n\\\\");
}

#[test]
fn single_mutation_document_normalizes_create_to_add_response_key() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{ "_docID": "doc-1" }]
    }));

    let document = single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .expect("normalized mutation document");
    assert_eq!(document["_docID"], "doc-1");
}

#[test]
fn single_mutation_document_rejects_duplicate_normalized_keys() {
    let response = QueryResponse::success(serde_json::json!({
        "create_AgentRequest": { "_docID": "doc-create" },
        "add_AgentRequest": [{ "_docID": "doc-add" }]
    }));

    let error = single_mutation_document(&response, "create_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("returned both"));
}

#[test]
fn mutation_composite_version_rejects_concurrent_newest_heads() {
    let response = QueryResponse::success(serde_json::json!({
        "update_AgentRequest": [{
            "_version": [
                { "cid": "bafy-head-a", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-head-b", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-parent", "height": 6, "fieldName": "_C" }
            ]
        }]
    }));

    let error = mutation_composite_version(&response, "update_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("document version is ambiguous"));
}

#[test]
fn document_composite_version_sorts_and_never_falls_back_to_field_commits() {
    let document = serde_json::json!({
        "_version": [
            { "cid": "bafy-old", "height": 2, "fieldName": "_C" },
            { "cid": "bafy-field", "height": 9, "fieldName": "content" },
            { "cid": "bafy-new", "height": 4, "fieldName": "_C" }
        ]
    });
    let commit = document_composite_version(&document, "AgentMessage boundary")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");

    let field_only = serde_json::json!({
        "_version": [{ "cid": "bafy-field", "height": 9, "fieldName": "content" }]
    });
    assert!(
        document_composite_version(&field_only, "AgentMessage boundary")
            .unwrap()
            .is_none()
    );
}

#[test]
fn mutation_composite_version_accepts_unique_newest_composite() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{
            "_version": [
                { "cid": "bafy-field", "height": 8, "fieldName": "status" },
                { "cid": "bafy-new", "height": 8, "fieldName": "_C" },
                { "cid": "bafy-old", "height": 7, "fieldName": "_C" }
            ]
        }]
    }));

    let commit = mutation_composite_version(&response, "create_AgentRequest")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");
}
