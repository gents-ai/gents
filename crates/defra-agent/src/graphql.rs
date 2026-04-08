//! Shared GraphQL utility functions used across agent-daemon modules.

pub fn escape_graphql_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub fn response_has_documents(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => map.contains_key("_docID"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_graphql_string() {
        assert_eq!(escape_graphql_string("hello"), "hello");
        assert_eq!(escape_graphql_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_graphql_string("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_graphql_string("tab\there"), "tab\\there");
        assert_eq!(escape_graphql_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_graphql_string("mixed\"\n\\"), "mixed\\\"\\n\\\\");
    }
}
