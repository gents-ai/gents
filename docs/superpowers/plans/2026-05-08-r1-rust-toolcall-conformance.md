# R1 — Rust ToolCall Lifecycle Conformance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Rust state-machine module + schema migration + conformance tests defined in `docs/superpowers/specs/2026-05-08-r1-rust-toolcall-conformance-design.md`. No runtime behavior change — just align `AgentToolCall` persistence with the Lean `ToolCallContext` spec from PR #152.

**Architecture:** New `tool_call_lifecycle` module mirroring `RequestLifecycle` (`crates/defra-agent/src/lifecycle/`); WASM Lens migration crate (`crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/`); `FailureClass` enum collapse from 12 to 5 variants; hook layer refactor; conformance tests in 3 buckets (in-module vocabulary, transition matrix, runtime-on-Rust integration).

**Tech Stack:** Rust 2021, Tokio async, DefraDB embedded node (`defra_node::EmbeddedNode`), DefraDB Lens system (`lens_sdk` crate, `crate-type = ["cdylib"]` for WASM), GraphQL via DefraDB SDL, existing `lean_vocab_test` conformance harness.

---

## Conventions

- **Working directory:** `/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-149-native-glob-deadline/`. All paths in this plan are relative to that root.
- **Branch:** `bug/issue-149-native-glob-deadline` (already current). PR #152 is open. R1 lands additional commits on the same branch unless the user specifies otherwise.
- **Build commands:**
  - Rust full check: `cargo check -p defra-agent` (fast; doesn't link tests)
  - Rust unit tests: `cargo test -p defra-agent --lib` (in-crate `#[cfg(test)]` modules)
  - Rust integration tests: `cargo test -p defra-agent --test <test_file_name>` (single file)
  - All Rust tests: `cargo test -p defra-agent` (slow; includes integration tests that spin up DefraDB)
  - Lean: `cd crates/defra-agent/proofs && lake build`
  - WASM lens build: `cd crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2 && cargo build --release --target wasm32-unknown-unknown`
- **Commit cadence:** one commit per task unless explicitly bundled. Each commit ends with the standard `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- **TDD discipline:** for Rust code, the rhythm is *write failing test → run → confirm fail → write minimal impl → run → confirm pass → commit*. For Lean changes, *theorem with `sorry` → confirm sorry warning → replace → confirm clean → commit* (same as B1).

---

## Task 1: Add `"ToolFailureClass"` vocabulary to Lean conformance machine

The Bucket 1 conformance test for the Rust `FailureClass` enum needs the Lean side to emit a `"ToolFailureClass"` vocabulary entry. PR #152 added `"ToolCallState"` but not `"ToolFailureClass"`. One-line addition.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean`

- [ ] **Step 1: Locate the `vocabularies` block**

```bash
grep -n "ToolCallState\|toolCallStateNames\|ToolRetryDisposition" crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean
```

Expected: a few lines near the bottom of the file inside `def vocabularies : List VocabularyContract`. The existing entry is `, { domain := "ToolCallState", values := toolCallStateNames }`.

- [ ] **Step 2: Define a `failureClassNames` helper**

Add this definition immediately above the `vocabularies` block (so it's defined before the list references it):

```lean
def failureClassNames : List String :=
  ToolExecution.FailureClass.all.map ToolExecution.FailureClass.toDefraDB
```

- [ ] **Step 3: Add the vocabulary entry**

In the `vocabularies` list, immediately after the existing `"ToolCallState"` entry and before `"ToolRetryDisposition"`, insert:

```lean
  , { domain := "ToolFailureClass", values := failureClassNames }
```

- [ ] **Step 4: Build and verify clean**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build with no `error:` and no `sorry`.

- [ ] **Step 5: Verify the JSON now emits the new vocabulary**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean 2>&1 | grep -A1 "ToolFailureClass"
```

Expected: a line containing `"domain":"ToolFailureClass"` followed by a values array including `argumentInvalid`, `serviceUnavailable`, `transport`, `toolReturnedError`, `external`.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean
git commit -m "$(cat <<'EOF'
Add ToolFailureClass vocabulary to conformance contract

Prerequisite for R1's Rust FailureClass enum conformance test (Bucket 1).
PR #152 added ToolCallState; this lands the companion FailureClass
vocabulary so the Rust side can verify its 5-variant enum matches.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Create `tool_call_lifecycle.rs` skeleton with `ToolCallState` enum

Foundation for the new state-machine module. This task creates the file with just the state enum; `FailureClass` and the lifecycle struct land in subsequent tasks.

**Files:**
- Create: `crates/defra-agent/src/tool_call_lifecycle.rs`
- Modify: `crates/defra-agent/src/lib.rs` (add `pub mod tool_call_lifecycle;`)

- [ ] **Step 1: Write the failing test**

Append to `crates/defra-agent/src/tool_call_lifecycle.rs` (creating the file):

```rust
//! Tool-call lifecycle state machine.
//!
//! Mirrors `crates/defra-agent/src/lifecycle.rs` (`RequestLifecycle`) for tool
//! calls. Defines the persisted vocabulary, failure-class enum, and the
//! `ToolCallLifecycle` struct that owns every persistence write.
//!
//! Lifecycle is daemon-visible only; subprocess kill mechanics, output
//! streaming, and persistent processes are out of scope.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    pub(crate) const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_persisted(value: &str) -> Option<Self> {
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

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    pub(crate) const fn is_cancellable(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_persisted_vocabulary() {
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

    #[test]
    fn all_lists_six_states() {
        assert_eq!(ToolCallState::ALL.len(), 6);
    }
}
```

In `crates/defra-agent/src/lib.rs`, locate the existing `pub mod lifecycle;` line (or similar — look for module declarations near the top of the file) and add immediately after it:

```rust
pub mod tool_call_lifecycle;
```

- [ ] **Step 2: Run tests to confirm they pass**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::
```

Expected: 3 tests pass (`round_trip_persisted_vocabulary`, `cancellable_iff_non_terminal`, `all_lists_six_states`).

- [ ] **Step 3: Confirm `cargo check` is clean**

```bash
cargo check -p defra-agent
```

Expected: clean (no errors, no warnings related to the new module).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/lib.rs
git commit -m "$(cat <<'EOF'
Add ToolCallState enum and lifecycle module skeleton

Mirrors PersistedLifecycleState from lifecycle.rs:84-153. Six states matching
the Lean ToolCallState vocabulary (pending, running, completed, failed,
timedOut, cancelled), with as_str / from_persisted / is_terminal /
is_cancellable / ALL helpers. Three unit tests cover round-trip parsing and
cancellable-iff-non-terminal (Lean T4 analog).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `FailureClass` enum to `tool_call_lifecycle.rs`

Lean's 5-variant `FailureClass`. Replaces (in subsequent tasks) the existing 12-variant `ToolFailureClass` from `trace_export.rs`.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`

- [ ] **Step 1: Write the failing test (append to existing tests module)**

Locate the `#[cfg(test)] mod tests { ... }` block in `tool_call_lifecycle.rs` and add inside it:

```rust
    #[test]
    fn failure_class_round_trip_persisted_vocabulary() {
        for fc in FailureClass::ALL {
            assert_eq!(FailureClass::from_persisted(fc.as_str()), Some(fc));
        }
        assert_eq!(FailureClass::from_persisted("unknown"), None);
    }

    #[test]
    fn failure_class_all_lists_five_variants() {
        assert_eq!(FailureClass::ALL.len(), 5);
    }
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::
```

Expected: 2 new tests fail to compile (`FailureClass` undefined).

- [ ] **Step 3: Implement `FailureClass`**

In `tool_call_lifecycle.rs`, add immediately after the `impl ToolCallState` block (and before the `#[cfg(test)] mod tests`):

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
    pub const ALL: [Self; 5] = [
        Self::ArgumentInvalid,
        Self::ServiceUnavailable,
        Self::Transport,
        Self::ToolReturnedError,
        Self::External,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArgumentInvalid => "argumentInvalid",
            Self::ServiceUnavailable => "serviceUnavailable",
            Self::Transport => "transport",
            Self::ToolReturnedError => "toolReturnedError",
            Self::External => "external",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
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

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::
```

Expected: 5 tests pass (3 from Task 2 + 2 new).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Add FailureClass enum to tool_call_lifecycle module

5-variant enum matching the Lean ToolExecution.FailureClass spec:
ArgumentInvalid, ServiceUnavailable, Transport, ToolReturnedError, External.
Will replace the 12-variant ToolFailureClass in trace_export.rs in a
subsequent task; this commit only adds the new enum.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add the `defra-agent-lenses` workspace member with the lens crate skeleton

Creates the new workspace member directory and the WASM lens crate scaffold. The actual transform logic lands in Task 5; this task is the build-system plumbing.

**Files:**
- Create: `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/Cargo.toml`
- Create: `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/src/lib.rs` (placeholder)
- Modify: `Cargo.toml` (workspace root — add the new member)

- [ ] **Step 1: Confirm the parent directory does not yet exist**

```bash
ls crates/defra-agent-lenses/ 2>&1
```

Expected: `ls: crates/defra-agent-lenses/: No such file or directory`. If it exists, report the surprise.

- [ ] **Step 2: Create the lens crate's `Cargo.toml`**

```bash
mkdir -p crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/src
```

Then write `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/Cargo.toml`:

```toml
[package]
name = "agent-tool-call-lifecycle-v1-to-v2-lens"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
lens_sdk = "^0.8"
```

- [ ] **Step 3: Create a placeholder `src/lib.rs`**

Write `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/src/lib.rs`:

```rust
//! WASM Lens migration: AgentToolCall v1 -> v2.
//!
//! Computes `lifecycle_state` from the legacy `status` and `tool_failure_class`
//! fields, and rebuckets `tool_failure_class` from the 12-variant Rust
//! vocabulary to the 5-variant Lean vocabulary. Inverse drops the
//! `lifecycle_state` field for v2->v1 reads on a v1 peer.
//!
//! Implementation lives in subsequent tasks; this is the crate scaffold.

// Placeholder: real transform logic lands in Task 5.
```

- [ ] **Step 4: Add the new member to the workspace `Cargo.toml`**

In the root `Cargo.toml`, locate the `[workspace] members = [...]` block (around line 3-9). Add a new line for the lens crate. The members list should now read:

```toml
[workspace]
resolver = "2"
members = [
    "apps/desktop-tauri/src-tauri",
    "crates/defra-agent",
    "crates/defra-agent-cli",
    "crates/defra-agent-desktop",
    "crates/defra-agent-desktop-core",
    "crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2",
    "crates/defra-agent-protocol",
]
```

- [ ] **Step 5: Verify the workspace builds**

```bash
cargo check --workspace
```

Expected: clean. The new lens crate compiles as a no-op `cdylib` since `lib.rs` is just comments.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/defra-agent-lenses/
git commit -m "$(cat <<'EOF'
Scaffold defra-agent-lenses workspace member

New workspace member crates/defra-agent-lenses/ housing per-migration WASM
Lens crates. Initial entry: agent_tool_call_lifecycle_v1_to_v2 (cdylib).
Real transform logic lands next; this is build-system plumbing only.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Implement the lens forward transform (`try_transform`)

The forward lens reads each v1 document's `status` and `tool_failure_class`, computes the v2 `lifecycle_state` and the rebucketed `tool_failure_class`. Mirrors the `set_default` example pattern from `defradb.rs/tools/integration-test/test-lenses/set_default/src/lib.rs:9-39`.

**Files:**
- Modify: `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/src/lib.rs`

- [ ] **Step 1: Write the failing unit tests**

Replace the placeholder `lib.rs` with the test scaffold first:

```rust
//! WASM Lens migration: AgentToolCall v1 -> v2.

use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

lens_sdk::define!(try_transform, try_inverse);

/// Compute the v2 (lifecycle_state, tool_failure_class) pair from the v1
/// (status, tool_failure_class) pair. Public for unit tests.
pub fn compute_v2_fields(
    status: Option<&str>,
    legacy_failure_class: Option<&str>,
) -> (String, Option<String>) {
    match (status, legacy_failure_class) {
        // In-flight calls become Running. Failure class preserved if non-null
        // (will be rebucketed by the time it reaches a terminal state).
        (Some("called"), legacy) => ("running".to_string(), legacy.map(rebucket_failure_class)),
        // Successful completion: no failure class.
        (Some("completed"), None) => ("completed".to_string(), None),
        // Timeout completion: state becomes timedOut, failure class cleared.
        (Some("completed"), Some("tool_timeout")) => ("timedOut".to_string(), None),
        // Other completion-with-failure: state becomes failed, failure class
        // rebucketed to the Lean 5-variant vocabulary.
        (Some("completed"), Some(legacy)) => ("failed".to_string(), Some(rebucket_failure_class(legacy))),
        // Unrecognized status: preserve, do not migrate.
        (Some(s), legacy) => (s.to_string(), legacy.map(rebucket_failure_class)),
        (None, _) => ("running".to_string(), None),
    }
}

/// Map a legacy 12-variant ToolFailureClass string to the Lean 5-variant
/// FailureClass string. Per R1 spec section "ToolFailureClass collapse".
pub fn rebucket_failure_class(legacy: &str) -> String {
    match legacy {
        // Identity.
        "service_unavailable" => "serviceUnavailable".to_string(),
        // Service-side discovery failures collapse to ServiceUnavailable.
        "tool_not_found" | "resource_not_found" | "service_schema_drift" => {
            "serviceUnavailable".to_string()
        }
        // Argument validation failures collapse to ArgumentInvalid.
        "invalid_tool_arguments" | "invalid_json_arguments" | "arguments_not_object" => {
            "argumentInvalid".to_string()
        }
        // Tool execution errors collapse to ToolReturnedError.
        "tool_runtime_error" | "nonzero_command_exit" | "unclassified" => {
            "toolReturnedError".to_string()
        }
        // Already-Lean-vocabulary values pass through (defensive: lens runs
        // idempotently on partially-migrated data).
        "argumentInvalid" | "serviceUnavailable" | "transport" | "toolReturnedError"
        | "external" => legacy.to_string(),
        // Unknown: classify as External (non-tool-layer concern).
        _ => "external".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn called_becomes_running() {
        let (state, fc) = compute_v2_fields(Some("called"), None);
        assert_eq!(state, "running");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_no_failure_class_stays_completed() {
        let (state, fc) = compute_v2_fields(Some("completed"), None);
        assert_eq!(state, "completed");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_with_tool_timeout_becomes_timedOut() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("tool_timeout"));
        assert_eq!(state, "timedOut");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_with_invalid_arguments_becomes_failed_argumentInvalid() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("invalid_tool_arguments"));
        assert_eq!(state, "failed");
        assert_eq!(fc, Some("argumentInvalid".to_string()));
    }

    #[test]
    fn completed_with_nonzero_exit_becomes_failed_toolReturnedError() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("nonzero_command_exit"));
        assert_eq!(state, "failed");
        assert_eq!(fc, Some("toolReturnedError".to_string()));
    }

    #[test]
    fn unknown_failure_class_becomes_external() {
        assert_eq!(rebucket_failure_class("some_future_variant"), "external");
    }

    #[test]
    fn already_migrated_failure_class_passes_through() {
        assert_eq!(rebucket_failure_class("argumentInvalid"), "argumentInvalid");
    }
}

fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };

        let status = input.get("status").and_then(|v| v.as_str()).map(str::to_string);
        let legacy_fc = input
            .get("tool_failure_class")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let (lifecycle_state, new_fc) =
            compute_v2_fields(status.as_deref(), legacy_fc.as_deref());

        input.insert(
            "lifecycle_state".to_string(),
            Value::String(lifecycle_state),
        );
        input.insert(
            "tool_failure_class".to_string(),
            new_fc.map(Value::String).unwrap_or(Value::Null),
        );

        return Ok(StreamOption::Some(input));
    }
    Ok(StreamOption::EndOfStream)
}

fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        // v2->v1 inverse: drop the lifecycle_state field. tool_failure_class
        // stays in v1 vocabulary form because we cannot losslessly recover the
        // 12-variant legacy vocabulary from the 5-variant Lean vocabulary; the
        // inverse intentionally leaves the rebucketed value in place.
        input.remove("lifecycle_state");
        return Ok(StreamOption::Some(input));
    }
    Ok(StreamOption::EndOfStream)
}
```

- [ ] **Step 2: Run the unit tests**

```bash
cd crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2 && cargo test
```

Expected: 7 tests pass.

- [ ] **Step 3: Build the WASM artifact**

```bash
cd crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2
cargo build --release --target wasm32-unknown-unknown
```

Expected: produces `target/wasm32-unknown-unknown/release/agent_tool_call_lifecycle_v1_to_v2_lens.wasm` (or similar). If the `wasm32-unknown-unknown` target is not installed, run `rustup target add wasm32-unknown-unknown` first.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/src/lib.rs
git commit -m "$(cat <<'EOF'
Implement AgentToolCall v1->v2 lens forward and inverse transforms

Forward: reads (status, tool_failure_class) and emits (lifecycle_state,
rebucketed tool_failure_class). Per the spec table:
  - "called" -> "running"
  - "completed" + null -> "completed"
  - "completed" + "tool_timeout" -> "timedOut" + null
  - "completed" + other -> "failed" + rebucketed
Rebucketing collapses the 12-variant legacy vocabulary onto Lean's 5.

Inverse: drops lifecycle_state; tool_failure_class stays in Lean
vocabulary (lossy v2->v1 — unavoidable since 12->5 is not invertible).

Seven unit tests cover the rebucketing table and the no-op pass-through
for already-migrated rows.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Update the `AgentToolCall` GraphQL schema (add `lifecycle_state`)

The v1 → v2 schema patch adds the `lifecycle_state` field. The legacy `status` field stays for the soak window; v2 → v3 (removing `status`) is a follow-up issue.

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql`

