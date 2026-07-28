use super::*;

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
