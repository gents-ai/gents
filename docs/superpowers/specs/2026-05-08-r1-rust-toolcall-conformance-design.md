# R1 — Rust ToolCall Lifecycle Conformance Design

**Status:** Design (R1 of the Rust-conformance program)
**Date:** 2026-05-08
**Tracks:** [PR #152](https://github.com/sourcenetwork/defra-agent/pull/152) (B1 Lean spec); originating issue [#149](https://github.com/sourcenetwork/defra-agent/issues/149)
**Scope:** Schema migration + state-machine module + conformance tests. **No runtime behavior change.**

## Background

PR #152 landed the Lean spec for a daemon-visible `ToolCallContext` lifecycle (six states: `pending | running | completed | failed | timedOut | cancelled`, nine transitions, single-machine theorems T1..T5, composition theorems C1, C1', C2, C3). The conformance contract in `Proofs/Conformance/Contracts/Machines.lean` now emits the `toolCallMachine` entry with the full vocabulary and transition matrix.

The Rust runtime today does not match the spec in any meaningful way:

- `AgentToolCall.status` persists `"called"` (in-flight) and `"completed"` (terminal). Failures distinguish via `tool_failure_class` while `status` remains `"completed"`. No `lifecycle_state` field exists on the schema.
- `ToolFailureClass` in `crates/defra-agent/src/trace_export.rs:8-21` has 12 variants. Lean's `FailureClass` has 5.
- Tool-call lifecycle logic is scattered across `crates/defra-agent/src/session/tool_calls.rs` (free-floating `save_tool_call` / `update_started_tool_call` / `complete_tool_call` async functions) and `crates/defra-agent/src/hook/persistence.rs:160-272` (the hook bridge). No state-machine module, no centralized transition guards, no enum mapping the Lean vocabulary.
- The existing `RequestLifecycle` (in `crates/defra-agent/src/lifecycle.rs:189-204` and submodules under `crates/defra-agent/src/lifecycle/`) is the codebase's established pattern for this kind of state machine. Tool calls do not yet follow it.

R1 closes the structural gap: align vocabulary, centralize transitions in a Rust state-machine module mirroring `RequestLifecycle`, wire conformance tests that consume the Lean JSON. **R1 does not change runtime behavior** — no new timeouts, no cancellation propagation, no subprocess management. Those are R2..R5.

## Goals

- Replace `AgentToolCall.status` with a `lifecycle_state` field carrying the Lean 6-state vocabulary.
- Collapse the Rust `ToolFailureClass` enum to Lean's 5 variants, rebucketing existing call sites.
- Introduce a `ToolCallLifecycle` struct mirroring `RequestLifecycle`: every persistence write goes through a guarded transition method.
- Wire conformance tests that verify Rust enums match the Lean conformance JSON (`assert_lean_contract_vocabulary_matches`) and that runtime-persisted state pairs are legal Lean transitions (`assert_lean_transition_is_legal`).
- Migrate existing rows via DefraDB's native Lens migration system.

## Non-goals (explicitly out of scope for R1)

1. **Request-deadline propagation through to the tool layer.** R2.
2. **Hard timeouts on tool futures** (the actual operational fix for #149). R3.
3. **Cancellation token propagation into in-flight tool futures.** R4.
4. **Native-tool subprocess migration** (`glob`, `list_files`, etc. → managed-exec). R5.
5. **Startup recovery sweep for stuck tool calls.** Future R-recovery; rolled into R3.
6. **Persistent processes spanning turns** (codex `unified_exec` analog). B4.
7. **Sandbox/permission tier model.** B5.
8. **Observability counters** (`/healthz` active-tool, age-since-progress). B6.

## Architecture

### File layout

```
crates/
  defra-agent-lenses/                                          NEW workspace member
    agent_tool_call_lifecycle_v1_to_v2/
      Cargo.toml                                               crate-type cdylib (WASM)
      src/lib.rs                                               lens transform + tests
  defra-agent/
    src/tool_call_lifecycle.rs                                 NEW: enums, struct, ALL/as_str/from_persisted
    src/tool_call_lifecycle/
      rows.rs                                                  NEW: DefraDB row types
      transition.rs                                            NEW: impl ToolCallLifecycle methods
      query.rs                                                 NEW: load helpers
    src/session/tool_calls.rs                                  DELETED (functions absorbed into transition.rs)
    src/hook/persistence.rs                                    MODIFIED: hooks instantiate ToolCallLifecycle
    src/trace_export.rs                                        MODIFIED: ToolFailureClass collapsed
    src/migration.rs                                           NEW: idempotent schema patch + lens registration
```

This mirrors the existing `crates/defra-agent/src/lifecycle/{rows,transition,query,...}.rs` decomposition. Recovery and claim submodules are not included in R1 (recovery is future-phase; tool calls aren't claimed in the request sense).

### Schema migration via DefraDB native Lens system

Two GraphQL collection changes against `AgentToolCall`, performed via DefraDB's `collection_patch` mechanism:

1. **v1 → v2:** Add `lifecycle_state: String` field. Both `status` and `lifecycle_state` exist on this version.
2. **v2 → v3:** Remove the legacy `status` field, after a soak period.

R1 ships v1 → v2. The v2 → v3 cut is a follow-up issue once the soak period passes.

A WASM Lens crate (`crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/`) registers a forward lens v1 → v2 that reads each document's `status` and `tool_failure_class` and computes `lifecycle_state` plus a rebucketed `tool_failure_class`:

| `status` (v1 input) | `tool_failure_class` (v1 input) | `lifecycle_state` (v2 output) | `tool_failure_class` (v2 output) |
|---|---|---|---|
| `"called"` | (any) | `"running"` | (preserved, rebucketed if non-null) |
| `"completed"` | `null` | `"completed"` | `null` |
| `"completed"` | `"tool_timeout"` | `"timedOut"` | `null` |
| `"completed"` | `"invalid_tool_arguments"` etc. | `"failed"` | rebucketed to Lean 5 (see Section "FailureClass collapse") |

Mirrors `tools/integration-test/test-lenses/set_default/` in defradb.rs for crate structure. The lens runs on read for replicated documents at v1 schema and on write for new v2 rows.

A complementary inverse lens v2 → v1 drops `lifecycle_state` and is registered in the same step, so a v2 node can publish to a v1 peer mid-rollout.

The lens registration call lives in `crates/defra-agent/src/migration.rs`, invoked at startup. Idempotent: re-running on a v2/v3 deployment is a no-op.

A startup-time backfill query touches every existing `AgentToolCall` row to force the lens to run eagerly (rather than lazy-on-read), so dashboard queries don't encounter un-migrated documents post-R1.

## State and context types

### `ToolCallState` enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "timedOut" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    const fn is_cancellable(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}
```

Direct mirror of `PersistedLifecycleState` from `lifecycle.rs:84-153`. **Single enum, no Local/Persisted split.** Request needs the split because the runtime distinguishes "Streaming" from persistence's "Processing/InputRequired"; tool calls have no analogous runtime-only state.

### `FailureClass` enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    ArgumentInvalid,
    ServiceUnavailable,
    Transport,
    ToolReturnedError,
    External,
}

impl FailureClass {
    const ALL: [Self; 5] = [
        Self::ArgumentInvalid,
        Self::ServiceUnavailable,
        Self::Transport,
        Self::ToolReturnedError,
        Self::External,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::ArgumentInvalid => "argumentInvalid",
            Self::ServiceUnavailable => "serviceUnavailable",
            Self::Transport => "transport",
            Self::ToolReturnedError => "toolReturnedError",
            Self::External => "external",
        }
    }

    fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "argumentInvalid" => Some(Self::ArgumentInvalid),
            "serviceUnavailable" => Some(Self::ServiceUnavailable),
            "transport" => Some(Self::Transport),
            "toolReturnedError" => Some(Self::ToolReturnedError),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}
```

Replaces the existing 12-variant `ToolFailureClass` in `crates/defra-agent/src/trace_export.rs:8-21`. The collapse loses operator-visible granularity in dashboards — that is the cost of strict spec conformance.

### `ToolFailureClass` collapse (rebucketing table)

| Old `ToolFailureClass` | New `FailureClass` | Notes |
|---|---|---|
| `ServiceUnavailable` | `ServiceUnavailable` | identity |
| `ToolNotFound`, `ResourceNotFound`, `ServiceSchemaDrift` | `ServiceUnavailable` | service-side discovery failures |
| `InvalidToolArguments`, `InvalidJsonArguments`, `ArgumentsNotObject` | `ArgumentInvalid` | request-side validation |
| `ToolRuntimeError`, `NonzeroCommandExit`, `Unclassified` | `ToolReturnedError` | tool ran and emitted an error |
| `ToolTimeout` | (no FailureClass) | becomes `lifecycle_state = "timedOut"` instead |
| `DeadlineOrInferenceFailure` | (none — non-tool concern) | request-level state, doesn't belong here |

Every call site in `trace_export.rs` and `session/` that constructs the old enum gets rebucketed in the same PR. The lens migration applies the same rebucketing to historical rows, so the dashboard vocabulary is uniform across the migration boundary.

### `ToolCallLifecycle` struct

```rust
pub struct ToolCallLifecycle {
    node: Arc<EmbeddedNode>,
    session_id: String,
    tool_call_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    doc_id: Option<String>,
    state: ToolCallState,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_class: Option<FailureClass>,
}

impl ToolCallLifecycle {
    /// Construct without persisting. The first transition method creates the row.
    pub fn new(
        node: Arc<EmbeddedNode>,
        session_id: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        args: String,
    ) -> Self;

    /// Load an existing tool-call row. Returns the lifecycle in its current
    /// persisted state. Used by retry paths and recovery.
    pub async fn load(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<Self>>;
}
```

Mirrors `RequestLifecycle` from `lifecycle.rs:189-204` field-for-field where applicable. Fields the request struct has but the tool struct does not:

- No `agent_name` / `agent_did` / `behavior_id` / `execution_origin` / `backend_id` — tool calls are session-scoped, not directly bound to agent identity at this layer.
- No `failure_reason` text field — only the structured `failure_class`. Free-form failure text rides on the `result` field, which `complete()` and `fail()` both set.
- No `progress_seq` — tool calls do not stream progress.
- No `deadline_duration_secs` / `claimed_deadline_at` — **deadline propagation is R2, deliberately absent from R1.**
- No `valid_until_at_claim` — tool calls do not have TTL semantics.

`new()` is synchronous and does not persist; the first transition method (`start_running`) creates the DefraDB row in state `Running`. This matches the existing `save_tool_call` semantics where the hook layer first materializes the row at tool-invocation time.

`load()` handles the retry case: if the daemon retries a failed inference and re-emits the same `tool_call_id`, `load()` returns the existing lifecycle in whatever state it last reached.

## Transitions (the methods)

Live in `crates/defra-agent/src/tool_call_lifecycle/transition.rs`, mirroring `crates/defra-agent/src/lifecycle/transition.rs:26-660`. Each method has an `ensure_state(&[allowed], "method_name")` guard at the top, performs the GraphQL mutation atomically with retry, and updates the in-memory `state` only after the DB confirms.

```rust
impl ToolCallLifecycle {

    /// Pending → Running. Creates the DefraDB row if missing; idempotent
    /// if already in Running. Sets `started_at`.
    pub async fn start_running(&mut self) -> Result<()>;

    /// Pending → Failed. Used when the dispatcher cannot start the call
    /// (MCP service unreachable, argument parse failure detected pre-spawn).
    pub async fn spawn_failed(
        &mut self,
        failure: FailureClass,
        reason: &str,
    ) -> Result<()>;

    /// Running → Completed. Writes the tool result; sets completed_at, latency_ms.
    pub async fn complete(&mut self, result: &str) -> Result<()>;

    /// Running → Failed. For tool errors during execution (non-zero exit,
    /// runtime exception, transport mid-call). Sets failure_class.
    pub async fn fail(
        &mut self,
        result: &str,
        failure: FailureClass,
    ) -> Result<()>;

    /// Running → TimedOut. R1 does not call this from runtime code — exists
    /// for R3 to wire up. Defining now is part of R1's "centralize the
    /// state machine" goal.
    pub async fn timeout(&mut self) -> Result<()>;

    /// Pending → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_before_dispatch(&mut self) -> Result<()>;

    /// Running → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_during_run(&mut self) -> Result<()>;
}
```

### Mapping from Lean transitions

| Lean `Transition` constructor | Rust method | Notes |
|---|---|---|
| `dispatch` | `start_running` | "Row created in Running" path. Renamed for Rust semantic clarity. |
| `spawnFailed(failure)` | `spawn_failed(failure, reason)` | `reason` is operator-facing free text. |
| `complete` | `complete(result)` | `result` is the tool's output. |
| `fail(failure)` | `fail(result, failure)` | `result` carries the error message. |
| `timeout` | `timeout` | Defined in R1, unwired until R3. |
| `cancelBeforeDispatch` | `cancel_before_dispatch` | Defined in R1, unwired until R4. |
| `cancelDuringRun` | `cancel_during_run` | Defined in R1, unwired until R4. |
| `timeAdvance`, `persistenceStep` | (no Rust method) | Spec-internal trace primitives, not exposed. |

**Naming deviation rationale.** Lean uses `dispatch`. Rust uses `start_running`. The Lean term reflects "the daemon dispatches a queued call into execution"; the Rust runtime has no queueing layer in R1, so `start_running` describes what the method actually does. The conformance test verifies the *transition* (Pending → Running) matches Lean, not the method name.

### Why all seven methods land in R1, including unused ones

The deep review of B1 flagged a smell where the spec's hypotheses didn't drive proof-relevant constraints. The Rust analog: if R1 only implements the methods runtime currently calls (`start_running`, `complete`, `fail`), then `timeout` / `cancel_*` exist only in spec, not code. R3/R4 would have to introduce them with the risk of subtle drift. Defining all seven in R1 makes R3's job "wire up the existing `timeout` method to fire on deadline-exceeded" rather than "introduce timeout AND wire it up." The conformance tests verify each method's `ensure_state` guard matches the Lean transition's pre-state precondition, so every transition is checked even when not yet triggered.

### `ensure_state` guard

Mirrors the existing helper. Returns a structured error on guard failure:

```rust
self.ensure_state(
    &[ToolCallState::Running],
    "complete",
)?;
```

Returns `Err(IllegalToolCallTransition { from, to, method })`. The error is logged and surfaced as a daemon-level fault rather than a tool-call failure — illegal transitions are programmer errors.

## Conformance tests

Three buckets, paralleling existing patterns.

### Bucket 1 — Vocabulary tests (in-module, fast)

In `crates/defra-agent/src/tool_call_lifecycle.rs`, a `#[cfg(test)] mod tests` block mirroring `lifecycle.rs:240-336`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean_vocab_test::{
        assert_lean_contract_vocabulary_matches,
        assert_state_machine_contract_is_complete,
        lean_state_machine_contract,
        LeanContractVocabulary,
    };

    #[test]
    fn rust_tool_call_state_vocabulary_matches_lean_model() {
        let rust_states = ToolCallState::ALL
            .iter()
            .copied()
            .map(ToolCallState::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "ToolCallState",
            rust_source: "ToolCallState::ALL",
            rust_values: &rust_states,
        });
    }

    #[test]
    fn rust_failure_class_vocabulary_matches_lean_model() {
        let rust_classes = FailureClass::ALL
            .iter()
            .copied()
            .map(FailureClass::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "ToolFailureClass",
            rust_source: "FailureClass::ALL",
            rust_values: &rust_classes,
        });
    }

    #[test]
    fn tool_call_state_machine_contract_is_complete() {
        assert_state_machine_contract_is_complete("ToolCall");
    }

    #[test]
    fn tool_call_terminal_partition_matches_lean_contract() {
        let machine = lean_state_machine_contract("ToolCall");
        let terminal = ToolCallState::ALL
            .iter()
            .copied()
            .filter(|s| s.is_terminal())
            .map(ToolCallState::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            terminal,
            machine.terminal_states.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tool_call_round_trip_persisted_vocabulary() {
        for state in ToolCallState::ALL {
            assert_eq!(ToolCallState::from_persisted(state.as_str()), Some(state));
        }
        assert_eq!(ToolCallState::from_persisted("called"), None);
        assert_eq!(ToolCallState::from_persisted("unknown"), None);
    }

    #[test]
    fn cancellable_iff_non_terminal() {
        for state in ToolCallState::ALL {
            assert_eq!(state.is_cancellable(), !state.is_terminal());
        }
    }
}
```

The `cancellable_iff_non_terminal` test is the Rust analog of Lean theorem T4. The vocabulary tests are direct analogs of `lifecycle.rs:241-265`.

### Bucket 2 — Transition matrix tests (integration)

In `crates/defra-agent/tests/state_machine_conformance.rs`:

```rust
#[test]
fn tool_call_transitions_match_lean_contract() {
    assert_lean_transition_is_legal("ToolCall", "pending", "running");
    assert_lean_transition_is_legal("ToolCall", "pending", "failed");
    assert_lean_transition_is_legal("ToolCall", "pending", "cancelled");
    assert_lean_transition_is_legal("ToolCall", "running", "completed");
    assert_lean_transition_is_legal("ToolCall", "running", "failed");
    assert_lean_transition_is_legal("ToolCall", "running", "timedOut");
    assert_lean_transition_is_legal("ToolCall", "running", "cancelled");
    // T1 — terminal irreversibility
    assert_lean_transition_is_illegal("ToolCall", "completed", "running");
    assert_lean_transition_is_illegal("ToolCall", "failed", "running");
    assert_lean_transition_is_illegal("ToolCall", "timedOut", "running");
    assert_lean_transition_is_illegal("ToolCall", "cancelled", "running");
}
```

These are pure Lean-contract assertions — they verify the conformance JSON enumerates the right transition pairs.

### Bucket 3 — Runtime-on-Rust tests (integration)

A new test file `crates/defra-agent/tests/tool_call_lifecycle_conformance.rs`. Spins up a real DefraDB instance via the existing test harness and exercises:

- `start_running → complete` persists `lifecycle_state = "completed"`.
- `start_running → fail(External)` persists `lifecycle_state = "failed"`, `tool_failure_class = "external"`.
- Terminal irreversibility: after `complete()`, `fail()` returns `IllegalToolCallTransition`.
- Idempotent `start_running()`: calling twice creates exactly one DefraDB row.
- `ToolCallLifecycle::load` returns the persisted state for an existing row.

These exercise the GraphQL mutations and verify the persisted vocabulary matches what the spec dictates. They are conformance-of-runtime tests, not just conformance-of-types.

### Lean-side prerequisite

PR #152 added the `toolCallMachine` entry to `Conformance/Contracts/Machines.lean`. That entry needs one extension before R1 lands its tests: a `"ToolFailureClass"` vocabulary entry next to the existing `"ToolCallState"` entry, sourced from `ToolExecution.FailureClass.all`. Trivial one-line addition; included as the first task of R1's implementation plan.

## Hook layer integration

`crates/defra-agent/src/hook/persistence.rs:160-272` is the bridge between agent runtime tool execution and persistence. After R1:

```rust
async fn on_tool_call(&self, tool_call_id, tool_name, args) -> Result<()> {
    let mut lc = ToolCallLifecycle::new(
        self.node.clone(),
        self.session_id.clone(),
        tool_call_id.clone(),
        message_sequence,
        tool_name,
        args,
    );
    lc.start_running().await?;
    self.in_flight_lifecycles.lock().insert(tool_call_id, lc);
    Ok(())
}

async fn on_tool_result(&self, tool_call_id, result, error) -> Result<()> {
    let mut lc = self.in_flight_lifecycles.lock().remove(tool_call_id)
        .ok_or_else(|| anyhow!("on_tool_result for unknown tool_call_id"))?;
    match error {
        None => lc.complete(result).await,
        Some(err) => {
            let fc = classify_runtime_error(err);
            lc.fail(result, fc).await
        }
    }
}
```

The hook holds an in-flight `ToolCallLifecycle` map keyed by `tool_call_id` for the duration of the inference turn. A `Drop` impl on the hook clears the map to avoid leaks if a tool call starts and never produces a result.

`session/tool_calls.rs` is deleted; its mutation logic is absorbed into `tool_call_lifecycle/transition.rs` methods.

## Out of scope (and where each lands)

| Item | Where it lands |
|---|---|
| Request-deadline propagation through to the tool layer | R2 |
| Hard timeouts on tool futures (the operational fix for #149) | R3 |
| Cancellation token propagation into in-flight tool futures | R4 |
| Native-tool subprocess migration to managed-exec | R5 |
| Startup recovery sweep for stuck tool calls | R3 or future R-recovery |
| Persistent processes spanning turns | B4 |
| Sandbox/permission tier model | B5 |
| Observability counters | B6 |

## Risks

1. **Operator-visible granularity loss.** Reducing `ToolFailureClass` from 12 to 5 variants means `ToolNotFound` / `ResourceNotFound` / `ServiceSchemaDrift` collapse to `ServiceUnavailable` in dashboards. The lens rebuckets historical rows, so there is no vocabulary discontinuity at the migration boundary, but operators do see the smaller vocabulary. This is the cost of strict spec conformance and the user-chosen "collapse" option.

2. **Lens migration on large datasets.** Existing production deployments may have many `AgentToolCall` rows. R1 includes a startup-time backfill query that touches every row to force eager lens execution, ensuring no dashboard query encounters un-migrated documents.

3. **Hook in-flight map memory.** The `in_flight_lifecycles: Mutex<HashMap<ToolCallId, ToolCallLifecycle>>` is bounded by the number of tool calls in a single inference turn (in practice < 10). If a tool call starts and the model never emits a result, the map entry leaks until the hook drops. Mitigated by a `Drop` impl on the hook that clears the map.

4. **Migration ordering in test environments.** CI test harnesses create a fresh DefraDB per test, so each fresh instance starts at schema v1 and runs the migration. Defensive: a `migration::test_round_trip_lens` integration test creates v1 rows, runs the migration, asserts the v2 shape.

5. **Schema-version drift across nodes during rollout.** P2P deployments may have one node on v2 and another on v1 mid-rollout. The forward lens handles v1 → v2 reads on v2 nodes; the inverse lens v2 → v1 must drop `lifecycle_state` cleanly. Worth a test: register both lenses; replicate a v2 document to a v1 node; verify v1 reads see the legacy `status` field.

## Future work flagged for later phases

- **Codebase-wide deadline audit** (already noted in project memory `project_deadline_audit_followup.md`). Trigger after R3.
- **`Coherent` predicate analog on the Rust side.** When B4 lands the persistent-process distinction in Lean, the Rust state machine will need a similar shape. R1 doesn't introduce it because there's only one coherence shape (bound-to-request).
- **v2 → v3 schema cut** removing the legacy `status` field after a soak period.

## References

- B1 spec design: `docs/superpowers/specs/2026-05-08-toolcall-lifecycle-spec-design.md`
- B1 Lean spec: `Proofs/ToolExecution/{State,Transition,Properties,Executable}.lean`
- B1 PR: [#152](https://github.com/sourcenetwork/defra-agent/pull/152)
- Originating issue: [#149](https://github.com/sourcenetwork/defra-agent/issues/149)
- CancelCause follow-up: [#153](https://github.com/sourcenetwork/defra-agent/issues/153)
- Existing precedent on the Rust side: `crates/defra-agent/src/lifecycle.rs` and `crates/defra-agent/src/lifecycle/{rows,transition,query,claim,recovery}.rs`
- DefraDB Lens migration precedent: `defradb.rs` repo `tools/integration-test/test-lenses/set_default/`
- Project conventions: `CLAUDE.md`
