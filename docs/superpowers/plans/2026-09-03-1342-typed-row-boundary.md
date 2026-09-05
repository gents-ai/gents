# #1342 Typed lifecycle_state at the Row Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `lifecycle_state` is typed where rows are deserialized, so the private wrapper structs and per-site `parse_opt` calls introduced by #1330 disappear.

**Architecture:** `RequestLifecycleState` gains strict `serde::Deserialize`/`Serialize` (unknown value = error; matches the clean-cutover rule). `gents_protocol::row::AgentRequestRow.lifecycle_state` becomes `Option<RequestLifecycleState>` and the row gains the schema fields it lacks (`caused_by_parent_request_doc_id`, `caused_by_parent_tool_call_id`, `caused_by_parent_tool_call_doc_id`, `subagent_depth`, `terminalized_at`, `terminal_redrive_attempts`, `interrupt_requested_at`, `valid_until`, `deadline`, workspace fields, retry fields — reconcile against `agent_request.graphql`). Private request-row structs that are subsets of it are deleted in favor of `AgentRequestRow` (selecting only the fields they need is still fine: serde ignores absent optional fields). Predicates take `Option<RequestLifecycleState>`.

**Tech Stack:** Rust.

**Spec:** GitHub issue #1342. Depends on #1330.

## Global Constraints

- Strict typing: a row whose `lifecycle_state` is a string outside the vocabulary fails deserialization with the value named. No `#[serde(default)]` that maps unknown to `None`.
- `AgentRequestRow` is the one request row type in `gents-protocol`; `run_timeline::TimelineRequestRow` may keep extra projection fields but must embed or derive from it rather than redeclaring the request columns.
- Net code deletion (target: the ~40 private request-row declarations shrink to the handful that carry non-request projection fields).

---

### Task 1: Serde on the owner and the canonical row

**Files:** `crates/gents-protocol/src/request_lifecycle.rs` (serde impls + tests: round trip, unknown string is an error naming the value), `crates/gents-protocol/src/row.rs` (`AgentRequestRow.lifecycle_state: Option<RequestLifecycleState>`; add missing schema fields; `is_terminal()`, `is_claimable()` helpers on the row), `crates/gents-protocol/src/graphql.rs` (selection includes the fields used).

- [ ] Tests first; implement; `cargo test -p gents-protocol` green; commit — `protocol: lifecycle_state is typed on AgentRequestRow (#1342)`.

### Task 2: Runtime rows

**Files:** every `crates/gents/src/**` struct with `lifecycle_state: Option<String>` or `String` that deserializes an `AgentRequest` (`admission/recovery.rs`, `tool_call_lifecycle/recovery.rs` ×2, `trigger_engine/cross_deployment_cancel_mirror.rs`, `background_completion/reconciliation.rs`, `workspace/overlay.rs`, `background_tools.rs`, `lifecycle/rows.rs`, `watcher/query/rows.rs` ×2, `lifecycle/queue/*`, `run_timeline.rs`, `toolset/session_history.rs`, `descendant_graph.rs`, `goal.rs`, `graph_pipeline/run.rs`, `trigger_engine/*_source.rs`). For each: either replace the struct with `AgentRequestRow` (when it is a pure subset) or change the field type to `Option<RequestLifecycleState>` (when the struct carries other projection fields). Delete `is_active_non_pending` where it is `!is_pending()`. Predicates take the typed value; `parse_opt` calls at those sites go away.

- [ ] `cargo test -p gents --lib` green, then `cargo test -p gents` full; commit per subsystem if large — `runtime: request rows carry typed lifecycle_state (#1342)`.

### Task 3: CLI, bridge, desktop-core rows

**Files:** `crates/gents-cli/src/**` (`request_helpers.rs`, `http/*`, `commands/*`), `crates/gents-desktop-bridge/src/**`, `crates/gents-desktop-core/src/**` request-row structs; view structs that expose `lifecycle_state` to TS keep `String`/`Option<String>` on the wire (`as_str()` at the view boundary) so generated TS is unchanged.

- [ ] Tests green per crate; commit — `cli+desktop: request rows carry typed lifecycle_state (#1342)`.

### Task 4: Tests-side lists
- [ ] `crates/gents/tests/support/r5_conformance/{runner,invariants}.rs`, `conformance/{compaction_gate,streaming_compaction,replicated_request_convergence,request_lifecycle}.rs`, `e2e_live/*`: terminal lists become `RequestLifecycleState::is_terminal`/`terminal_graphql_list`; note `invariants.rs:225` omits `interrupted` today — after this change it includes it; confirm the affected invariant test still passes and say why the omission was not load-bearing.
- [ ] Gate: `cargo test -p gents`, `cargo test -p gents-cli -p gents-desktop-bridge -p gents-desktop-core`, `cargo check --workspace --all-targets`, `cargo fmt --all --check`; grep `lifecycle_state: Option<String>` across `crates` returns only wire-view structs; net deletion check.
