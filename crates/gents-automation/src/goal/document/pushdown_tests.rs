use super::GoalDocument;

const UNCAPPED_GOAL_JSON: &str = r#"{
    "_docID": "bafyrei_contract_goal_uncapped",
    "goal_id": "goal-uncapped",
    "session_id": "session-uncapped",
    "node_did": "did:key:zNODE",
    "creation_key": "creation-key",
    "objective": "ship the capability contract",
    "status": "active",
    "tokens_used": 0,
    "continuation_sequence": 0
}"#;

const CAPPED_GOAL_JSON: &str = r#"{
    "_docID": "bafyrei_contract_goal_capped",
    "goal_id": "goal-capped",
    "session_id": "session-capped",
    "node_did": "did:key:zNODE",
    "creation_key": "creation-key",
    "objective": "ship the capability contract",
    "status": "active",
    "token_budget": 1000,
    "tokens_used": 0,
    "continuation_sequence": 0
}"#;

#[test]
fn goal_document_omitted_budget_deserializes_as_none_not_a_sentinel() {
    let uncapped: GoalDocument =
        serde_json::from_str(UNCAPPED_GOAL_JSON).expect("uncapped goal document");
    assert_eq!(
        uncapped.token_budget, None,
        "an omitted token_budget must deserialize as None (uncapped), never as a sentinel number"
    );

    let capped: GoalDocument =
        serde_json::from_str(CAPPED_GOAL_JSON).expect("capped goal document");
    assert_eq!(
        capped.token_budget,
        Some(1000),
        "an explicit cap must round-trip unchanged"
    );

    // Explicit null is the same contract as omission: uncapped.
    let explicit_null: GoalDocument = serde_json::from_str(&UNCAPPED_GOAL_JSON.replace(
        "\"status\": \"active\",",
        "\"status\": \"active\",\n    \"token_budget\": null,",
    ))
    .expect("goal document with explicit null budget");
    assert_eq!(explicit_null.token_budget, None);
}
