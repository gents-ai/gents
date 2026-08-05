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