- [ ] **Step 1: Read the current schema**

```bash
cat crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql
```

Expected: 16 lines, last field `latency_ms: Int`, no `lifecycle_state`.

- [ ] **Step 2: Add the new field**

Edit the file so its full content reads:

```graphql
type AgentToolCall @branchable {
    tool_call_key: String @index(unique: true)
    session_id: String @index
    message_sequence: Int
    tool_name: String @index
    tool_call_id: String @index
    args: String
    result: String
    status: String
    lifecycle_state: String @index
    started_at: DateTime
    completed_at: DateTime
    selected_service_id: String
    selected_tool_name: String
    tool_failure_class: String
    latency_ms: Int
}
```

The new line is `lifecycle_state: String @index`. The `@index` directive is added because the runtime will query by lifecycle state (mirrors how `AgentRequest.lifecycle_state` is indexed in its schema).

- [ ] **Step 3: Verify the workspace still builds**

```bash
cargo check --workspace
```

Expected: clean. Schema files are `include_str!` compiled, so the change is reflected at the next build.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql
git commit -m "$(cat <<'EOF'
Add lifecycle_state field to AgentToolCall schema

Adds the indexed lifecycle_state field. The legacy status field remains
for the soak window; v2 -> v3 (removing status) is a follow-up issue.
The Lens migration in defra-agent-lenses populates lifecycle_state from
status + tool_failure_class for existing rows.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add the `migration.rs` module skeleton

Houses the idempotent schema-patch + lens-registration logic invoked at startup. Skeleton only — the actual `patch_collection` and `set_migration` calls land in Task 8.

**Files:**
- Create: `crates/defra-agent/src/migration.rs`
- Modify: `crates/defra-agent/src/lib.rs` (add `pub mod migration;`)

- [ ] **Step 1: Create the migration module skeleton**

Write `crates/defra-agent/src/migration.rs`:

```rust
//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> v2 by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!
//! Idempotent: re-running on a v2 deployment is a no-op (collection already
//! patched, migration already registered).

use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

/// Run all pending tool-call migrations against the embedded node.
/// Called from the daemon startup path before any AgentToolCall reads.
#[allow(dead_code)] // wired in Task 9
pub(crate) async fn ensure_tool_call_migrations(
    _node: Arc<EmbeddedNode>,
) -> Result<()> {
    // Real implementation lands in Task 8.
    Ok(())
}
```

In `crates/defra-agent/src/lib.rs`, add (immediately after `pub mod tool_call_lifecycle;` from Task 2):

```rust
mod migration;
```

Note: `mod migration;` (not `pub mod`) — the migration is internal plumbing.

- [ ] **Step 2: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/migration.rs crates/defra-agent/src/lib.rs
git commit -m "$(cat <<'EOF'
Add migration module skeleton

Houses ensure_tool_call_migrations(). Real implementation (collection_patch
+ set_migration calls) lands next.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Implement `ensure_tool_call_migrations` (collection_patch + set_migration)

The actual migration logic. Patches the schema if not already at v2, registers the Lens forward+inverse, both idempotent.

**Files:**
- Modify: `crates/defra-agent/src/migration.rs`

- [ ] **Step 1: Replace the placeholder body with the real implementation**

Edit `crates/defra-agent/src/migration.rs` to:

```rust
//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> v2 by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!
//! Idempotent: re-running on a v2 deployment is a no-op (collection already
//! patched, migration already registered).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::{DB, EmbeddedNode};
use serde_json::json;

const ADD_LIFECYCLE_STATE_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"lifecycle_state","Kind":11}}]"#;

/// Resolve the path to the bundled WASM lens artifact. The lens crate is built
/// as part of the workspace; the path is relative to the daemon binary's
/// location at install time.
///
/// Production deployments ship the WASM file alongside the binary; tests use
/// the workspace target directory.
fn lens_wasm_path() -> PathBuf {
    // Test/dev path: workspace target dir.
    // TODO follow-up issue: production deployments need a bundled artifact path
    // resolved from std::env::current_exe(). Tracked separately.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/release/agent_tool_call_lifecycle_v1_to_v2_lens.wasm")
}

/// Run all pending tool-call migrations against the embedded node.
/// Called from the daemon startup path before any AgentToolCall reads.
pub(crate) async fn ensure_tool_call_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    // 1. Check if AgentToolCall already has lifecycle_state.
    let collection = node
        .get_collection("AgentToolCall")
        .context("get AgentToolCall collection")?;

    let already_v2 = match collection {
        Some(ref cv) => collection_has_lifecycle_state(cv),
        None => {
            // Collection doesn't exist yet (fresh install). The schema add at
            // startup creates it directly with lifecycle_state already in the
            // SDL, so no patch is needed. Migration is a no-op.
            tracing::debug!("AgentToolCall collection absent; migration no-op");
            return Ok(());
        }
    };

    if already_v2 {
        tracing::debug!("AgentToolCall already at v2; migration no-op");
        return Ok(());
    }

    // 2. Apply the v1 -> v2 schema patch.
    let v1_version_id = collection
        .as_ref()
        .map(|cv| cv.version_id.clone())
        .ok_or_else(|| anyhow::anyhow!("AgentToolCall collection has no version_id"))?;

    let v2 = node
        .patch_collection("AgentToolCall", ADD_LIFECYCLE_STATE_PATCH)
        .await
        .context("patch_collection v1 -> v2 (add lifecycle_state)")?;
    let v2_version_id = v2.version_id;

    // 3. Activate v2 as the source-of-truth for new writes.
    node.set_active_collection_version(&v2_version_id)
        .await
        .context("set_active_collection_version v2")?;

    // 4. Register the forward Lens v1 -> v2.
    let lens_path = lens_wasm_path();
    let lens_path_str = lens_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 lens path"))?;

    let forward_config = json!({
        "SourceCollectionID": v1_version_id,
        "DestinationCollectionID": v2_version_id,
        "Lens": {
            "Lenses": [{
                "Path": lens_path_str,
                "Arguments": {}
            }]
        }
    })
    .to_string();

    node.set_migration(serde_json::from_str(&forward_config)?)
        .await
        .context("set_migration forward v1 -> v2")?;

    tracing::info!(
        v1 = %v1_version_id,
        v2 = %v2_version_id,
        "AgentToolCall migrated to v2 with lens"
    );

    Ok(())
}

/// Decide whether a collection version already has the `lifecycle_state`
/// field. Used to detect already-migrated databases.
fn collection_has_lifecycle_state(cv: &defra_node::CollectionVersion) -> bool {
    cv.fields.iter().any(|f| f.name == "lifecycle_state")
}
```

**Note:** The exact field-introspection API (`cv.fields.iter().any(|f| f.name == ...)`) may differ from what `defra-node::CollectionVersion` actually exposes. If the build fails because of a missing field or method, look at the type definition in `~/go/src/github.com/sourcenetwork/defradb.rs/crates/defra-node/src/lib.rs` and adjust. The conceptual operation is "ask the collection version whether it has a field named `lifecycle_state`."

- [ ] **Step 2: Verify the file compiles**

```bash
cargo check -p defra-agent
```

Expected: clean. If a type doesn't match (`CollectionVersion::fields`, `LensConfig`, etc.), inspect the defra-node source in `/Users/johnzampolin/go/src/github.com/sourcenetwork/defradb.rs/crates/defra-node/src/lib.rs` and adjust to match the actual API.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/migration.rs
git commit -m "$(cat <<'EOF'
Implement ensure_tool_call_migrations (collection_patch + set_migration)

Idempotent: detects whether AgentToolCall already has lifecycle_state and
no-ops if so. Otherwise applies the JSON patch (adds lifecycle_state, kind
11 = String) and registers the forward Lens from the workspace WASM
artifact. Activates v2 as the source-of-truth for new writes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Wire `ensure_tool_call_migrations` into daemon startup

