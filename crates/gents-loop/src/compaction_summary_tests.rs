use super::*;
use serde_json::Value;

#[test]
fn checkpoint_wire_fallback_matches_executed_model() {
    gents_lean_vocab::families::compaction_checkpoint_fallback_checkpoint::replay(|i| {
        parse_fallback_checkpoint(&i.raw)
            .ok()
            .map(|checkpoint| gents_lean_vocab::family::recode(&checkpoint))
    });
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
