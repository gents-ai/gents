use crate::document_config::EventSource;
use crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS;
use serde_json::{json, Value};

fn source(group: Value) -> EventSource {
    serde_json::from_value(json!({
        "agent_did": "did:key:group-validation",
        "event_source_id": "results",
        "source_collection": "ReviewResult",
        "correlation_field": "review_id",
        "group": group,
    }))
    .expect("canonical source")
}

#[test]
fn explicit_invalid_group_numbers_cannot_disappear_behind_fixed_count() {
    for field in ["timeout_secs", "min_count"] {
        for value in [i64::MIN, -1, 0] {
            let mut group = json!({"expected_count": 2, "timeout_secs": 30});
            group[field] = json!(value);
            let error = source(group)
                .validate_group()
                .expect_err("explicit nonpositive value must be rejected");
            assert!(
                error.to_string().contains(field),
                "{field}={value}: {error}"
            );
        }
    }
}

#[test]
fn optional_group_defaults_and_supported_completion_modes_remain_valid() {
    for group in [
        Value::Null,
        json!({"expected_count": 1}),
        json!({"expected_count": {"source_field": "expected_results"}}),
        json!({"timeout_secs": 1}),
        json!({"expected_count": 2, "timeout_secs": 30, "min_count": 1}),
        json!({"expected_count": MAX_EVENT_TRIGGER_GROUP_DOCS,
            "timeout_secs": i64::MAX, "min_count": MAX_EVENT_TRIGGER_GROUP_DOCS}),
    ] {
        source(group.clone())
            .validate_group()
            .unwrap_or_else(|error| panic!("valid group {group}: {error}"));
    }
}

#[test]
fn existing_correlation_cardinality_and_timeout_requirements_are_preserved() {
    let mut missing_correlation = source(json!({"expected_count": 1}));
    missing_correlation.correlation_field = None;
    assert!(missing_correlation.validate_group().is_err());
    for group in [
        json!({}),
        json!({"expected_count": 0}),
        json!({"expected_count": MAX_EVENT_TRIGGER_GROUP_DOCS + 1}),
        json!({"expected_count": {"source_field": "not-a-field"}}),
        json!({"expected_count": 2, "min_count": 1}),
        json!({"expected_count": 2, "timeout_secs": 30, "min_count": 3}),
        json!({"timeout_secs": 30, "min_count": MAX_EVENT_TRIGGER_GROUP_DOCS + 1}),
    ] {
        assert!(
            source(group.clone()).validate_group().is_err(),
            "invalid group admitted: {group}"
        );
    }
}

#[test]
fn source_matching_uses_shared_identifier_filter_and_created_event_guards() {
    let mut valid = source(json!({"expected_count": 2}));
    valid.filter = Some("{ status: { _eq: \"ready\" } }".into());
    valid
        .validate()
        .expect("valid matching source with default event kind");
    valid.event_kind = Some("created".into());
    valid.validate().expect("explicit created event kind");
    for kind in ["", "updated", "deleted", " created "] {
        let mut invalid = valid.clone();
        invalid.event_kind = Some(kind.into());
        assert!(invalid.validate().is_err(), "unsupported kind {kind:?}");
    }
    for collection in ["", "__Schema", "Result) { leaked }"] {
        let mut invalid = valid.clone();
        invalid.source_collection = collection.into();
        assert!(
            invalid.validate().is_err(),
            "invalid collection {collection:?}"
        );
    }
    for filter in ["not an object", "{} } mutation { delete_Result }"] {
        let mut invalid = valid.clone();
        invalid.filter = Some(filter.into());
        assert!(invalid.validate().is_err(), "invalid filter {filter:?}");
    }
    let mut invalid = valid.clone();
    invalid.correlation_field = Some("bad-field".into());
    assert!(invalid.validate().is_err());
    invalid = valid;
    invalid.group.as_mut().expect("group").min_count = Some(-1);
    assert!(
        invalid.validate().is_err(),
        "full source admission must include grouping"
    );
}