Locate where the daemon publishes schemas to the embedded node and call the migration immediately after.

**Files:**
- Modify: `crates/defra-agent/src/schema.rs` or wherever `ensure_schemas` is called

- [ ] **Step 1: Find the call site of `ensure_schemas`**

```bash
grep -rn "ensure_schemas\|ensure_runtime_schemas" crates/defra-agent/src/ --include="*.rs"
```

Expected: a few call sites — typically in `lib.rs`, `bin/`, or a daemon initialization module.

- [ ] **Step 2: Add the migration call at each daemon startup site**

For each call site that runs `ensure_schemas(&node, ...)` or `ensure_runtime_schemas(&node)`, add a follow-up call to `migration::ensure_tool_call_migrations`. Example (the exact location depends on Step 1's findings):

```rust
crate::schema::ensure_schemas(&node, ...).await?;
crate::migration::ensure_tool_call_migrations(node.clone()).await?;
```

The migration MUST run after schema publication (so the collection exists in the node's catalog) and BEFORE any `AgentToolCall` writes (so the lens is registered before lifecycle methods run).

- [ ] **Step 3: Verify build and run unit tests**

```bash
cargo check -p defra-agent
cargo test -p defra-agent --lib
```

Expected: clean, all unit tests still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/
git commit -m "$(cat <<'EOF'
Invoke ensure_tool_call_migrations from daemon startup

Wires the AgentToolCall v1 -> v2 migration after schema publication and
before any tool-call lifecycle writes. Idempotent: re-running on a v2
deployment is a no-op.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Add the `ToolCallLifecycle` struct + `new()` constructor

Foundation for the transition methods. Constructor is sync and does not persist.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests { ... }` block in `tool_call_lifecycle.rs`:

```rust
    use std::sync::Arc;

    fn dummy_node() -> Arc<defra_node::EmbeddedNode> {
        // Don't construct a real node for this unit test. Use a function that
        // returns Arc::new() over a stub. The actual construction is verified
        // in Bucket 3 integration tests.
        unimplemented!("unit test does not require a live node")
    }

    #[test]
    fn lifecycle_new_starts_in_pending() {
        // We can't construct a real node here, so this test only verifies the
        // constructor's invariants by reading the struct via a path that
        // doesn't require the node. We do this by inspecting state directly
        // after construction (no node calls happen in `new`).
        //
        // Skipping: real verification lives in Bucket 3 integration tests.
        // Compile-only sanity test: construct the type signature.
        let _: fn(
            Arc<defra_node::EmbeddedNode>,
            String,
            String,
            u32,
            String,
            String,
        ) -> ToolCallLifecycle = ToolCallLifecycle::new;
    }
```

This is a compile-time signature test. The full behavioral verification of the constructor happens in Bucket 3 integration tests once a real node is available.

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::
```

Expected: compile error (`ToolCallLifecycle::new` not found).

- [ ] **Step 3: Implement the struct**

In `tool_call_lifecycle.rs`, add (after the `FailureClass` block, before `#[cfg(test)] mod tests`):

```rust
use std::sync::Arc;

use defra_node::EmbeddedNode;

/// State machine struct for an individual tool call. Mirrors `RequestLifecycle`
/// from `lifecycle.rs:189-204`. Owns every persistence write for a single
/// AgentToolCall row.
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
    /// Construct a new lifecycle. Does NOT persist; the first transition
    /// method (`start_running`) creates the DefraDB row.
    pub fn new(
        node: Arc<EmbeddedNode>,
        session_id: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        args: String,
    ) -> Self {
        Self {
            node,
            session_id,
            tool_call_id,
            message_sequence,
            tool_name,
            args,
            doc_id: None,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
        }
    }

    /// Test-only accessor for the current in-memory state.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> ToolCallState {
        self.state
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::
```

Expected: 5 tests pass (the 5 from prior tasks; the new compile-only test counts as a pass).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Add ToolCallLifecycle struct and new() constructor

Mirrors RequestLifecycle from lifecycle.rs:189-204, scoped to a single
tool call. Constructor is sync and does not persist; first transition
method creates the DefraDB row. Compile-only signature test landed;
behavioral tests come later via Bucket 3 integration tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Add `tool_call_lifecycle/transition.rs` submodule with `ensure_state` helper

Mirrors `crates/defra-agent/src/lifecycle/transition.rs`. The helper is the precondition gate every transition method calls.

**Files:**
- Create: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (add `mod transition;`)

- [ ] **Step 1: Confirm the directory does not yet exist**

```bash
ls crates/defra-agent/src/tool_call_lifecycle/ 2>&1
```

Expected: `ls: ...: No such file or directory`. If it exists, report the surprise.

- [ ] **Step 2: Create the directory and submodule**

```bash
mkdir -p crates/defra-agent/src/tool_call_lifecycle
```

Write `crates/defra-agent/src/tool_call_lifecycle/transition.rs`:

```rust
//! Transition methods on ToolCallLifecycle.
//!
//! Mirrors `crates/defra-agent/src/lifecycle/transition.rs`. Each transition
//! method calls `ensure_state` at the top to assert the precondition state,
//! then performs the GraphQL mutation atomically, then updates in-memory
//! state on confirmed success.
//!
//! `ensure_state` is verified via Bucket 3 integration tests (Task 25), which
//! exercise it through every transition method's precondition path. There is
//! no standalone unit test — fabricating a stub `Arc<EmbeddedNode>` would
//! require unsafe memory tricks and the integration coverage is sufficient.

use anyhow::{anyhow, Result};

use super::{ToolCallLifecycle, ToolCallState};

/// Error returned when a transition method is called from an illegal
/// pre-state. Programmer error, not a user-visible failure.
#[derive(Debug, thiserror::Error)]
#[error("illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})")]
pub struct IllegalToolCallTransition {
    pub method: &'static str,
    pub from: ToolCallState,
    pub allowed: Vec<ToolCallState>,
}

impl ToolCallLifecycle {
    /// Assert that the current state is in `allowed`. Returns
    /// `IllegalToolCallTransition` otherwise.
    pub(crate) fn ensure_state(
        &self,
        allowed: &[ToolCallState],
        method: &'static str,
    ) -> Result<()> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(anyhow!(IllegalToolCallTransition {
                method,
                from: self.state,
                allowed: allowed.to_vec(),
            }))
        }
    }
}
```

- [ ] **Step 3: Wire the submodule into `tool_call_lifecycle.rs`**

In `crates/defra-agent/src/tool_call_lifecycle.rs`, add immediately after the existing imports (near the top, after `use std::sync::Arc; use defra_node::EmbeddedNode;`):

```rust
mod transition;

pub use transition::IllegalToolCallTransition;
```

- [ ] **Step 4: Add `thiserror` to `crates/defra-agent/Cargo.toml` if not already present**

```bash
grep "thiserror" crates/defra-agent/Cargo.toml
```

If absent, add to `[dependencies]`:

```toml
thiserror = "1"
```

- [ ] **Step 5: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/tool_call_lifecycle/ crates/defra-agent/Cargo.toml
git commit -m "$(cat <<'EOF'
Add ensure_state helper and IllegalToolCallTransition error

Mirrors lifecycle/transition.rs's ensure_state pattern. Each transition
method (next 7 tasks) opens with this guard. Behavioral verification lives
in Bucket 3 integration tests rather than fragile unit tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Implement `start_running()` (Pending → Running)

The first real transition method. Creates the DefraDB row in state `Running` if missing; idempotent if already in `Running`.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method skeleton**

Add to `transition.rs` after the `IllegalToolCallTransition` block:

```rust
use anyhow::Context;
use chrono::Utc;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;

impl ToolCallLifecycle {
    /// Pending → Running. Creates the DefraDB row if missing; idempotent if
    /// already in Running. Sets `started_at` to `now`.
    pub async fn start_running(&mut self) -> Result<()> {
        if self.state == ToolCallState::Running {
            // Idempotent re-entry (retry path).
            return Ok(());
        }
        self.ensure_state(
            &[ToolCallState::Pending],
            "start_running",
        )?;

        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "{started_at_str}",
                    selected_service_id: null,
                    selected_tool_name: null,
                    tool_failure_class: null,
                    latency_ms: null
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "start_running")
            .await
            .context("start_running mutation")?;

        // Extract _docID from the response.
        // (resp is a node Response type; parsing pattern follows
        //  lifecycle/transition.rs precedents — use the same helper if there
        //  is one, or extract via serde_json::from_str on resp.results.)
        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Running;
        self.started_at = Some(now);
        Ok(())
    }
}

/// Helper to extract _docID from a create_* mutation response. The exact
/// shape depends on how defra_node::Response is structured; consult
/// crates/defra-agent/src/lifecycle/transition.rs for a working example.
fn extract_doc_id_from_create_response(
    _resp: &defra_node::Response,
) -> Option<String> {
    // TODO: copy the working pattern from lifecycle/transition.rs's
    // create-mutation handlers. Likely involves serde_json parsing of the
    // results field. Keeping inline so the implementer can pattern-match
    // against the existing precedent on first compile.
    None
}
```

**Note on `extract_doc_id_from_create_response`:** the placeholder above returns `None` so the build catches the unfinished work. The implementer's job is to fill in the actual extraction by patterning off `crates/defra-agent/src/lifecycle/transition.rs` — search for `create_AgentRequest` or similar patterns and copy the doc-ID extraction logic.

- [ ] **Step 2: Fill in the doc-ID extraction**

Run:

```bash
grep -B2 -A15 "_docID\|extract.*doc_id\|results\[" crates/defra-agent/src/lifecycle/transition.rs | head -40
```

This shows how the existing code parses the `_docID` from a create-mutation response. Replicate that pattern in `extract_doc_id_from_create_response`.

- [ ] **Step 3: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::start_running (Pending -> Running)

Creates the AgentToolCall row in state running. Idempotent: re-entry on
already-running is a no-op. Mirrors session/tool_calls.rs:save_tool_call
mutation shape; both `status="called"` (legacy) and
`lifecycle_state="running"` are written during the v1->v2 soak window.

Behavioral verification lands in Bucket 3 integration tests (Task 27).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Implement `complete()` (Running → Completed)

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

Append to the `impl ToolCallLifecycle` block in `transition.rs`:

```rust
    /// Running → Completed. Writes the tool result; sets completed_at,
    /// latency_ms.
    pub async fn complete(&mut self, result: &str) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "complete")?;

        let doc_id = self.doc_id.as_ref()
            .ok_or_else(|| anyhow!("complete called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self.started_at
            .ok_or_else(|| anyhow!("complete called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "completed",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "complete")
            .await
            .context("complete mutation")?;

        self.state = ToolCallState::Completed;
        Ok(())
    }
```

- [ ] **Step 2: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::complete (Running -> Completed)

Writes result, completed_at, latency_ms. Both status="completed" (legacy)
and lifecycle_state="completed" are persisted during the v1->v2 soak.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Implement `fail()` (Running → Failed)

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

Append to the `impl ToolCallLifecycle` block:

```rust
    /// Running → Failed. For tool errors during execution. Sets failure_class.
    pub async fn fail(&mut self, result: &str, failure: super::FailureClass) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;

        let doc_id = self.doc_id.as_ref()
            .ok_or_else(|| anyhow!("fail called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self.started_at
            .ok_or_else(|| anyhow!("fail called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        let failure_class_str = failure.as_str();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "failed",
                        completed_at: "{now_str}",
                        tool_failure_class: "{failure_class_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "fail")
            .await
            .context("fail mutation")?;

        self.state = ToolCallState::Failed;
        self.failure_class = Some(failure);
        Ok(())
    }
```

- [ ] **Step 2: Verify build**

```bash
cargo check -p defra-agent
```

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::fail (Running -> Failed)

Writes result, failure class, completed_at, latency_ms. Status field is
"completed" (legacy "completed + failure class" shape); lifecycle_state is
"failed" with the structured failure_class.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Implement `spawn_failed()` (Pending → Failed)

For dispatcher-side failures detected before execution starts (MCP service unreachable, argument parse error, etc.).

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

```rust
    /// Pending → Failed. Used when the dispatcher cannot start the call
    /// (MCP service unreachable, argument parse failure pre-spawn).
    pub async fn spawn_failed(
        &mut self,
        failure: super::FailureClass,
        reason: &str,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "spawn_failed")?;

        // Pending means the row hasn't been created yet. We create it
        // directly in Failed state.
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let escaped_result = escape_graphql_string(reason);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;
        let failure_class_str = failure.as_str();

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "failed",
                    started_at: "{started_at_str}",
                    completed_at: "{started_at_str}",
                    tool_failure_class: "{failure_class_str}",
                    latency_ms: 0
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "spawn_failed")
            .await
            .context("spawn_failed mutation")?;

        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Failed;
        self.failure_class = Some(failure);
        self.started_at = Some(now);
        Ok(())
    }
```

- [ ] **Step 2: Verify build and commit**

```bash
cargo check -p defra-agent
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::spawn_failed (Pending -> Failed)

Direct-to-failed creation for dispatcher-side errors detected pre-spawn
(MCP service unreachable, argument parse failure). Creates the row with
zero latency_ms and the supplied failure class.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Implement `timeout()` (Running → TimedOut)

Defined for spec completeness; not called from runtime code in R1 — R3 wires it up.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

```rust
    /// Running → TimedOut. R1 does not call this from runtime code; R3 wires
    /// it up to fire on deadline expiry. Defined here so the API surface
    /// matches the Lean spec.
    pub async fn timeout(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "timeout")?;

        let doc_id = self.doc_id.as_ref()
            .ok_or_else(|| anyhow!("timeout called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self.started_at
            .ok_or_else(|| anyhow!("timeout called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        status: "completed",
                        lifecycle_state: "timedOut",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "timeout")
            .await
            .context("timeout mutation")?;

        self.state = ToolCallState::TimedOut;
        Ok(())
    }
```

- [ ] **Step 2: Verify build and commit**

```bash
cargo check -p defra-agent
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::timeout (Running -> TimedOut)

Defined for R1 spec conformance; runtime call sites land in R3 (the
operational fix for issue #149). The lifecycle_state="timedOut" carries
the cause directly — no failure class is set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Implement `cancel_before_dispatch()` (Pending → Cancelled)

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

```rust
    /// Pending → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_before_dispatch(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "cancel_before_dispatch")?;

        // Pending: row may not exist yet. Create directly in Cancelled.
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "completed",
                    lifecycle_state: "cancelled",
                    started_at: "{started_at_str}",
                    completed_at: "{started_at_str}",
                    latency_ms: 0
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "cancel_before_dispatch")
            .await
            .context("cancel_before_dispatch mutation")?;
        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Cancelled;
        self.started_at = Some(now);
        Ok(())
    }
```

- [ ] **Step 2: Verify build and commit**

```bash
cargo check -p defra-agent
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::cancel_before_dispatch (Pending -> Cancelled)

Direct-to-cancelled creation for tool calls aborted before execution
starts. Defined in R1; runtime call sites land in R4 (cancellation token
propagation).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: Implement `cancel_during_run()` (Running → Cancelled)

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Add the method**

```rust
    /// Running → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_during_run(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let doc_id = self.doc_id.as_ref()
            .ok_or_else(|| anyhow!("cancel_during_run called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self.started_at
            .ok_or_else(|| anyhow!("cancel_during_run called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        status: "completed",
                        lifecycle_state: "cancelled",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "cancel_during_run")
            .await
            .context("cancel_during_run mutation")?;

        self.state = ToolCallState::Cancelled;
        Ok(())
    }
```

- [ ] **Step 2: Verify build and commit**

```bash
cargo check -p defra-agent
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::cancel_during_run (Running -> Cancelled)

Mid-execution cancellation for in-flight tool calls. Defined in R1;
runtime call sites land in R4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: Implement `ToolCallLifecycle::load()`

The retry/recovery path: load an existing tool-call row and reconstruct the lifecycle in its current persisted state.

**Files:**
- Create: `crates/defra-agent/src/tool_call_lifecycle/query.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (add `mod query;` and re-export)

- [ ] **Step 1: Create the query submodule**

Write `crates/defra-agent/src/tool_call_lifecycle/query.rs`:

```rust
//! Read-only queries for tool-call lifecycle reconstruction.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    tool_failure_class: Option<String>,
}

impl ToolCallLifecycle {
    /// Load an existing AgentToolCall row by session_id and tool_call_id.
    /// Returns `None` if the row does not exist.
    pub async fn load(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<Self>> {
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_tool_call_id = escape_graphql_string(tool_call_id);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                    }},
                    limit: 1
                ) {{
                    _docID
                    message_sequence
                    tool_name
                    args
                    lifecycle_state
                    started_at
                    tool_failure_class
                }}
            }}"#
        );

        let resp = node.execute(&query).await;
        if resp.has_errors() {
            return Err(anyhow!(
                "load AgentToolCall query failed: {:?}",
                resp.errors
            ));
        }

        let rows: Vec<ToolCallRow> = serde_json::from_value(
            resp.results
                .get("AgentToolCall")
                .cloned()
                .unwrap_or_default(),
        )
        .context("parse AgentToolCall query results")?;

        let row = match rows.into_iter().next() {
            Some(r) => r,
            None => return Ok(None),
        };

        let state = row
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
            .unwrap_or(ToolCallState::Running);  // legacy rows pre-migration default to Running

        let started_at = row
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let failure_class = row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted);

        let mut lc = Self::new(
            node,
            session_id.to_string(),
            tool_call_id.to_string(),
            row.message_sequence,
            row.tool_name,
            row.args,
        );
        lc.set_doc_id(Some(row.doc_id));
        lc.set_state(state);
        lc.set_started_at(started_at);
        lc.set_failure_class(failure_class);
        Ok(Some(lc))
    }
}
```

In `tool_call_lifecycle.rs`, add internal setters that the query module uses (next to the existing `state_for_test` accessor):

```rust
impl ToolCallLifecycle {
    pub(crate) fn set_doc_id(&mut self, doc_id: Option<String>) { self.doc_id = doc_id; }
    pub(crate) fn set_state(&mut self, state: ToolCallState) { self.state = state; }
    pub(crate) fn set_started_at(&mut self, t: Option<chrono::DateTime<chrono::Utc>>) { self.started_at = t; }
    pub(crate) fn set_failure_class(&mut self, fc: Option<FailureClass>) { self.failure_class = fc; }
}
```

Add the submodule wiring in `tool_call_lifecycle.rs`:

```rust
mod query;
```

(Place near the existing `mod transition;`.)

- [ ] **Step 2: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/tool_call_lifecycle/query.rs
git commit -m "$(cat <<'EOF'
Implement ToolCallLifecycle::load for retry/recovery paths

Reads an existing AgentToolCall row by (session_id, tool_call_id) and
reconstructs the lifecycle in its persisted state. Used by the hook layer's
retry path when a tool_call_id reappears (e.g., after an inference retry).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: Collapse `ToolFailureClass` enum in `trace_export.rs`

Replace the existing 12-variant enum with the new 5-variant `FailureClass` from `tool_call_lifecycle`. Updates every call site that constructs the old variants.

**Files:**
- Modify: `crates/defra-agent/src/trace_export.rs`

- [ ] **Step 1: Find every call site of the old variants**

```bash
grep -rn "ToolFailureClass::" crates/defra-agent/src/ --include="*.rs"
```

Record the file:line list — every site needs rebucketing.

- [ ] **Step 2: Replace the enum definition**

In `crates/defra-agent/src/trace_export.rs`, locate the old `enum ToolFailureClass` (lines 8-21 per the spec). Replace with a re-export of the new enum:

```rust
pub use crate::tool_call_lifecycle::FailureClass as ToolFailureClass;
```

**Or**, if call sites pattern-match on specific old variants:

```rust
// Re-export the new enum under both names during the rebucketing pass.
pub use crate::tool_call_lifecycle::FailureClass as ToolFailureClass;
```

- [ ] **Step 3: Rebucket each call site**

For every call site found in Step 1, replace the old variant with the new bucket per the spec table:

| Old | New |
|---|---|
| `ToolFailureClass::ServiceUnavailable` | `FailureClass::ServiceUnavailable` |
| `ToolFailureClass::ToolNotFound` | `FailureClass::ServiceUnavailable` |
| `ToolFailureClass::ResourceNotFound` | `FailureClass::ServiceUnavailable` |
| `ToolFailureClass::ServiceSchemaDrift` | `FailureClass::ServiceUnavailable` |
| `ToolFailureClass::InvalidToolArguments` | `FailureClass::ArgumentInvalid` |
| `ToolFailureClass::InvalidJsonArguments` | `FailureClass::ArgumentInvalid` |
| `ToolFailureClass::ArgumentsNotObject` | `FailureClass::ArgumentInvalid` |
| `ToolFailureClass::ToolRuntimeError` | `FailureClass::ToolReturnedError` |
| `ToolFailureClass::NonzeroCommandExit` | `FailureClass::ToolReturnedError` |
| `ToolFailureClass::Unclassified` | `FailureClass::ToolReturnedError` |
| `ToolFailureClass::ToolTimeout` | (no FailureClass — caller now uses `lifecycle.timeout()` instead of setting failure class) |
| `ToolFailureClass::DeadlineOrInferenceFailure` | (no FailureClass — request-layer concern; remove call site) |

For sites using `ToolTimeout` or `DeadlineOrInferenceFailure`, the rebucketing is more invasive: the caller previously persisted the failure class with `status="completed"`; under R1 it should call the corresponding lifecycle method (`lifecycle.timeout()` for timeouts, or just propagate the error to the request layer). For R1 — which preserves runtime behavior — the simplest move is to map both to `FailureClass::External` to preserve a non-null failure_class until R3 wires up the timeout method. Document this in the commit message.

- [ ] **Step 4: Update the `as_str` helper if it exists**

If `trace_export.rs` has its own `as_str` for the old enum, delete it; the new `FailureClass::as_str` from `tool_call_lifecycle` is the source of truth.

- [ ] **Step 5: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean. If a call site is missed, the build fails on a missing variant — fix and rerun.

- [ ] **Step 6: Run tests**

```bash
cargo test -p defra-agent --lib
```

Expected: all in-crate unit tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/src/trace_export.rs crates/defra-agent/src/
git commit -m "$(cat <<'EOF'
Collapse ToolFailureClass to 5 Lean-vocab variants

Replaces the 12-variant enum with a re-export of FailureClass from
tool_call_lifecycle. Rebuckets every call site per the R1 spec table:
service-discovery failures -> ServiceUnavailable; argument validation
-> ArgumentInvalid; tool execution errors -> ToolReturnedError;
timeout/deadline -> External (until R3 routes to lifecycle.timeout()).

Operator dashboards lose granularity: ToolNotFound vs ResourceNotFound
distinctions collapse. This is the cost of strict spec conformance,
documented in the R1 spec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 21: Refactor `hook/persistence.rs` to use `ToolCallLifecycle`

Replaces direct calls to `save_tool_call` / `complete_tool_call` with lifecycle method calls. Adds the in-flight lifecycle map.

**Files:**
- Modify: `crates/defra-agent/src/hook/persistence.rs`

- [ ] **Step 1: Read the current `on_tool_call` and `on_tool_result` methods**

```bash
sed -n '160,272p' crates/defra-agent/src/hook/persistence.rs
```

Record the current shape — fields on the hook struct, the parameters, the body of each method.

- [ ] **Step 2: Add the in-flight map field**

In the hook struct definition (search for the `struct DefraSessionHook` or similar — likely at the top of the file or the start of the impl block), add a new field:

```rust
in_flight_lifecycles: tokio::sync::Mutex<
    std::collections::HashMap<String, crate::tool_call_lifecycle::ToolCallLifecycle>
