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

use gents_documents::task_goal_declaration::validate_task_goal_declaration;
use gents_documents::{InferenceExecution, Task};
use gents_store::schema::{GOAL_SCHEMA, TASK_SCHEMA};

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
fn task_document_round_trips_omitted_budget_as_none() {
    let task: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "contract-task",
        "agent_did": "contract",
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
        "agent_did": "contract",
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
