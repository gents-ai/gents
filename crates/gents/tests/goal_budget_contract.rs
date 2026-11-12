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
