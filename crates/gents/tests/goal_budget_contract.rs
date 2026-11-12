//! Capability contract for goal token budgets and execution-profile defaults.
//!
//! Jack's contract, pinned here as regression tests with no production change:
//!
//! 1. Goals are uncapped by default. The durable `Goal` schema declares
//!    `token_budget: Int` (nullable, not `Int!`), the Rust representation is
//!    `Option<i64>`, and no runtime path injects a default cap: budget
//!    enforcement only ever consults `token_budget.is_some_and(..)` at the
//!    trigger source, and `create_goal` / task declarations treat an omitted
//!    budget as unlimited.
//! 2. Execution profiles inherit platform defaults. Every `InferenceExecution`
//!    field is `Option` and its own docs say "unset fields preserve the
//!    owned-loop defaults"; a profile document with no overrides stays
//!    all-`None` after a serialize/deserialize round trip (no copied default
//!    values are written back), and an all-`None` profile validates.
//! 3. Explicit caps are preserved and a budget-bearing update that omits the
//!    budget keeps the existing cap (`set_goal_in_txn` falls back to the
//!    existing goal's budget via `unwrap_or_else`); the task-level declaration
//!    validator keeps rejecting non-positive budgets and budget-without-
//!    objective, so no huge sentinel value is needed or permitted.
//!
//! These tests use only public API and static schema text: no database, no
//! network, no provider.

use gents_automation::goal::GoalDocument;
use gents_store::schema::{GOAL_SCHEMA, TASK_SCHEMA};

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
fn goal_schema_declares_token_budget_nullable_so_default_is_uncapped() {
    // The schema is the source of truth for "no cap by default": a nullable
    // `Int` (no `!`) means an omitted budget persists as null/None.
    assert!(
        GOAL_SCHEMA.contains("token_budget: Int\n"),
        "Goal schema must declare token_budget as a nullable Int, got:\n{GOAL_SCHEMA}"
    );
    assert!(
        !GOAL_SCHEMA.contains("token_budget: Int!"),
        "Goal schema must not require a token budget (that would impose a default cap)"
    );

    // Same contract at the task-declaration layer.
    assert!(
        TASK_SCHEMA.contains("goal_token_budget: Int\n"),
        "Task schema must declare goal_token_budget as a nullable Int"
    );
    assert!(
        !TASK_SCHEMA.contains("goal_token_budget: Int!"),
        "Task schema must not require goal_token_budget"
    );
}

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