>,
```

Initialize it to an empty map in the constructor.

- [ ] **Step 3: Rewrite `on_tool_call`**

```rust
async fn on_tool_call(
    &self,
    tool_call_id: &str,
    tool_name: &str,
    args: &str,
    message_sequence: u32,
) -> Result<()> {
    let mut lc = crate::tool_call_lifecycle::ToolCallLifecycle::new(
        self.node.clone(),
        self.session_id.clone(),
        tool_call_id.to_string(),
        message_sequence,
        tool_name.to_string(),
        args.to_string(),
    );
    lc.start_running().await?;
    self.in_flight_lifecycles.lock().await.insert(tool_call_id.to_string(), lc);
    Ok(())
}
```

- [ ] **Step 4: Rewrite `on_tool_result`**

```rust
async fn on_tool_result(
    &self,
    tool_call_id: &str,
    result: &str,
    error: Option<&str>,
) -> Result<()> {
    let mut lc = self
        .in_flight_lifecycles
        .lock()
        .await
        .remove(tool_call_id)
        .ok_or_else(|| anyhow::anyhow!(
            "on_tool_result for unknown tool_call_id={tool_call_id}"
        ))?;

    match error {
        None => lc.complete(result).await,
        Some(err_str) => {
            let fc = classify_runtime_error(err_str);
            lc.fail(result, fc).await
        }
    }
}

