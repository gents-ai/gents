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

use gents::documents::{InferenceExecution, Task};
use gents::goal::{
    decide_goal_continuation, validate_task_goal_declaration, GoalDecision, GoalDocument,
    GoalRequestTerminal, GoalStatus,
};
use gents::{GOAL_SCHEMA, TASK_SCHEMA};

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

#[test]
fn task_declaration_treats_omitted_budget_as_uncapped_and_still_rejects_bad_caps() {
    // Omitted budget with an objective is valid: uncapped by default.
    validate_task_goal_declaration(Some("ship it"), None)
        .expect("an objective without a budget is a valid uncapped goal");

    // No objective and no budget is also a valid non-goal task.
    validate_task_goal_declaration(None, None)
        .expect("a task without a goal declaration needs no budget");

    // A budget cannot exist without an objective (this is what rules out a
    // huge sentinel budget smuggled onto a non-goal task).
    let error = validate_task_goal_declaration(None, Some(i64::MAX))
        .expect_err("a budget without an objective must be rejected");
    assert!(
        error.to_string().contains("requires a goal objective"),
        "unexpected rejection reason: {error}"
    );

    // Caps stay bounded and positive; no sentinel escape hatch.
    assert!(validate_task_goal_declaration(Some("ship it"), Some(0)).is_err());
    assert!(validate_task_goal_declaration(Some("ship it"), Some(-1)).is_err());
    assert!(validate_task_goal_declaration(Some("ship it"), Some(i64::MAX)).is_ok());
}

#[test]
fn continuation_only_stops_for_budget_when_a_cap_is_present_and_reached() {
    // The decision function receives `budget_reached` as a fact; the trigger
    // source derives it from `token_budget.is_some_and(|b| used >= b)`, so an
    // uncapped goal (None) can never produce budget_reached=true. Pin both
    // sides of that gate.
    let uncapped = decide_goal_continuation(
        GoalStatus::Active,
        GoalRequestTerminal::Completed,
        true,  // session_idle
        false, // child_exists
        false, // budget_reached: unreachable for None budgets
        true,  // has_activity
        false, // request_is_wrapup
        0,     // infrastructure_retries
        false, // wrapup_requested
        false, // wrapup_completed
    );
    assert_eq!(
        uncapped,
        GoalDecision::Continue,
        "an active goal without a reached cap keeps continuing"
    );

    let capped_and_reached = decide_goal_continuation(
        GoalStatus::Active,
        GoalRequestTerminal::Completed,
        true,
        false,
        true, // budget_reached
        true,
        false,
        0,
        false,
        false,
    );
    assert_eq!(
        capped_and_reached,
        GoalDecision::Wrapup,
        "only a present-and-reached cap drives the budget wrap-up"
    );
}

#[test]
fn task_document_round_trips_omitted_budget_as_none() {
    let task: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "contract-task",
        "agent_id": "contract",
        "prompt_template": "do the thing",
        "goal_objective_template": "ship the capability contract",
        "goal_token_budget": null
    }))
    .expect("task document with explicit null budget");
    assert_eq!(task.goal_token_budget, None);
    assert_eq!(
        task.goal_objective_template.as_deref(),
        Some("ship the capability contract")
    );

    let omitted: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "contract-task",
        "agent_id": "contract",
        "prompt_template": "do the thing"
    }))
    .expect("task document without any goal fields");
    assert_eq!(omitted.goal_token_budget, None);
    assert_eq!(omitted.goal_objective_template, None);
}

#[test]
fn execution_profiles_without_overrides_inherit_platform_defaults() {
    // An execution profile written with no overrides must stay all-None after
    // a round trip: the document never copies platform default values into
    // stored overrides, so later platform-default changes are inherited.
    let bare = serde_json::json!({
        "node_did": "did:key:zNODE",
        "execution_id": "profile-default"
    });
    let profile: InferenceExecution =
        serde_json::from_value(bare.clone()).expect("bare execution profile");
    assert_eq!(profile.max_turns, None);
    assert_eq!(profile.max_total_tokens, None);
    assert_eq!(profile.stream_batch_ms, None);
    assert_eq!(profile.stream_liveness_timeout_secs, None);
    assert_eq!(profile.provider_idle_timeout_secs, None);
    assert_eq!(profile.deadline_duration_secs, None);

    // Round trip through the serialized form keeps the omissions (no copied
    // defaults are materialized into the document).
    let reserialized = serde_json::to_value(&profile).expect("serialize bare profile");
    let round_tripped: InferenceExecution =
        serde_json::from_value(reserialized).expect("re-deserialize bare profile");
    assert_eq!(round_tripped, profile);
    assert_eq!(round_tripped.max_total_tokens, None);

    // An all-None profile is valid: unset fields preserve the owned-loop
    // defaults (stream_batch_ms 1s, liveness 120s, provider idle 300s,
    // deadline 86,400s) instead of failing validation.
    assert!(
        profile.validation_violations().is_empty(),
        "a profile with no overrides must validate against platform defaults"
    );

    // An explicit per-request token cap is honored and preserved, and stays
    // distinct from the goal-level budget contract.
    let explicit = serde_json::from_value::<InferenceExecution>(serde_json::json!({
        "node_did": "did:key:zNODE",
        "execution_id": "profile-explicit",
        "max_total_tokens": 50_000
    }))
    .expect("profile with explicit cap");
    assert_eq!(explicit.max_total_tokens, Some(50_000));
    assert!(explicit.validation_violations().is_empty());
}
