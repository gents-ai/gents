use super::*;

#[test]
fn validate_graphql_filter_fragment_accepts_real_filters() {
    for filter in [
        r#"{ kind: { _eq: "signup" } }"#,
        r#"{ status: { _in: ["a", "b"] } }"#,
        r#"{ _and: [ { a: { _eq: 1 } }, { b: { _gt: -2.5 } } ] }"#,
        r#"{ name: { _like: "%needs \"quotes\"%" } }"#,
        "{ enabled: { _eq: true } }",
    ] {
        assert!(
            validate_graphql_filter_fragment(filter).is_ok(),
            "{filter:?} is a legitimate filter and must pass"
        );
    }
}

#[test]
fn validate_graphql_filter_fragment_rejects_break_outs() {
    for filter in [
        r#"{} ] }, limit: 1) { _docID } Agent(filter: { _and: [ {} ] }, limit: 1) { system_prompt } PocEvent(filter: { _and: [ {}"#,
        "{ a: 1 } }",
        "{ a: 1 ]",
        "{ a: 1 }) { x } (",
        "{ a: 1 } { b: 2 }",
        "{ a: 1 } # trailing",
        "kind",
        "",
        "   ",
        r#"{ a: { _eq: "open } }"#,
        r#"{"instructions": {"_eq": "b"}}"#,
    ] {
        assert!(
            validate_graphql_filter_fragment(filter).is_err(),
            "{filter:?} must be rejected"
        );
    }
}

#[test]
fn graphql_input_literal_rejects_object_keys_that_are_not_graphql_names() {
    let hostile = serde_json::json!({
        "endpoint": {
            r#"x: 1 }, api_key: "leaked" }) { _docID } #"#: 1
        }
    });
    let err = graphql_input_literal(&hostile)
        .expect_err("a non-Name object key must be rejected, not spliced");
    assert!(
        err.to_string().contains("identifier"),
        "error should name the identifier rule: {err}"
    );

    let ok = serde_json::json!({ "outer": { "inner_1": "v" } });
    assert!(graphql_input_literal(&ok)
        .expect("valid Name keys render")
        .contains("inner_1: \"v\""));
}

#[test]
fn graphql_input_literal_emits_null_for_empty_array_not_bracket_literal() {
    assert_eq!(
        graphql_input_literal(&serde_json::json!([])).expect("render literal"),
        "null",
    );
    let value = serde_json::json!({
        "context_id": "c",
        "skill_ids": [],
        "target_ids": [],
    });
    let rendered = graphql_input_literal(&value).expect("render literal");
    assert!(
        rendered.contains("target_ids: null"),
        "rendered: {rendered}"
    );
    assert!(rendered.contains("skill_ids: null"), "rendered: {rendered}");
    assert!(!rendered.contains("target_ids: []"), "rendered: {rendered}");
    assert!(!rendered.contains("skill_ids: []"), "rendered: {rendered}");
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum NameOutcome {
    Accepted,
    Empty,
    BadStart,
    BadCharacter,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CollectionOutcome {
    Accepted,
    Empty,
    BadStart,
    BadCharacter,
    ReservedPrefix,
}

fn name_outcome(name: &str) -> NameOutcome {
    match validate_graphql_name(name) {
        Ok(()) => NameOutcome::Accepted,
        Err(GraphqlError::EmptyIdentifier) => NameOutcome::Empty,
        Err(GraphqlError::IdentifierStart { .. }) => NameOutcome::BadStart,
        Err(GraphqlError::IdentifierCharacter { .. }) => NameOutcome::BadCharacter,
        Err(error) => panic!("validate_graphql_name({name:?}) returned {error}"),
    }
}

fn collection_outcome(name: &str) -> CollectionOutcome {
    match validate_collection_identifier(name) {
        Ok(()) => CollectionOutcome::Accepted,
        Err(GraphqlError::EmptyIdentifier) => CollectionOutcome::Empty,
        Err(GraphqlError::IdentifierStart { .. }) => CollectionOutcome::BadStart,
        Err(GraphqlError::IdentifierCharacter { .. }) => CollectionOutcome::BadCharacter,
        Err(GraphqlError::ReservedCollectionPrefix { .. }) => CollectionOutcome::ReservedPrefix,
        Err(error) => panic!("validate_collection_identifier({name:?}) returned {error}"),
    }
}

#[test]
fn graphql_name_rendering_matches_the_lean_model() {
    gents_lean_vocab::families::graphql_name_names::replay::<NameOutcome>(|input| {
        name_outcome(&input.value)
    });
    gents_lean_vocab::families::graphql_name_collections::replay::<CollectionOutcome>(|input| {
        collection_outcome(&input.value)
    });
    gents_lean_vocab::families::graphql_name_escapes::replay(|input| {
        escape_graphql_string(&input.value)
    });
}