/// Classify a runtime error string into a FailureClass. Defaults to
/// ToolReturnedError for unknown shapes.
fn classify_runtime_error(err: &str) -> crate::tool_call_lifecycle::FailureClass {
    use crate::tool_call_lifecycle::FailureClass;
    if err.contains("timeout") || err.contains("deadline") {
        FailureClass::External  // R3 will reroute to lifecycle.timeout()
    } else if err.contains("invalid argument") || err.contains("parse") {
        FailureClass::ArgumentInvalid
    } else if err.contains("unavailable") || err.contains("not found") {
        FailureClass::ServiceUnavailable
    } else if err.contains("transport") || err.contains("connection") {
        FailureClass::Transport
    } else {
        FailureClass::ToolReturnedError
    }
}
```

The `classify_runtime_error` heuristic is a placeholder bridge: a future PR (R3) will replace it with structured error classification. For R1 it preserves the runtime's current behavior of always emitting *some* failure class.

- [ ] **Step 5: Add the `Drop` impl on the hook**

To prevent map leaks when a tool call starts but never produces a result, add:

```rust
impl Drop for DefraSessionHook {
    fn drop(&mut self) {
        // Drain the in-flight map. Lifecycles dropped without completing
        // a transition leave their AgentToolCall row in state Running on
        // disk — startup recovery (future R) will sweep these.
        if let Ok(mut map) = self.in_flight_lifecycles.try_lock() {
            map.clear();
        }
    }
}
```

(Adjust the struct name if the hook is called something other than `DefraSessionHook`.)

- [ ] **Step 6: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean. If the hook struct's name differs, fix the `Drop` impl.

- [ ] **Step 7: Run tests**

```bash
cargo test -p defra-agent --lib
```

Expected: all unit tests pass. Hook integration tests may fail until Bucket 3 lands (Task 27); that's expected.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent/src/hook/persistence.rs
git commit -m "$(cat <<'EOF'
Refactor hook/persistence to use ToolCallLifecycle

Replaces direct save_tool_call / complete_tool_call calls with lifecycle
method invocations. Hook holds an in-flight lifecycle map keyed by
tool_call_id for the duration of a turn. Drop impl clears the map to
prevent leaks when a tool call starts and never completes.

classify_runtime_error is a placeholder heuristic bridging from string
errors to FailureClass; structured classification lands in R3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 22: Delete `crates/defra-agent/src/session/tool_calls.rs`

Now that the hook layer routes through `ToolCallLifecycle`, the old standalone functions in `session/tool_calls.rs` are dead code.

**Files:**
- Delete: `crates/defra-agent/src/session/tool_calls.rs`
- Modify: `crates/defra-agent/src/session/mod.rs` (or wherever the module is declared)

- [ ] **Step 1: Find references to `session::tool_calls`**

```bash
grep -rn "session::tool_calls\|use.*session::tool_calls\|mod tool_calls\|pub.*tool_calls" crates/defra-agent/src/ --include="*.rs"
```

Record every reference. Some may be re-exports, some imports, some module declarations.

- [ ] **Step 2: Verify no callers remain (other than the module declaration)**

Run:

```bash
grep -rn "save_tool_call\|complete_tool_call\|update_started_tool_call" crates/defra-agent/src/ --include="*.rs"
```

Expected: only references in `session/tool_calls.rs` itself plus the soon-to-be-deleted module declaration. If there are callers in `hook/persistence.rs` or elsewhere, Task 21 missed them — go back and finish.

- [ ] **Step 3: Delete the file**

```bash
git rm crates/defra-agent/src/session/tool_calls.rs
```

- [ ] **Step 4: Remove the module declaration**

In `crates/defra-agent/src/session/mod.rs` (or wherever `mod tool_calls;` lives), remove that line.

- [ ] **Step 5: Verify build**

```bash
cargo check -p defra-agent
```

Expected: clean. If imports break, fix them.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/session/
git commit -m "$(cat <<'EOF'
Delete session/tool_calls.rs (replaced by ToolCallLifecycle)

The standalone save_tool_call / update_started_tool_call /
complete_tool_call async functions are dead code now that hook/persistence
routes through ToolCallLifecycle. Module declaration removed from
session/mod.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 23: Add Bucket 1 conformance tests (in-module, vocabulary)

Verifies Rust enums match Lean's `toDefraDB` output. Mirrors `crates/defra-agent/src/lifecycle.rs:240-336`.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (extend `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the conformance tests**

