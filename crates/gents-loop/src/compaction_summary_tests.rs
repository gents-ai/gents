use super::*;
use serde_json::Value;

#[test]
fn checkpoint_wire_fallback_matches_executed_model() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["checkpoint_fallback_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let parsed = parse_fallback_checkpoint(case["raw"].as_str().unwrap());
        if case["expected"].is_null() {
            assert!(parsed.is_err(), "{} accepted invalid input", case["name"]);
        } else {
            let checkpoint = parsed.unwrap_or_else(|error| panic!("{}: {error}", case["name"]));
            assert_eq!(
                serde_json::to_value(checkpoint).unwrap(),
                case["expected"],
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn checkpoint_wire_schema_owns_the_list_field_inventory() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let schema = schemars::schema_for!(ContinuationCheckpoint);
    let lists = schema.as_value()["properties"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, value)| value["type"] == "array" && value["items"]["type"] == "string")
        .map(|(name, _)| name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let modeled = contract["checkpoint_list_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(lists, modeled);
    assert_eq!(schema.as_value()["additionalProperties"], false);
}

#[test]
fn checkpoint_wire_fallback_prompt_carries_the_canonical_schema() {
    let prompt = compaction_json_fallback_prompt();
    assert!(prompt.contains(
        &schemars::schema_for!(ContinuationCheckpoint)
            .as_value()
            .to_string()
    ));
    assert!(!compaction_prompt().contains("supplied structured-output schema"));
    assert!(
        serde_json::from_str::<ContinuationCheckpoint>(
            r#"{"goal":"continue","current_work":"scalar","next_actions":["check"]}"#
        )
        .is_err(),
        "guided decoding must still require canonical list types"
    );
}