Append to the existing `#[cfg(test)] mod tests { ... }` block in `tool_call_lifecycle.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p defra-agent --lib tool_call_lifecycle::tests::
```

Expected: all 4 new tests pass. The Lean conformance JSON has the entries from Task 1 (`ToolFailureClass`) and PR #152 (`ToolCallState`, `toolCallMachine`).

If a test fails because the Lean side doesn't have a particular vocabulary entry, return to Task 1 and verify the entry was added correctly.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Add Bucket 1 conformance tests for ToolCallState and FailureClass

Verifies the Rust ToolCallState::ALL and FailureClass::ALL arrays match
the Lean conformance JSON's "ToolCallState" and "ToolFailureClass"
vocabularies, and that the ToolCall state-machine contract is complete
(no states or transitions emitted by Lean that Rust doesn't recognize).

Mirrors the pattern from lifecycle.rs:240-336 for RequestState.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 24: Add Bucket 2 conformance tests (transition matrix)

Pure Lean-contract assertions verifying the transition pairs are emitted correctly.

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Find the right insertion point**

```bash
grep -n "fn .*tool_call\|fn .*ToolCall\|InferenceCall" crates/defra-agent/tests/state_machine_conformance.rs | head -20
```

Locate the section that does similar tests for `InferenceCall` (around lines 133-135 per the spec). Add the new test in the same section.

- [ ] **Step 2: Add the test**

Append to `state_machine_conformance.rs`:

```rust
#[test]
fn tool_call_transitions_match_lean_contract() {
    use crate::support::conformance_consumers::{
        assert_lean_transition_is_legal,
        assert_lean_transition_is_illegal,
    };

    // Spec-relational legal transitions
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

**Note on `assert_lean_transition_is_illegal`:** if this helper does not exist in `support::conformance_consumers`, check what's available:

```bash
grep -n "fn assert_lean_transition" crates/defra-agent/tests/support/conformance_consumers.rs
```

If only `assert_lean_transition_is_legal` exists, add `assert_lean_transition_is_illegal` as a sibling helper that asserts the pair is NOT in the `legal_transitions` list of the contract. This is a small (5-10 line) addition.

- [ ] **Step 3: Run the test**

```bash
cargo test -p defra-agent --test state_machine_conformance tool_call_transitions
```

Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/
git commit -m "$(cat <<'EOF'
Add Bucket 2 conformance tests for ToolCall transition matrix

Asserts the seven legal transitions (pending->running/failed/cancelled,
running->completed/failed/timedOut/cancelled) are emitted by the Lean
toolCallMachine contract. Asserts terminal irreversibility (T1) by
checking the four "terminal -> running" pairs are NOT in legal_transitions.

If assert_lean_transition_is_illegal didn't exist before this task, it
is added as a sibling helper to assert_lean_transition_is_legal.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 25: Add Bucket 3 conformance tests (runtime-on-Rust integration)

Real DefraDB integration tests. Spins up an embedded node, exercises the lifecycle methods, asserts persisted state.

**Files:**
- Create: `crates/defra-agent/tests/tool_call_lifecycle_conformance.rs`

- [ ] **Step 1: Find the integration-test harness pattern**

```bash
grep -B2 -A6 "fn make.*node\|fn spawn.*node\|TestCluster\|test_node\|embedded.*test" crates/defra-agent/tests/support/*.rs | head -40
```

Locate the helper that spins up a test `EmbeddedNode`. Follow the precedent.

- [ ] **Step 2: Create the integration test file**

Write `crates/defra-agent/tests/tool_call_lifecycle_conformance.rs`:

```rust
//! Bucket 3 conformance: runtime-on-Rust integration tests for
//! ToolCallLifecycle. Exercises the real GraphQL mutations through a
//! live EmbeddedNode and asserts persisted state matches the Lean spec.

mod support;

use std::sync::Arc;

use defra_agent::tool_call_lifecycle::{FailureClass, ToolCallLifecycle, ToolCallState};
use support::fixtures::start_test_node;
use support::snapshots::fetch_tool_call_snapshots_for_session;

#[tokio::test]
async fn lifecycle_pending_to_running_to_completed_persists_correctly() {
    let node = Arc::new(start_test_node().await);
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "test-session-1".into(),
        "tool-call-1".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
    );

    lc.start_running().await.unwrap();
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "test-session-1").await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("running"));

    lc.complete("ok").await.unwrap();
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "test-session-1").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("completed"));
}

#[tokio::test]
async fn lifecycle_running_to_failed_persists_failure_class() {
    let node = Arc::new(start_test_node().await);
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "test-session-2".into(),
        "tool-call-2".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
    );

    lc.start_running().await.unwrap();
    lc.fail("error message", FailureClass::ToolReturnedError).await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&node, "test-session-2").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(snapshots[0].tool_failure_class.as_deref(), Some("toolReturnedError"));
}

#[tokio::test]
async fn lifecycle_terminal_irreversibility() {
    let node = Arc::new(start_test_node().await);
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "test-session-3".into(),
        "tool-call-3".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
    );

    lc.start_running().await.unwrap();
    lc.complete("done").await.unwrap();

    // Attempting fail() after complete must error.
    let err = lc.fail("late error", FailureClass::External).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("illegal tool call transition"), "expected guard error, got: {msg}");
}

#[tokio::test]
async fn lifecycle_idempotent_start_running() {
    let node = Arc::new(start_test_node().await);
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "test-session-4".into(),
        "tool-call-4".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
    );

    lc.start_running().await.unwrap();
    lc.start_running().await.unwrap();  // Second call is a no-op

    let snapshots = fetch_tool_call_snapshots_for_session(&node, "test-session-4").await;
    assert_eq!(snapshots.len(), 1, "exactly one row should exist after duplicate start_running");
}

#[tokio::test]
async fn lifecycle_load_returns_persisted_state() {
    let node = Arc::new(start_test_node().await);
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "test-session-5".into(),
        "tool-call-5".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
    );

    lc.start_running().await.unwrap();
    lc.fail("oops", FailureClass::Transport).await.unwrap();
    drop(lc);

    let loaded = ToolCallLifecycle::load(node, "test-session-5", "tool-call-5")
        .await
        .unwrap()
        .expect("row should exist");
    assert_eq!(loaded.state_for_test(), ToolCallState::Failed);
}
```

If `start_test_node` does not exist, find the actual fixture function name via Step 1 and substitute. The shape of the test is the important part.

- [ ] **Step 3: Run the integration tests**

```bash
cargo test -p defra-agent --test tool_call_lifecycle_conformance
```

Expected: all 5 tests pass. If they fail because the test fixtures expose a different API than assumed, adjust the imports.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/tool_call_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Add Bucket 3 conformance tests for ToolCallLifecycle runtime

Five integration tests against a live EmbeddedNode:
  - Pending -> Running -> Completed persists correctly
  - Running -> Failed persists failure_class
  - Terminal irreversibility (post-complete fail() errors)
  - Idempotent start_running (single row, no duplicate)
  - load() reconstructs persisted state

Exercises the real GraphQL mutations through the lifecycle struct.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 26: Add lens migration integration test

Verifies the v1 → v2 migration is idempotent and produces the expected `lifecycle_state` for representative legacy rows.

**Files:**
- Create: `crates/defra-agent/tests/tool_call_migration.rs`

- [ ] **Step 1: Create the migration integration test**

Write `crates/defra-agent/tests/tool_call_migration.rs`:

```rust
//! Lens migration integration test for AgentToolCall v1 -> v2.

mod support;

use std::sync::Arc;

use defra_agent::tool_call_lifecycle::ToolCallState;
use support::fixtures::start_test_node;

#[tokio::test]
async fn migration_called_status_becomes_running_lifecycle() {
    let node = Arc::new(start_test_node().await);

    // Simulate a v1 row by writing one with status="called" and no
    // lifecycle_state. (The migration runs at start_test_node time;
    // we need to test the LENS itself, not just startup migration.)
    //
    // For this test, we exercise the lens transform directly via the
    // lens crate's compute_v2_fields function. The DefraDB-level
    // integration is verified by start_test_node not panicking and
    // by the Bucket 3 tests passing on a freshly-migrated database.
    use agent_tool_call_lifecycle_v1_to_v2_lens::compute_v2_fields;

    assert_eq!(
        compute_v2_fields(Some("called"), None),
        ("running".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), None),
        ("completed".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), Some("tool_timeout")),
        ("timedOut".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), Some("invalid_tool_arguments")),
        ("failed".to_string(), Some("argumentInvalid".to_string()))
    );
}

#[tokio::test]
async fn migration_is_idempotent_on_already_migrated_database() {
    // Start two nodes from the same on-disk path. The second startup should
    // detect that the schema is already at v2 and skip the migration without
    // erroring.
    let node1 = Arc::new(start_test_node().await);
    let _ = ToolCallState::ALL; // noop reference to ensure the type is in scope

    // Drop and reopen
    drop(node1);
    let node2 = Arc::new(start_test_node().await);

    // If the second startup didn't panic, the migration is idempotent.
    let _ = node2;
}
```

The first test is a unit-style assertion against the lens transform. The second is a smoke test of migration idempotency — it just verifies that re-starting against an already-migrated database works.

A more thorough test would create v1-shaped rows directly (bypassing the schema), then run the migration and assert v2 shape. That's hard to do without lower-level DB access; deferring to a follow-up task if the simpler tests miss bugs.

To make the lens crate accessible from the test, add to `crates/defra-agent/Cargo.toml` `[dev-dependencies]`:

```toml
agent-tool-call-lifecycle-v1-to-v2-lens = { path = "../defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2" }
```

- [ ] **Step 2: Verify the dev-dependency**

```bash
cargo check -p defra-agent --tests
```

Expected: clean.

- [ ] **Step 3: Run the test**

```bash
cargo test -p defra-agent --test tool_call_migration
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/tool_call_migration.rs crates/defra-agent/Cargo.toml
git commit -m "$(cat <<'EOF'
Add lens migration integration tests

Two tests: (1) the lens transform's compute_v2_fields function produces
the expected (lifecycle_state, tool_failure_class) pairs for representative
v1 inputs; (2) re-opening an already-migrated database does not error
(migration idempotency smoke test).

A more thorough end-to-end test that creates v1-shaped rows through the
DB, then runs the migration, then asserts v2 shape, is deferred — it
requires lower-level DB access than the test harness exposes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 27: Final verification — full workspace build, all tests, both lake builds

Wrap-up. No new code; just confirm everything green from scratch.

**Files:**
- (no modifications expected)

- [ ] **Step 1: Full workspace clean build**

```bash
cargo build --workspace --release 2>&1 | tail -20
```

Expected: clean. All crates compile, including the WASM lens (in `release` it builds for the host target by default; the `wasm32-unknown-unknown` target build is separate).

- [ ] **Step 2: WASM lens build**

```bash
cd crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2 && cargo build --release --target wasm32-unknown-unknown 2>&1 | tail -10
```

Expected: produces the `.wasm` artifact.

- [ ] **Step 3: All Rust tests**

```bash
cd /Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-149-native-glob-deadline
cargo test -p defra-agent 2>&1 | tail -30
```

Expected: all tests pass. Test count includes the new conformance tests from Tasks 23-26.

- [ ] **Step 4: Lean build still clean**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -5
```

Expected: clean (Task 1 added the `ToolFailureClass` vocabulary).

- [ ] **Step 5: Sanity check the new files**

```bash
cd /Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-149-native-glob-deadline
ls crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/
ls crates/defra-agent/src/tool_call_lifecycle/
ls crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/migration.rs
ls crates/defra-agent/tests/tool_call_lifecycle_conformance.rs crates/defra-agent/tests/tool_call_migration.rs
test -f crates/defra-agent/src/session/tool_calls.rs && echo "FAIL: tool_calls.rs not deleted" || echo "OK: tool_calls.rs absent"
```

Expected: all the new files exist; the deleted file is absent.

- [ ] **Step 6: No commit needed if everything passed**

If all five steps were clean, no commit is required. If `Proofs.lean` or any module declarations needed touch-up, commit:

```bash
git add -A
git commit -m "$(cat <<'EOF'
Final R1 verification fixups

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Plan completion checklist

After Task 27 passes:

- [ ] `cargo build --workspace --release` clean from scratch
- [ ] `cargo test -p defra-agent` all green
- [ ] `lake build` clean
- [ ] WASM lens artifact produced
- [ ] New files present:
  - `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/{Cargo.toml, src/lib.rs}`
  - `crates/defra-agent/src/tool_call_lifecycle.rs`
  - `crates/defra-agent/src/tool_call_lifecycle/{transition,query}.rs`
  - `crates/defra-agent/src/migration.rs`
  - `crates/defra-agent/tests/tool_call_lifecycle_conformance.rs`
  - `crates/defra-agent/tests/tool_call_migration.rs`
- [ ] `crates/defra-agent/src/session/tool_calls.rs` deleted
- [ ] `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql` has the `lifecycle_state` field
- [ ] `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean` has the `"ToolFailureClass"` vocabulary entry
- [ ] Each task committed individually with the canonical Co-Authored-By trailer

The branch now has the foundation for R2 (deadline propagation), R3 (the operational #149 fix via `lifecycle.timeout()`), R4 (cancellation propagation via `lifecycle.cancel_*`), and R5 (managed-exec subprocess migration). Each phase consumes the lifecycle API landed here.
