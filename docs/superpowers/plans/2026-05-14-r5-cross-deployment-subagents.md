# R5 Cross-Deployment Subagents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement cross-deployment subagent execution (R5 v1, trusted-fleet) per `docs/superpowers/specs/2026-05-14-r5-cross-deployment-subagents-design.md`. Agents on deployment A can spawn background subagents whose behavior lives on paired-peer deployment B; child terminals project back to A; cascade cancellations propagate via doc-mirror without ACP grants.

**Architecture:** Substrate-driven plumbing. Every cross-boundary behavior is already verified by `SubagentCompletion.tla` (#176) or `SubagentCancelPropagation.tla` (#188); R5 wires the existing R4/R6 single-deployment code paths into the cross-deployment topology by (a) reusing replicated rows as if they were local, (b) routing the cascade signal through a single-writer field on the parent `AgentToolCall`, and (c) adding one `ToolRecoveryCause` constructor for the unclaimed-spawn failure mode. Zero new Lean modules. Zero new TLA+ artifacts. Zero new bridge transitions.

**Tech Stack:** Rust (workspace, `cargo` for build/test), Lean 4 (`lake` for proofs), DefraDB (embedded node), GraphQL schemas in `crates/defra-agent-protocol/schemas/agent/`.

**Sequencing note (read before starting):** Execution of this plan is **sequenced after R6 merges** (`docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md`). R6 renames `Proofs/Subagent/` → `Proofs/Background/` and `subagent_*.rs` → `background_*.rs`, and parametrically generalizes the bridge transitions over `BackgroundedKind = Subagent | Tool`. This plan refers to files by their **post-R6 names** throughout. If R6 has not yet merged, do not start; rebase this branch onto R6's merge commit first. The rebase is mechanical because R6 already lives on this branch's parent.

---

## File Structure

The implementation touches the following files. Tasks below reference exact paths.

**Created:**
- `crates/defra-agent/tests/fixtures/r5_scenarios/happy_path.json`
- `crates/defra-agent/tests/fixtures/r5_scenarios/b_crash_mid_execution.json`
- `crates/defra-agent/tests/fixtures/r5_scenarios/a_crash_mid_wait.json`
- `crates/defra-agent/tests/fixtures/r5_scenarios/partition_during_cancel.json`
- `crates/defra-agent/tests/fixtures/r5_scenarios/multi_completion_coalesce.json`
- `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`
- `crates/defra-agent/tests/support/r5_conformance/mod.rs`
- `crates/defra-agent/tests/support/r5_conformance/scenario.rs`
- `crates/defra-agent/tests/support/r5_conformance/runner.rs`
- `crates/defra-agent/tests/support/r5_conformance/invariants.rs`

**Modified:**
- `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql` — add 4 fields
- `crates/defra-agent-protocol/schemas/agent/tool_selection.graphql` — add 1 field
- `crates/defra-agent/proofs/Proofs/Recovery/Sweeps.lean` — widen `ToolRecoveryCause`
- `crates/defra-agent/proofs/Proofs/Recovery/ContractCases.lean` — add `recoverySweepCases` entry
- `crates/defra-agent/src/tool_call_lifecycle.rs` (and module-internal files) — set new fields at spawn, fire `bridge_cancel_cascade` with cross-deployment branch
- `crates/defra-agent/src/background_tools.rs` (post-R6) — cross-deployment cascade-cancel writes the bridge field
- `crates/defra-agent/src/background_completion.rs` (post-R6) — unclaimed-spawn reconciler, cancel-ack observer
- `crates/defra-agent/src/trigger_engine/subagent_source.rs` — paired-peer DID dispatch (spawn-claim trust path); cancel-mirror observer hook (or new sibling file under `trigger_engine/`)
- `crates/defra-agent/src/runtime_snapshot.rs` (or wherever paired-peer DID set is computed) — expose paired-peer DIDs from `PeerPairingDesired` to runtime consumers
- `crates/defra-agent/src/recovery.rs` (or whichever site dispatches `ToolRecoveryCause` to `recover_all`) — add the new cause path

**Test files:**
- `crates/defra-agent/tests/r5_cross_deployment_conformance.rs` (new)
- Existing test files MAY gain regression assertions if Rust-side field projections evolve

---

## Conventions and helper commands

Throughout the plan:

- **Build the workspace:** `cargo build` (root of repo)
- **Run all tests:** `cargo test`
- **Run a single test by name:** `cargo test -p defra-agent --test <file> -- <test_name>`
- **Run Lean proofs:** `cd crates/defra-agent/proofs && lake build` (must close with no `sorry`s)
- **Run conformance tests only:** `cargo test -p defra-agent --test r5_cross_deployment_conformance`
- **Run pairing-conformance regression:** `cargo test -p defra-agent --test pairing_reconcile_conformance` (must continue to pass throughout)

Commits should be small (one task or fewer per commit). Use this commit body format:

```
<short subject>

<one paragraph why; reference task N from this plan>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

---

## Phase 0: Sanity checks before starting

### Task 0: Confirm R6 has merged and rebase

**Files:** none

- [ ] **Step 1: Verify R6's renames are present.**

Run: `ls crates/defra-agent/proofs/Proofs/Background/ crates/defra-agent/src/background_completion.rs crates/defra-agent/src/background_tools.rs`
Expected: all three paths exist. If they don't, R6 has not merged; stop and rebase this branch onto R6's merge commit before proceeding.

- [ ] **Step 2: Verify baseline tests pass.**

Run: `cargo test --workspace`
Expected: all green. If any failures, stop and triage — do not start R5 against a broken baseline.

- [ ] **Step 3: Verify Lean proofs close.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success, no `sorry`s. If broken, stop and triage.

- [ ] **Step 4: Verify conformance harness baseline.**

Run: `cargo test -p defra-agent --test pairing_reconcile_conformance`
Expected: all green.

No commit for this task.

---

## Phase 1: Schema additions

This phase adds the new persistence surfaces. The new fields are not yet read or written; later tasks wire them. This phase establishes the storage layer and ensures the rest of the system handles defaulted values for legacy rows.

### Task 1: Add new fields to `AgentToolCall` schema

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql`
- Test: `crates/defra-agent/tests/state_machine_conformance.rs` (existing test must continue to pass after schema bump)

- [ ] **Step 1: Write a regression test asserting the new fields exist on `AgentToolCall`.**

Add to `crates/defra-agent/tests/state_machine_conformance.rs`:

```rust
#[tokio::test]
async fn agent_tool_call_has_r5_cross_deployment_fields() {
    let db = crate::support::test_db("agent-tool-call-r5-fields").await;
    let response = db
        .node
        .execute(
            r#"{
                __type(name: "AgentToolCall") {
                    fields { name }
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "introspection errors: {:?}", response.errors);
    let names: std::collections::HashSet<String> = response
        .data
        .as_ref()
        .and_then(|d| d.get("__type"))
        .and_then(|t| t.get("fields"))
        .and_then(|fs| fs.as_array())
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for field in [
        "unclaimed_deadline_at",
        "cancel_cascade_intent_at",
        "cancel_pending_remote_ack",
        "stuck_since",
    ] {
        assert!(names.contains(field), "AgentToolCall missing field {field}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails.**

Run: `cargo test -p defra-agent --test state_machine_conformance -- agent_tool_call_has_r5_cross_deployment_fields`
Expected: FAIL — field assertions fail because schema does not yet expose them.

- [ ] **Step 3: Add the four new fields to the schema.**

Edit `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql` to add (after `child_request_id: String @index`):

```graphql
    unclaimed_deadline_at: DateTime
    cancel_cascade_intent_at: DateTime
    cancel_pending_remote_ack: Boolean
    stuck_since: DateTime
```

The final file should be:

```graphql
type AgentToolCall @branchable {
    tool_call_key: String @index(unique: true)
    request_id: String @index
    session_id: String @index
    message_sequence: Int
    tool_name: String @index
    tool_call_id: String @index
    args: String
    result: String
    status: String
    lifecycle_state: String @index
    started_at: DateTime
    deadline_at: DateTime
    completed_at: DateTime
    selected_service_id: String
    selected_tool_name: String
    tool_failure_class: String
    latency_ms: Int
    await_mode: String
    cancel_policy: String
    child_request_id: String @index
    unclaimed_deadline_at: DateTime
    cancel_cascade_intent_at: DateTime
    cancel_pending_remote_ack: Boolean
    stuck_since: DateTime
}
```

- [ ] **Step 4: Run the test to verify it passes.**

Run: `cargo test -p defra-agent --test state_machine_conformance -- agent_tool_call_has_r5_cross_deployment_fields`
Expected: PASS.

- [ ] **Step 5: Confirm no other tests regressed.**

Run: `cargo test --workspace`
Expected: all green. If a test fails because the schema bump changes serde defaults or row hash, fix it inline — those rows must accept the new defaulted fields without behavioral change.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
R5 schema: add cross-deployment fields to AgentToolCall

Adds unclaimed_deadline_at, cancel_cascade_intent_at,
cancel_pending_remote_ack, and stuck_since. Fields are nullable and
default to null/false on existing rows. R5 plan task 1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 2: Add `cross_deployment_spawn_timeout_seconds` to `ToolSelection`

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/tool_selection.graphql`
- Test: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Write a regression test asserting the new field exists.**

Add to `crates/defra-agent/tests/state_machine_conformance.rs`:

```rust
#[tokio::test]
async fn tool_selection_has_cross_deployment_spawn_timeout() {
    let db = crate::support::test_db("tool-selection-r5-timeout").await;
    let response = db
        .node
        .execute(
            r#"{
                __type(name: "ToolSelection") {
                    fields { name }
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "introspection errors: {:?}", response.errors);
    let names: std::collections::HashSet<String> = response
        .data
        .as_ref()
        .and_then(|d| d.get("__type"))
        .and_then(|t| t.get("fields"))
        .and_then(|fs| fs.as_array())
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.contains("cross_deployment_spawn_timeout_seconds"),
        "ToolSelection missing cross_deployment_spawn_timeout_seconds",
    );
}
```

- [ ] **Step 2: Run the test to verify it fails.**

Run: `cargo test -p defra-agent --test state_machine_conformance -- tool_selection_has_cross_deployment_spawn_timeout`
Expected: FAIL.

- [ ] **Step 3: Add the field.**

Edit `crates/defra-agent-protocol/schemas/agent/tool_selection.graphql` to add `cross_deployment_spawn_timeout_seconds: Int` after `subagent_background_enabled: Boolean`:

```graphql
type ToolSelection {
    selection_id: String @index(unique: true)
    agent_did: String @index
    display_name: String
    enable_file_tools: Boolean
    file_tools_mode: String
    file_tool_root: String
    enable_bash: Boolean
    bash_mode: String
    command_execution_policy: String
    command_allowed_argv_prefixes: [String]
    command_forbidden_argv_prefixes: [String]
    command_network_mode: String
    cli_tool_names: [String]
    enable_meta_tools: Boolean
    allowed_mcp_service_ids: [String]
    delegate_to: [String]
    subagent_targets: [String]
    subagent_spawn_enabled: Boolean
    subagent_steering_enabled: Boolean
    subagent_background_enabled: Boolean
    cross_deployment_spawn_timeout_seconds: Int
}
```

- [ ] **Step 4: Verify and commit.**

Run: `cargo test -p defra-agent --test state_machine_conformance -- tool_selection_has_cross_deployment_spawn_timeout`
Expected: PASS.

Run: `cargo test --workspace`
Expected: all green.

```bash
git add crates/defra-agent-protocol/schemas/agent/tool_selection.graphql crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
R5 schema: add cross_deployment_spawn_timeout_seconds to ToolSelection

Per-behavior override for the unclaimed-spawn timeout; global default
is 60s when the field is null. R5 plan task 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2: Lean recovery widening

This phase closes the formal proof obligation before any Rust runtime behavior reads the new fields. The widening is one enum constructor + one match-arm + one new test-vector entry.

### Task 3: Add `unclaimedCrossDeploymentSpawn` to `ToolRecoveryCause`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Recovery/Sweeps.lean`

- [ ] **Step 1: Run the proofs to confirm a green baseline.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success.

- [ ] **Step 2: Add the new constructor to the enum.**

Edit `Proofs/Recovery/Sweeps.lean`, find the `ToolRecoveryCause` enum (around line 144), add a final constructor:

```lean
inductive ToolRecoveryCause where
  | deadlineExceeded
  | parentInterrupted
  | parentTerminal
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  | unclaimedCrossDeploymentSpawn
  deriving DecidableEq, Repr
```

- [ ] **Step 3: Run `lake build` to confirm match-exhaustiveness errors fire.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: FAIL with "missing cases" / "non-exhaustive match" in `toContract`, `terminalState`, and `terminalState_terminal`.

- [ ] **Step 4: Extend the `toContract` clause.**

In the same file, add a clause to the `toContract` definition (around line 157):

```lean
def toContract : ToolRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .parentInterrupted => "parentInterrupted"
  | .parentTerminal => "parentTerminal"
  | .childCompleted => "childCompleted"
  | .childFailed => "childFailed"
  | .childDead => "childDead"
  | .childInterrupted => "childInterrupted"
  | .childSuperseded => "childSuperseded"
  | .unclaimedCrossDeploymentSpawn => "unclaimedCrossDeploymentSpawn"
```

- [ ] **Step 5: Extend the `terminalState` clause.**

In the same file, add a clause to the `terminalState` definition (around line 167):

```lean
def terminalState : ToolRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .parentInterrupted => .cancelled
  | .parentTerminal => .failed
  | .childCompleted => .completed
  | .childFailed => .failed
  | .childDead => .failed
  | .childInterrupted => .cancelled
  | .childSuperseded => .failed
  | .unclaimedCrossDeploymentSpawn => .failed
```

- [ ] **Step 6: Run `lake build` and verify proofs still close.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success, no `sorry`s. The existing `terminalState_terminal` proof should close automatically by `cases cause` exhaustion. If it doesn't, inspect the error — the proof of `isTerminal .failed` is shared with `.parentTerminal`, `.childFailed`, etc., so it should just work.

- [ ] **Step 7: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/Recovery/Sweeps.lean
git commit -m "$(cat <<'EOF'
R5 Lean: widen ToolRecoveryCause with unclaimedCrossDeploymentSpawn

Adds the unclaimed-spawn cause variant. Maps to terminal state .failed;
existing terminalState_terminal proof closes by cases exhaustion. The
existing toolCallRecoverySweep registration covers this cause already
because its stale-row predicate is on cancel_policy (cascade vs detach),
not on the cause. R5 plan task 3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 4: Add the conformance-vector entry

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Recovery/ContractCases.lean`

- [ ] **Step 1: Add a new entry to `recoverySweepCases`.**

Edit `Proofs/Recovery/ContractCases.lean`. Find `recoverySweepCases` (around line 33). Add a new entry inside the list, after the existing `tool_running_*` entries:

```lean
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_unclaimed_cross_deployment_spawn_to_failed"
      "running"
      "failed"
      "r5-cross-deployment-subagents-design"
```

(The `deadlineAuditRef` string can be replaced with a more specific reference once R5 picks an audit anchor; the placeholder above is acceptable for the merge.)

- [ ] **Step 2: Run `lake build` to verify the existing `recoverySweepCases_registered_sweeps` and `recoverySweepCases_decrease_to_zero` theorems still close.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success. Both theorems use `native_decide` which should evaluate the widened list correctly.

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/Recovery/ContractCases.lean
git commit -m "$(cat <<'EOF'
R5 Lean: register tool_running_unclaimed_cross_deployment_spawn_to_failed

Adds the conformance-vector entry that emits the new recovery case to
Rust. R5 plan task 4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3: A-side spawn changes

The R6 spawn path already writes the bridge row. R5 extends it with the new fields and adds the per-behavior timeout resolution.

### Task 5: Resolve `cross_deployment_spawn_timeout_seconds` from `ToolSelection`

**Files:**
- Modify: `crates/defra-agent/src/subagent_tools.rs` (or wherever `load_parent_subagent_authorization` lives; the loader of `ToolSelectionDocument`)

- [ ] **Step 1: Locate the `ToolSelectionDocument` loader.**

Run: `grep -rn "load_parent_subagent_authorization\|subagent_targets\|ToolSelectionDocument" crates/defra-agent/src/ | head -30`

Identify the struct/function that loads `subagent_targets`. The new field `cross_deployment_spawn_timeout_seconds` should be loaded alongside.

- [ ] **Step 2: Add the field to the loaded struct.**

Add a field `cross_deployment_spawn_timeout_seconds: Option<u32>` to the existing parent-subagent-authorization or tool-selection struct. Load it from the GraphQL query. Default to `None` when null.

Concrete code (assuming the loader is in `subagent_tools.rs` and has a struct like `ParentSubagentAuthorization`):

```rust
#[derive(Debug, Clone)]
pub struct ParentSubagentAuthorization {
    // existing fields...
    pub cross_deployment_spawn_timeout_seconds: Option<u32>,
}
```

Update the GraphQL query string that loads `ToolSelection` to include the new field. Update the deserialization to capture it.

- [ ] **Step 3: Add a helper to resolve the effective timeout.**

In the same file:

```rust
const DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS: u32 = 60;

pub fn effective_cross_deployment_spawn_timeout_seconds(
    auth: &ParentSubagentAuthorization,
) -> u32 {
    auth.cross_deployment_spawn_timeout_seconds
        .unwrap_or(DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS)
}
```

- [ ] **Step 4: Write a unit test for the resolver.**

Add to the same file's `tests` module (or a new one if none exists):

```rust
#[cfg(test)]
mod cross_deployment_timeout_tests {
    use super::*;

    #[test]
    fn override_takes_precedence() {
        let auth = ParentSubagentAuthorization {
            // ...other fields default...
            cross_deployment_spawn_timeout_seconds: Some(120),
            ..Default::default()
        };
        assert_eq!(effective_cross_deployment_spawn_timeout_seconds(&auth), 120);
    }

    #[test]
    fn default_when_none() {
        let auth = ParentSubagentAuthorization {
            cross_deployment_spawn_timeout_seconds: None,
            ..Default::default()
        };
        assert_eq!(effective_cross_deployment_spawn_timeout_seconds(&auth), 60);
    }
}
```

(If `ParentSubagentAuthorization` doesn't derive `Default`, either add it or construct the test fixtures explicitly.)

- [ ] **Step 5: Run the test and verify it passes.**

Run: `cargo test -p defra-agent cross_deployment_timeout_tests`
Expected: both tests PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/src/subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: resolve cross-deployment spawn timeout from ToolSelection

Adds cross_deployment_spawn_timeout_seconds to the parent subagent
authorization loader; resolver falls back to 60s default. R5 plan task 5.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 6: Set `unclaimed_deadline_at` on bridge spawn

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs` (or whichever module writes the new bridge `AgentToolCall` row at spawn time)

- [ ] **Step 1: Locate the bridge-spawn write site.**

Run: `grep -rn "INSERT.*AgentToolCall\|create_agent_tool_call\|create_subagent_request_with_request_id\|bridge_spawn" crates/defra-agent/src/ | head -20`

Find the function that materializes the parent `AgentToolCall` row at the moment of subagent spawn. (R6's parametric `bridge_spawn` likely lives in `tool_call_lifecycle/`.)

- [ ] **Step 2: Pass the resolved timeout into the bridge-spawn write.**

Wire `effective_cross_deployment_spawn_timeout_seconds(&auth)` through the spawn call chain. At the write site, compute:

```rust
let unclaimed_deadline_at = chrono::Utc::now()
    + chrono::Duration::seconds(timeout_secs as i64);
```

and include `unclaimed_deadline_at` as a field in the GraphQL mutation that creates the bridge row. The other three new fields (`cancel_cascade_intent_at`, `cancel_pending_remote_ack`, `stuck_since`) are left at their schema defaults (null / false / null).

- [ ] **Step 3: Write an integration test.**

Pattern after the existing fixtures in `crates/defra-agent/tests/r4_subagent_tools.rs` (look for any `#[tokio::test]` that already spawns a subagent through the hook path; reuse its fixture setup verbatim). Add a new test at the bottom of the file:

```rust
#[tokio::test]
async fn background_subagent_spawn_sets_unclaimed_deadline_at() {
    // Reuse the in-file fixture builder used by existing background-spawn
    // tests; see e.g. `background_spawn_*` tests in this file for the
    // exact constructor calls (TestDb + ToolSelection write + behavior
    // registration). Spawn via the existing background-subagent hook.

    let fixture = build_background_spawn_fixture("test-r5-unclaimed-deadline").await;
    let spawn_result = fixture.spawn_background_subagent("child-behavior", "prompt").await;
    assert!(spawn_result.is_ok(), "spawn should succeed: {:?}", spawn_result);

    let row = fixture
        .load_bridge_tool_call(spawn_result.unwrap().parent_tool_call_id())
        .await
        .expect("bridge row exists");
    let deadline = row
        .unclaimed_deadline_at
        .expect("unclaimed_deadline_at is set on every background subagent spawn");
    let now = chrono::Utc::now();
    let delta = (deadline - now).num_seconds();
    assert!(
        (50..=70).contains(&delta),
        "unclaimed_deadline_at should be ~60s out (default); got delta={delta}s",
    );
}
```

If `build_background_spawn_fixture` and `load_bridge_tool_call` are not named exactly that in the file, rename them to whatever the existing test pattern uses — the *behavior* required is the same: stand up a TestDb + ToolSelection + behavior, invoke the spawn hook, and query the bridge row by `parent_tool_call_id`.

- [ ] **Step 4: Run the test, verify FAIL, implement, verify PASS.**

Run: `cargo test -p defra-agent --test r4_subagent_tools -- background_subagent_spawn_sets_unclaimed_deadline_at`
Expected initially: FAIL (field is null because the write path doesn't set it).
After Step 2 wires the field: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs crates/defra-agent/tests/r4_subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: set unclaimed_deadline_at on background subagent spawn

Resolves the per-behavior override from ToolSelection
(cross_deployment_spawn_timeout_seconds) and writes the wall-clock
deadline on the bridge AgentToolCall row at spawn time. Single-deployment
spawns also receive the field (it harmlessly elapses when the bridge
terminalizes via the in-process completion path). R5 plan task 6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4: A-side cancel-cascade cross-deployment branch

R6's parametric `bridge_cancel_cascade` for Subagent kind sets the child's `interruptRequestedAt`. In cross-deployment, the child lives on B and A cannot write to it without an ACP grant (out of scope). R5 redirects the cascade signal through `cancel_cascade_intent_at` on A's own bridge row; B mirrors in a separate task.

### Task 7: Write the cross-deployment cascade signal on A

**Files:**
- Modify: `crates/defra-agent/src/background_tools.rs` (post-R6; the cascade-cancel call site for Subagent kind)
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (or the bridge-cancel-cascade implementation site)

- [ ] **Step 1: Locate the existing `bridge_cancel_cascade` implementation site.**

Run: `grep -rn "bridge_cancel_cascade" crates/defra-agent/src/ | head -20`

Identify where the Rust dispatcher invokes the Subagent-kind branch. Likely a `match kind { Subagent => ..., Tool => ... }` or a kind-dispatched method.

- [ ] **Step 2: Detect "is the child local or replicated?" at cancel time.**

Add a helper near the cascade-cancel call site. R5 v1 uses the `agent_did` field on the child `AgentRequest` row as the locality oracle: the child is created with `agent_did = behavior's principal DID`, which differs from A's `local_did` whenever the child lives on a paired peer. This is the trusted-fleet single-org proxy for DefraDB's per-doc identity binding (which #180 will harden later).

```rust
async fn child_request_is_locally_owned(
    node: &EmbeddedNode,
    local_did: &str,
    child_request_id: &str,
) -> Result<bool> {
    let escaped = crate::graphql::escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ agent_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest for cross-deployment cancel dispatch failed: {:?}",
            response.errors
        );
    }
    let did = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("agent_did"))
        .and_then(|v| v.as_str())
        .map(String::from);
    // Absent child request → treat as cross-deployment (the local replica
    // hasn't arrived yet; the cancel must go through the bridge field).
    Ok(did.as_deref() == Some(local_did))
}
```

**Do not introduce a new identity-binding API in this task.** When #180 adds NAC-bound identity metadata, this helper migrates to read the writer DID from doc metadata instead of `agent_did`; that change is a follow-up.

- [ ] **Step 3: Branch the Subagent-kind cascade-cancel implementation.**

In `bridge_cancel_cascade`'s Subagent-kind branch, change the logic from:

```rust
// old (single-deployment only):
write_child_interrupt_requested_at(node, child_request_id).await?;
```

to:

```rust
// new (kind-dispatched on locality):
if child_request_is_locally_owned(node, local_did, child_request_id).await? {
    // Single-deployment: existing behavior.
    write_child_interrupt_requested_at(node, child_request_id).await?;
} else {
    // Cross-deployment: write the cascade-intent on the bridge row.
    write_bridge_cancel_cascade_intent(
        node,
        bridge_tool_call_id,
        chrono::Utc::now(),
    )
    .await?;
}
```

Implement `write_bridge_cancel_cascade_intent` as a single mutation that sets:

- `cancel_cascade_intent_at = now`
- `cancel_pending_remote_ack = true`

in one atomic write.

- [ ] **Step 4: Write integration tests.**

Add to `crates/defra-agent/tests/r4_subagent_tools.rs`. Pattern after existing R4 cascade-cancel tests in the file (search for `bridge_cancel_cascade` to find the closest fixture); reuse the fixture helpers and only change the `agent_did` to drive the cross-deployment branch.

```rust
#[tokio::test]
async fn cross_deployment_cancel_writes_cascade_intent_on_bridge() {
    let local_did = "did:agent:a-parent";
    let remote_did = "did:agent:b-child";
    // Fixture: spawn a background subagent and override the child's
    // agent_did to remote_did (write the row directly to simulate a
    // replicated child).
    let fixture = build_background_spawn_fixture("test-r5-xdep-cancel").await;
    let spawn = fixture.spawn_background_subagent("child-behavior", "prompt").await.expect("spawn");
    fixture.override_child_agent_did(spawn.child_request_id(), remote_did).await;

    fixture.cancel_parent_request(spawn.parent_request_id()).await.expect("cancel");

    let bridge = fixture.load_bridge_tool_call(spawn.parent_tool_call_id()).await.expect("bridge row");
    assert!(bridge.cancel_cascade_intent_at.is_some(), "cancel_cascade_intent_at must be set");
    assert_eq!(bridge.cancel_pending_remote_ack, Some(true), "cancel_pending_remote_ack must be true");

    let child = fixture.load_agent_request(spawn.child_request_id()).await.expect("child");
    assert!(
        child.interrupt_requested_at.is_none(),
        "cross-deployment branch must not write child interruptRequestedAt (no ACP grant)",
    );
}

#[tokio::test]
async fn single_deployment_cancel_still_sets_child_interrupt() {
    let fixture = build_background_spawn_fixture("test-r5-single-cancel").await;
    let spawn = fixture.spawn_background_subagent("child-behavior", "prompt").await.expect("spawn");
    // Child agent_did is the same as parent agent_did (default single-deployment path).

    fixture.cancel_parent_request(spawn.parent_request_id()).await.expect("cancel");

    let child = fixture.load_agent_request(spawn.child_request_id()).await.expect("child");
    assert!(
        child.interrupt_requested_at.is_some(),
        "single-deployment branch must still set child interruptRequestedAt",
    );
    let bridge = fixture.load_bridge_tool_call(spawn.parent_tool_call_id()).await.expect("bridge row");
    assert!(
        bridge.cancel_cascade_intent_at.is_none(),
        "single-deployment branch must NOT touch bridge cancel fields",
    );
}
```

If the fixture helpers (`override_child_agent_did`, `cancel_parent_request`, `load_agent_request`) don't exist with those exact names, add them to the test-support module in the same commit — they are thin wrappers around the same DefraDB mutations the existing R4 tests already use; the cost of adding them is one query per helper.

- [ ] **Step 5: Run tests, verify, commit.**

Run: `cargo test -p defra-agent --test r4_subagent_tools -- cross_deployment_cancel`
Expected: PASS for both.

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/background_tools.rs crates/defra-agent/tests/r4_subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: cross-deployment cascade-cancel writes bridge intent field

Branches bridge_cancel_cascade Subagent-kind on local-vs-replicated
child ownership. Single-deployment unchanged: writes child's
interruptRequestedAt. Cross-deployment: writes
cancel_cascade_intent_at + cancel_pending_remote_ack on the bridge row;
B mirrors in a separate task. R5 plan task 7.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5: A-side reconcilers

Two reconcilers live in `background_completion.rs` post-R6: the existing completion projector (unchanged) and two new ticks: the unclaimed-spawn deadline reconciler, and the cancel-ack observability reconciler.

### Task 8: Unclaimed-spawn reconciler

**Files:**
- Modify: `crates/defra-agent/src/background_completion.rs`

- [ ] **Step 1: Add a reconciler tick function.**

In `background_completion.rs`:

```rust
#[derive(Debug, Clone)]
pub enum UnclaimedSpawnReconcileOutcome {
    Failed { parent_tool_call_id: String, parent_request_id: String },
    Linked { parent_tool_call_id: String, parent_request_id: String },
    Skipped,
}

#[derive(Debug, Deserialize)]
struct UnclaimedBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    tool_call_id: String,
    child_request_id: String,
    started_at: Option<String>,
    deadline_at: Option<String>,
}

pub async fn reconcile_unclaimed_cross_deployment_spawns(
    node: Arc<EmbeddedNode>,
) -> Result<Vec<UnclaimedSpawnReconcileOutcome>> {
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now_str = crate::graphql::escape_graphql_string(&now_str);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    _and: [
                        {{ lifecycle_state: {{ _eq: "running" }} }},
                        {{ await_mode: {{ _eq: "background" }} }},
                        {{ child_request_id: {{ _ne: "" }} }},
                        {{ unclaimed_deadline_at: {{ _lt: "{now_str}" }} }}
                    ]
                }}
            ) {{
                _docID
                request_id
                tool_call_id
                child_request_id
                started_at
                deadline_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "unclaimed-spawn reconcile query failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<UnclaimedBridgeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut outcomes = Vec::with_capacity(rows.len());
    for row in rows {
        // Race: has the child finally arrived in our local replica?
        if child_request_exists_locally(node.as_ref(), &row.child_request_id).await? {
            clear_unclaimed_deadline_at(node.as_ref(), &row.doc_id).await?;
            outcomes.push(UnclaimedSpawnReconcileOutcome::Linked {
                parent_tool_call_id: row.tool_call_id.clone(),
                parent_request_id: row.request_id.clone(),
            });
            continue;
        }
        // Fire bridge_failure(ServiceUnavailable, no_peer_claimed_spawn).
        // Use the existing failure-projection helper for service-unavailable
        // failures on a running bridge tool call.
        let payload = crate::subagent_tools::subagent_tool_not_allowed_payload(
            "spawn_subagent",
            "/behavior_id",
            "<unknown>",
            "no paired peer claimed the cross-deployment spawn within unclaimed_spawn_timeout_seconds",
            &[],
        );
        crate::subagent_tools::fail_running_subagent_tool_call(
            node.as_ref(),
            &row.doc_id,
            row.started_at.as_deref(),
            row.deadline_at.as_deref(),
            &payload,
            crate::tool_call_lifecycle::FailureClass::ServiceUnavailable,
        )
        .await?;
        outcomes.push(UnclaimedSpawnReconcileOutcome::Failed {
            parent_tool_call_id: row.tool_call_id.clone(),
            parent_request_id: row.request_id.clone(),
        });
    }
    Ok(outcomes)
}

async fn child_request_exists_locally(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<bool> {
    let escaped = crate::graphql::escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "child existence probe failed: {:?}",
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .is_some_and(|rows| !rows.is_empty()))
}

async fn clear_unclaimed_deadline_at(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<()> {
    let escaped = crate::graphql::escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{ unclaimed_deadline_at: null }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "clear unclaimed_deadline_at failed: {:?}",
            response.errors
        );
    }
    Ok(())
}
```

The `fail_running_subagent_tool_call` and `subagent_tool_not_allowed_payload` symbols already exist in `crates/defra-agent/src/subagent_tools.rs` (post-R6: `background_tools.rs`); they are the same helpers used to fail unauthorized spawn attempts in `SubagentSource`. Reusing them keeps the failure shape consistent.

- [ ] **Step 2: Write tests.**

Add to `crates/defra-agent/tests/r4_subagent_tools.rs`:

```rust
#[tokio::test]
async fn unclaimed_spawn_past_deadline_fires_bridge_failure() {
    // Fixture: spawn a background subagent; do NOT materialize a child
    // AgentRequest. Advance simulated wall clock past unclaimed_deadline_at.
    // Tick the reconciler. Assert: bridge tool call row is now terminal
    // (.failed) with FailureClass::ServiceUnavailable and reason
    // "no_peer_claimed_spawn".
    todo!();
}

#[tokio::test]
async fn unclaimed_spawn_with_late_child_clears_deadline() {
    // Fixture: spawn a background subagent; deadline elapses on A.
    // Between deadline-elapse and reconciler tick, replication delivers
    // the child AgentRequest. Tick the reconciler.
    // Assert: bridge still running; unclaimed_deadline_at cleared (null).
    todo!();
}
```

- [ ] **Step 3: Wire the reconciler into the existing runtime tick.**

Find where `project_background_subagent_completion` (or the post-R6 equivalent) is invoked from. Add a parallel call into the new reconciler at the same cadence. (If there is a single supervisor loop that ticks completion projection, add the unclaimed-spawn reconciler tick there.)

- [ ] **Step 4: Run tests; verify; commit.**

```bash
git add crates/defra-agent/src/background_completion.rs crates/defra-agent/tests/r4_subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: add unclaimed-spawn reconciler on A

Steady-state reconciler ticks alongside completion projection; for each
running background bridge whose child has not replicated and whose
unclaimed_deadline_at has elapsed, fires bridge_failure with
FailureClass::ServiceUnavailable / reason no_peer_claimed_spawn. R5
plan task 8.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 9: Cancel-ack observability reconciler

**Files:**
- Modify: `crates/defra-agent/src/background_completion.rs`

- [ ] **Step 1: Implement the observability tick.**

```rust
pub const STUCK_CANCEL_THRESHOLD_SECS: i64 = 5 * 60;

#[derive(Debug, Clone)]
pub enum CancelAckOutcome {
    Acked { parent_tool_call_id: String },
    Stuck { parent_tool_call_id: String, since: chrono::DateTime<chrono::Utc> },
    Pending { parent_tool_call_id: String },
}

#[derive(Debug, Deserialize)]
struct CancelPendingBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    child_request_id: String,
    cancel_cascade_intent_at: Option<String>,
    stuck_since: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildAckProbeRow {
    state: Option<String>,
    interrupt_requested_at: Option<String>,
}

pub async fn observe_cancel_cascade_ack(
    node: Arc<EmbeddedNode>,
) -> Result<Vec<CancelAckOutcome>> {
    let now = chrono::Utc::now();
    let query = r#"{
        AgentToolCall(filter: { cancel_pending_remote_ack: { _eq: true } }) {
            _docID
            tool_call_id
            child_request_id
            cancel_cascade_intent_at
            stuck_since
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "cancel-ack observer query failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<CancelPendingBridgeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut outcomes = Vec::with_capacity(rows.len());
    for row in rows {
        let probe = load_child_ack_probe(node.as_ref(), &row.child_request_id).await?;
        let child_done = probe
            .as_ref()
            .map(|p| {
                let terminal = matches!(
                    p.state.as_deref(),
                    Some("completed" | "failed" | "dead" | "interrupted" | "superseded")
                );
                terminal || p.interrupt_requested_at.is_some()
            })
            .unwrap_or(false);

        if child_done {
            clear_cancel_pending_ack(node.as_ref(), &row.doc_id).await?;
            outcomes.push(CancelAckOutcome::Acked {
                parent_tool_call_id: row.tool_call_id.clone(),
            });
            continue;
        }

        // Determine stuck-since flip.
        let intent_at = row.cancel_cascade_intent_at.as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        let already_stuck = row.stuck_since.is_some();
        if let Some(intent_at) = intent_at {
            let age = (now - intent_at).num_seconds();
            if age >= STUCK_CANCEL_THRESHOLD_SECS && !already_stuck {
                set_stuck_since(node.as_ref(), &row.doc_id, now).await?;
                outcomes.push(CancelAckOutcome::Stuck {
                    parent_tool_call_id: row.tool_call_id.clone(),
                    since: now,
                });
                continue;
            }
        }
        outcomes.push(CancelAckOutcome::Pending {
            parent_tool_call_id: row.tool_call_id.clone(),
        });
    }
    Ok(outcomes)
}

async fn load_child_ack_probe(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<ChildAckProbeRow>> {
    let escaped = crate::graphql::escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                state
                interrupt_requested_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("child ack probe failed: {:?}", response.errors);
    }
    let rows: Vec<ChildAckProbeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Ok(rows.into_iter().next())
}

async fn clear_cancel_pending_ack(node: &EmbeddedNode, doc_id: &str) -> Result<()> {
    let escaped = crate::graphql::escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{
                    cancel_pending_remote_ack: false,
                    stuck_since: null
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("clear cancel_pending_remote_ack failed: {:?}", response.errors);
    }
    Ok(())
}

async fn set_stuck_since(
    node: &EmbeddedNode,
    doc_id: &str,
    when: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let escaped = crate::graphql::escape_graphql_string(doc_id);
    let when = when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let when = crate::graphql::escape_graphql_string(&when);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{ stuck_since: "{when}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("set stuck_since failed: {:?}", response.errors);
    }
    Ok(())
}
```

**Safety constraint:** This reconciler **never mutates the child request, the bridge's terminal state, or any other state-bearing field.** It only flips `cancel_pending_remote_ack` (true → false) and sets `stuck_since`. This is the explicit observability-not-safety boundary per spec §7.3. The Rust compiler can't enforce that constraint; the engineer must verify by inspection that no other field is touched by any code path in `observe_cancel_cascade_ack` or its helpers.

- [ ] **Step 2: Write tests.**

```rust
#[tokio::test]
async fn cancel_ack_observer_clears_flag_on_child_terminal() {
    // Fixture: bridge in Cancelled terminal with cancel_pending_remote_ack
    // = true; child AgentRequest in terminal state.
    // Tick the observer.
    // Assert: cancel_pending_remote_ack now false; bridge state unchanged.
    todo!();
}

#[tokio::test]
async fn cancel_ack_observer_flips_stuck_since_past_threshold() {
    // Fixture: bridge with cancel_cascade_intent_at = now - 6min;
    // cancel_pending_remote_ack = true; child still running.
    // Tick the observer.
    // Assert: stuck_since now set.
    todo!();
}

#[tokio::test]
async fn cancel_ack_observer_never_mutates_child() {
    // Property: tick the observer in any state; child AgentRequest is
    // never modified by this code path.
    todo!();
}
```

- [ ] **Step 3: Wire and commit.**

Wire alongside the unclaimed-spawn reconciler tick. Run all tests. Commit:

```bash
git add crates/defra-agent/src/background_completion.rs crates/defra-agent/tests/r4_subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: cancel-ack observability reconciler on A

Clears cancel_pending_remote_ack when the child request is terminal
locally; flips stuck_since past 5-min threshold. Never mutates child
state or bridge terminal state — strict observability boundary per
spec §7.3. R5 plan task 9.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6: B-side spawn-claim trust dispatch

`SubagentSource` already filters spawn intents by the local behavior snapshot. R5 adds one new branch: when the parent `AgentRequest` was written by a paired-peer DID, B skips the `load_parent_subagent_authorization` re-check (trusted-fleet trust contract).

### Task 10: Expose paired-peer DID set

**Files:**
- Modify: `crates/defra-agent/src/runtime_snapshot.rs` (or wherever `ActiveRuntimeSnapshot` is built)

- [ ] **Step 1: Add paired-peer DID set + local DID to the runtime snapshot.**

Find the `ActiveRuntimeSnapshot` struct. Add two new fields:

```rust
pub struct ActiveRuntimeSnapshot {
    // existing fields...
    /// This deployment's local principal DID. Used to detect when a
    /// replicated doc was written by a paired peer vs. this deployment.
    pub local_did: String,
    /// DIDs of paired peers as seen in PeerPairingDesired. Used by
    /// SubagentSource and the cancel mirror observer to gate on the
    /// trusted-fleet trust contract.
    pub paired_peer_dids: std::collections::HashSet<String>,
}
```

Populate when the snapshot is rebuilt — load the local DID from whatever loader produces `local_did` for the agent runtime today (search: `grep -rn "local_did\|local_principal" crates/defra-agent/src/runtime_snapshot.rs`); load paired DIDs by querying `PeerPairingDesired` and resolving each peer's `peer_id` to a DID. If the peer record stores `peer_id` rather than DID, the implementation needs a `peer_id → DID` resolver (consult `crates/defra-agent-desktop-core/src/client/core/writes.rs:213` or the peer-directory loader for the canonical resolution).

- [ ] **Step 2: Write a unit test.**

```rust
#[tokio::test]
async fn runtime_snapshot_includes_paired_peer_dids() {
    // Fixture: write PeerPairingDesired with a known peer_id mapped to
    // a known DID. Rebuild the snapshot.
    // Assert: snapshot.paired_peer_dids contains the DID.
    todo!();
}
```

- [ ] **Step 3: Run, verify, commit.**

```bash
git add crates/defra-agent/src/runtime_snapshot.rs
git commit -m "$(cat <<'EOF'
R5: expose paired-peer DID set on ActiveRuntimeSnapshot

Used by SubagentSource and the cancel-mirror observer to dispatch on
cross-deployment trust contract. R5 plan task 10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 11: Paired-peer dispatch in `SubagentSource`

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/subagent_source.rs`

- [ ] **Step 1: Add the new branch.**

In `SubagentSource::build_intent_for_tool_call_doc` (the function that loads parent context, runs authorization, and materializes the child), wrap the existing `load_parent_subagent_authorization` call:

```rust
let parent_authoring_did = self.load_parent_authoring_did(&parent_request_id).await?;
let snapshot = self.snapshot_rx.borrow().clone();

if snapshot.paired_peer_dids.contains(&parent_authoring_did) {
    // Cross-deployment spawn from a paired peer; trust the spawn
    // intent without re-validating against the parent's
    // ToolSelectionDocument (which lives on the paired peer).
    tracing::debug!(
        parent_request_id = %parent_request_id,
        parent_authoring_did = %parent_authoring_did,
        "subagent source claiming cross-deployment spawn from paired peer",
    );
} else {
    // Single-deployment spawn: existing path.
    let authorization = match load_parent_subagent_authorization(&self.node, &parent_request_id).await {
        // ... existing error handling ...
    };
    // ... existing denial check ...
}
```

`load_parent_authoring_did` reads the DefraDB doc-identity metadata for the parent `AgentRequest` row. As in Task 7 step 2, if the codebase doesn't yet expose a clean API for this, fall back to reading `agent_did` on the parent `AgentRequest` row as a proxy.

- [ ] **Step 2: Write a unit test.**

Add to `crates/defra-agent/tests/subagent_source_conformance.rs`:

```rust
#[tokio::test]
async fn cross_deployment_spawn_from_paired_peer_skips_auth_recheck() {
    // Fixture: a parent AgentRequest whose authoring DID is in the
    // paired_peer_dids set; a parent AgentToolCall row with
    // child_request_id and lifecycle_state = running. No
    // ToolSelectionDocument is present locally (it lives on the
    // paired peer).
    // Tick the SubagentSource.
    // Assert: a child AgentRequest is materialized.
    todo!();
}

#[tokio::test]
async fn single_deployment_spawn_still_runs_auth_recheck() {
    // Regression: parent DID is local; the existing authorization
    // check runs and denies/permits based on the local ToolSelection.
    todo!();
}
```

- [ ] **Step 3: Run, verify, commit.**

```bash
git add crates/defra-agent/src/trigger_engine/subagent_source.rs crates/defra-agent/tests/subagent_source_conformance.rs
git commit -m "$(cat <<'EOF'
R5: paired-peer DID dispatch in SubagentSource

Trusted-fleet contract: when the parent AgentRequest's authoring DID
is in the local paired_peer_dids set, B skips the auth re-check (the
ToolSelectionDocument lives on A and is not replicated). Single-
deployment claims unchanged. R5 plan task 11.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 7: B-side cancel mirror observer

A new B-side worker observes replicated `AgentToolCall` rows with `cancel_cascade_intent_at` set and mirrors onto the locally-owned child's `interruptRequestedAt`. Implementation can either extend `SubagentSource`'s subscription handler with an additional dispatch, or live in a sibling file under `crates/defra-agent/src/trigger_engine/`. The plan recommends the sibling-file shape for separation of concerns.

### Task 12: Cancel mirror observer

**Files:**
- Create: `crates/defra-agent/src/trigger_engine/cross_deployment_cancel_mirror.rs`
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs` (register the new worker)

- [ ] **Step 1: Create the new file.**

```rust
//! Cross-deployment cascade-cancel mirror observer.
//!
//! Watches replicated `AgentToolCall` rows for `cancel_cascade_intent_at`
//! transitions; for each row whose `child_request_id` corresponds to a
//! locally-owned `AgentRequest`, writes `interruptRequestedAt` on the
//! child to propagate the cancel signal into B's existing interrupt path.
//!
//! Trust contract: only honors writes from paired-peer DIDs (read from
//! `ActiveRuntimeSnapshot.paired_peer_dids`).

use std::sync::Arc;

use anyhow::Result;
use defra_node::{EmbeddedNode, EventName};
use tokio_util::sync::CancellationToken;
use tokio::sync::watch;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

pub struct CrossDeploymentCancelMirror {
    node: Arc<EmbeddedNode>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    subscription: Option<events::Subscription>,
    cancel: CancellationToken,
    // Track mirror dispatch idempotency by (bridge_tool_call_id,
    // cancel_cascade_intent_at). Avoid re-writing interruptRequestedAt
    // on every subscription tick.
    mirrored: std::collections::HashSet<String>,
}

impl CrossDeploymentCancelMirror {
    pub fn new(
        node: Arc<EmbeddedNode>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            node,
            snapshot_rx,
            subscription: None,
            cancel,
            mirrored: std::collections::HashSet::new(),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        self.subscription = Some(self.node.subscribe(&[EventName::Update]));
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok(()),
                msg = self.subscription.as_mut().unwrap().recv() => {
                    let Some(message) = msg else { return Ok(()) };
                    let Some(update) = message.as_update() else { continue };
                    // Filter to AgentToolCall collection.
                    let Some(name) = self.resolve_collection_name(&update.collection_id).await
                    else { continue };
                    if name != "AgentToolCall" { continue }
                    if let Err(error) = self.handle_tool_call_update(&update.doc_id).await {
                        tracing::warn!(%error, doc_id = %update.doc_id, "cancel mirror handle error");
                    }
                }
            }
        }
    }

    async fn handle_tool_call_update(&mut self, doc_id: &str) -> Result<()> {
        let row = match self.load_bridge_row(doc_id).await? {
            Some(r) => r,
            None => return Ok(()),
        };
        let intent_at = match row.cancel_cascade_intent_at.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Ok(()),
        };
        let child_request_id = match row.child_request_id.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Ok(()),
        };
        let dedupe_key = format!("{}:{}", row.tool_call_id, intent_at);
        if self.mirrored.contains(&dedupe_key) {
            return Ok(());
        }

        // Trust check: parent authoring DID is in the paired-peer set.
        let parent_did = match self.load_parent_authoring_did(&row.request_id).await? {
            Some(did) => did,
            None => return Ok(()),
        };
        let snapshot = self.snapshot_rx.borrow().clone();
        if !snapshot.paired_peer_dids.contains(&parent_did) {
            // Not from a paired peer — ignore.
            return Ok(());
        }

        // Locality check: the child must be locally owned by this peer.
        let child = match self.load_child_request(&child_request_id).await? {
            Some(c) => c,
            None => return Ok(()),
        };
        if child.agent_did.as_deref() != Some(snapshot.local_did.as_str()) {
            return Ok(());
        }

        // Idempotency: child already terminal or already interrupted.
        let already_handled = is_terminal_state(child.state.as_deref())
            || child.interrupt_requested_at.is_some();
        if already_handled {
            self.mirrored.insert(dedupe_key);
            return Ok(());
        }

        // Mirror.
        write_child_interrupt_requested_at(
            self.node.as_ref(),
            &child_request_id,
            &intent_at,
        )
        .await?;
        self.mirrored.insert(dedupe_key);
        Ok(())
    }

    async fn load_bridge_row(&self, doc_id: &str) -> Result<Option<BridgeCancelRow>> {
        let escaped = crate::graphql::escape_graphql_string(doc_id);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                    limit: 1
                ) {{
                    request_id
                    tool_call_id
                    child_request_id
                    cancel_cascade_intent_at
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("cancel mirror bridge load failed: {:?}", response.errors);
        }
        let rows: Vec<BridgeCancelRow> = response
            .data
            .as_ref()
            .and_then(|d| d.get("AgentToolCall"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().next())
    }

    async fn load_parent_authoring_did(
        &self,
        parent_request_id: &str,
    ) -> Result<Option<String>> {
        // For R5 v1, agent_did on the parent AgentRequest row is the
        // proxy for the authoring DID (see Task 7 step 2 for the
        // trusted-fleet rationale). When #180 lands, replace with the
        // DefraDB doc-identity binding read.
        let escaped = crate::graphql::escape_graphql_string(parent_request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                    limit: 1
                ) {{ agent_did }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("cancel mirror parent DID load failed: {:?}", response.errors);
        }
        Ok(response
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("agent_did"))
            .and_then(|v| v.as_str())
            .map(String::from))
    }

    async fn load_child_request(
        &self,
        child_request_id: &str,
    ) -> Result<Option<ChildRequestRow>> {
        let escaped = crate::graphql::escape_graphql_string(child_request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                    limit: 1
                ) {{
                    agent_did
                    state
                    interrupt_requested_at
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("cancel mirror child load failed: {:?}", response.errors);
        }
        let rows: Vec<ChildRequestRow> = response
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().next())
    }

    async fn resolve_collection_name(&self, collection_id: &str) -> Option<String> {
        // Mirror the same logic used by SubagentSource::resolve_collection_name.
        // The implementation lives in `crates/defra-agent/src/trigger_engine/subagent_source.rs`
        // — copy that function body verbatim or extract a shared helper into
        // `trigger_engine/mod.rs` in this same task to avoid duplication.
        let _ = collection_id;
        None
    }
}

#[derive(Debug, Deserialize)]
struct BridgeCancelRow {
    request_id: String,
    tool_call_id: String,
    child_request_id: Option<String>,
    cancel_cascade_intent_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    agent_did: Option<String>,
    state: Option<String>,
    interrupt_requested_at: Option<String>,
}

fn is_terminal_state(state: Option<&str>) -> bool {
    matches!(
        state,
        Some("completed" | "failed" | "dead" | "interrupted" | "superseded")
    )
}

async fn write_child_interrupt_requested_at(
    node: &EmbeddedNode,
    child_request_id: &str,
    when: &str,
) -> Result<()> {
    let escaped_id = crate::graphql::escape_graphql_string(child_request_id);
    let escaped_when = crate::graphql::escape_graphql_string(when);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_when}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "cancel mirror child interrupt write failed: {:?}",
            response.errors
        );
    }
    Ok(())
}
```

- [ ] **Step 2: Register the worker in the trigger engine module.**

In `crates/defra-agent/src/trigger_engine/mod.rs`, find where `SubagentSource` is constructed at startup. Add a sibling spawn for `CrossDeploymentCancelMirror::new(...).run()`. Ensure the cancellation token is shared with the existing trigger-engine workers.

- [ ] **Step 3: Write integration tests.**

Add to `crates/defra-agent/tests/r4_subagent_tools.rs` (or a new R5 file):

```rust
#[tokio::test]
async fn cancel_mirror_writes_interrupt_on_paired_peer_intent() {
    // Fixture: B has a local child AgentRequest. A's parent AgentToolCall
    // replicates with cancel_cascade_intent_at = some_timestamp and
    // authoring DID in B's paired_peer_dids.
    // Tick the cancel mirror.
    // Assert: child.interruptRequestedAt = some_timestamp.
    todo!();
}

#[tokio::test]
async fn cancel_mirror_is_idempotent() {
    // Fixture: same as above, but the child already has
    // interruptRequestedAt set.
    // Tick twice; assert no error, child unchanged.
    todo!();
}

#[tokio::test]
async fn cancel_mirror_ignores_unpaired_peer_intent() {
    // Fixture: A's parent AgentToolCall replicates with
    // cancel_cascade_intent_at set, but the authoring DID is NOT in
    // B's paired_peer_dids.
    // Tick the mirror.
    // Assert: child.interruptRequestedAt is still null.
    todo!();
}

#[tokio::test]
async fn cancel_mirror_absorbs_against_natural_terminal() {
    // Fixture: child is already in a natural terminal state
    // (.completed/.failed/.dead/.superseded). Cancel intent arrives.
    // Tick the mirror.
    // Assert: child state unchanged (natural terminal stable); no error.
    todo!();
}
```

- [ ] **Step 4: Run, verify, commit.**

```bash
git add crates/defra-agent/src/trigger_engine/cross_deployment_cancel_mirror.rs crates/defra-agent/src/trigger_engine/mod.rs crates/defra-agent/tests/r4_subagent_tools.rs
git commit -m "$(cat <<'EOF'
R5: B-side cancel mirror observer

Watches replicated AgentToolCall.cancel_cascade_intent_at and writes
interruptRequestedAt on the locally-owned child AgentRequest. Idempotent
on (bridge_tool_call_id, cancel_cascade_intent_at). Honors only paired-
peer authoring DIDs. R5 plan task 12.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 8: Conformance harness extension

R5 reuses the two-node scaffold from #107 (`tests/support/pairing_conformance/`). This phase creates a sibling `r5_conformance` scaffold that imports the existing harness's two-node setup helpers and adds the R5 action vocabulary. Each scenario lives in `tests/fixtures/r5_scenarios/` as JSON.

### Task 13: R5 scenario IR scaffold

**Files:**
- Create: `crates/defra-agent/tests/support/r5_conformance/mod.rs`
- Create: `crates/defra-agent/tests/support/r5_conformance/scenario.rs`
- Create: `crates/defra-agent/tests/support/r5_conformance/runner.rs`
- Create: `crates/defra-agent/tests/support/r5_conformance/invariants.rs`
- Modify: `crates/defra-agent/tests/support/mod.rs` (export the new module)

- [ ] **Step 1: Define the scenario IR.**

Create `crates/defra-agent/tests/support/r5_conformance/scenario.rs`:

```rust
//! R5 conformance scenario IR. Each scenario is a list of actions
//! driving the two-node R5 fixture.

use serde::Deserialize;

pub type NodeId = String;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
pub enum Action {
    /// Operator writes PeerPairingDesired on the named node, naming
    /// the peer DID and the collection set R5 cares about.
    OperatorWritePairing {
        node: NodeId,
        peer: NodeId,
        collections: Vec<String>,
    },
    /// A spawns a background subagent by writing the parent AgentToolCall.
    WriteParentToolCall {
        node: NodeId,
        parent_request_id: String,
        parent_tool_call_id: String,
        child_request_id: String,
        behavior_id: String,
        unclaimed_deadline_at: Option<String>,
    },
    /// Write or update an AgentRequest on the named node.
    WriteAgentRequest {
        node: NodeId,
        request_id: String,
        agent_did: String,
        behavior_id: String,
        state: String,
        caused_by_parent_request_id: Option<String>,
        caused_by_parent_tool_call_id: Option<String>,
    },
    /// Simulate replication of a doc from `from` to `to`.
    ReplicateDoc {
        from: NodeId,
        to: NodeId,
        collection: String,
        doc_id: String,
    },
    /// B writes its child AgentRequest terminal state and optional final
    /// AgentResponse.
    TerminalizeChildOnB {
        request_id: String,
        terminal: String,
        final_response: Option<String>,
    },
    /// Trigger bridge_cancel_cascade on A's bridge row.
    CancelParentOnA {
        parent_request_id: String,
        parent_tool_call_id: String,
    },
    /// Tick A's background_completion observer once.
    RunBackgroundCompletionObserverOnA,
    /// Tick B's cancel mirror observer once.
    RunCancelMirrorObserverOnB,
    /// Tick A's unclaimed-spawn reconciler once.
    RunUnclaimedSpawnReconcilerOnA,
    /// Tick A's cancel-ack observer once.
    RunCancelAckObserverOnA,
    /// Tick the named node's recovery sweep once.
    RunRecoverySweepOn { node: NodeId },
    /// Crash the named node (process kill simulation).
    Crash { node: NodeId },
    /// Move the named node's monotonic clock forward.
    AdvanceClockOn { node: NodeId, seconds: u64 },
    /// Wait for the harness to reach a quiescent state.
    WaitForConvergence { timeout_secs: u64 },
}
```

- [ ] **Step 2: Stub the harness runner.**

Create `crates/defra-agent/tests/support/r5_conformance/runner.rs`. Mirror `pairing_conformance/runner.rs`'s shape: a `Harness` struct holding two `HarnessNode`s, a `run` method, an observation history. Adapt the `apply_action` match to dispatch on the R5 `Action` variants.

The engineer should follow `crates/defra-agent/tests/support/pairing_conformance/runner.rs` line-for-line for the structural pattern; the only differences are the new actions and the simulated-replication mechanic (which here copies docs across the boundary on `ReplicateDoc`).

- [ ] **Step 3: Stub the invariants module.**

Create `crates/defra-agent/tests/support/r5_conformance/invariants.rs`. Each invariant from spec §10.2 becomes a function:

```rust
pub fn assert_bridge_terminal_unique(observation: &Observation) {
    // For each bridge row, assert terminal_write_count <= 1.
}

pub fn assert_projection_requires_b_durable_terminal(observation: &Observation) {
    // For each bridge whose terminalSource is ChildProjection, assert
    // the child's terminal AND final response are durable on the
    // observer's local replica.
}

// ... (the remaining 14 invariants — one per TLA+ invariant listed
//      in spec §10.2)
```

- [ ] **Step 4: Wire `mod.rs`.**

Create `crates/defra-agent/tests/support/r5_conformance/mod.rs`:

```rust
pub mod scenario;
pub mod runner;
pub mod invariants;

pub use runner::Harness;
pub use scenario::{Action, NodeId, Scenario};
```

Update `crates/defra-agent/tests/support/mod.rs` to `pub mod r5_conformance;` (alongside the existing `pub mod pairing_conformance;`).

- [ ] **Step 5: Run `cargo build` to ensure the scaffold compiles.**

Run: `cargo build --tests`
Expected: success (all the `todo!()` bodies compile).

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/tests/support/r5_conformance/ crates/defra-agent/tests/support/mod.rs
git commit -m "$(cat <<'EOF'
R5 harness: scenario IR scaffold

Adds r5_conformance/ alongside pairing_conformance/ in the test support
layer. Scenario IR enumerates the R5 action vocabulary from spec
§4.3/§10.1 verbatim. Runner and invariants are scaffolded; subsequent
tasks fill bodies. R5 plan task 13.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 14: Implement the harness runner

**Files:**
- Modify: `crates/defra-agent/tests/support/r5_conformance/runner.rs`

- [ ] **Step 1: Implement `Harness::start_two_nodes`.**

Mirror `pairing_conformance::Harness::start_two_nodes` — two `test_db` instances, an A and a B `HarnessNode`. Each node carries:
- `db: TestDb`
- `id: NodeId`
- A simulated wall clock (`std::time::Duration` offset from `Utc::now()`)
- A flag for `Crash` / restart bookkeeping

- [ ] **Step 2: Implement each `Action` variant.**

For each variant, implement the effect. Key implementations:

- `OperatorWritePairing` — write `PeerPairingDesired` on the named node (existing pattern from `pairing_conformance/runner.rs`'s `write_peer_pairing_desired`).
- `WriteParentToolCall` — write the `AgentToolCall` row on A with `await_mode = background`, `cancel_policy = cascade`, the supplied `child_request_id`, and the resolved `unclaimed_deadline_at`.
- `WriteAgentRequest` — write the `AgentRequest` row on the named node.
- `ReplicateDoc` — query the doc on `from`'s store; write it on `to`'s store (idempotent — UPSERT semantics). Simulates DefraDB replication.
- `TerminalizeChildOnB` — update the child `AgentRequest.state`; if `final_response` is provided, also write the `AgentResponse` row.
- `CancelParentOnA` — invoke `bridge_cancel_cascade` (or the in-process Rust call that triggers it) on A's bridge row.
- `RunBackgroundCompletionObserverOnA` — call `project_background_subagent_completion` (or the existing observer entry point) once with the child's request_id (or sweep all bridge rows).
- `RunCancelMirrorObserverOnB` — manually invoke the cancel-mirror handler for each replicated `AgentToolCall` doc on B (without spinning up a full subscription loop).
- `RunUnclaimedSpawnReconcilerOnA` — invoke `reconcile_unclaimed_cross_deployment_spawns` once.
- `RunCancelAckObserverOnA` — invoke `observe_cancel_cascade_ack` once.
- `RunRecoverySweepOn { node }` — invoke `ToolCallLifecycle::recover_all` (and `RequestLifecycle::recover_all` if relevant) on the named node.
- `Crash { node }` — record the crash, reset volatile in-memory state of the node fixture (DefraDB store persists).
- `AdvanceClockOn { node, seconds }` — bump the node's simulated clock offset by `seconds`. The reconcilers must read time through the harness's clock when running in test mode; the implementation may pass a `clock: Arc<dyn Clock>` parameter into each reconciler entry point, or use a thread-local override.
- `WaitForConvergence` — busy-loop tick observers until no further changes occur or `timeout_secs` elapses.

- [ ] **Step 3: Add observation snapshot capture.**

After each action, record a snapshot of the joint state: A's bridge rows, A's local replicas of B-owned rows, B's child request rows, B's responses, B's locally-replicated bridge rows. Stored in `Harness::history` for invariant evaluation.

- [ ] **Step 4: Run `cargo build --tests` to ensure compilation.**

Expected: success. Tests still don't execute scenarios yet (no scenarios in fixtures/), so `cargo test` passes (no R5 tests).

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/tests/support/r5_conformance/runner.rs
git commit -m "$(cat <<'EOF'
R5 harness: implement runner action dispatch

Implements each R5 scenario action against the two-node fixture.
Replication is simulated as a doc-copy step (matching pairing
conformance's pattern). Reconcilers and observers are invoked through
their existing entry points; harness-mode clock injection lets tests
advance deadlines without sleeping. R5 plan task 14.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 15: Implement the invariants

**Files:**
- Modify: `crates/defra-agent/tests/support/r5_conformance/invariants.rs`

- [ ] **Step 1: Implement each TLA+ invariant.**

The 16 invariants from spec §10.2 each become a function over an `Observation`. Each function asserts the property holds at the moment the observation was captured; failures `panic!` with a descriptive message including a snapshot of the offending state.

Group invariants by TLA+ source:

```rust
/// From SubagentCompletion.tla
pub mod completion {
    use super::Observation;

    pub fn bridge_terminal_unique(o: &Observation) { /* ... */ }
    pub fn projection_requires_b_durable_terminal(o: &Observation) { /* ... */ }
    pub fn projection_matches_lean_bridge_mapping(o: &Observation) { /* ... */ }
    pub fn notification_idempotent(o: &Observation) { /* ... */ }
    pub fn wakeup_coalesced(o: &Observation) { /* ... */ }
    pub fn wakeup_causal(o: &Observation) { /* ... */ }
    pub fn cancel_drain_preserves_user_pending(o: &Observation) { /* ... */ }
    pub fn parent_cancel_absorbs_late_terminal(o: &Observation) { /* ... */ }
}

/// From SubagentCancelPropagation.tla
pub mod cancel_propagation {
    use super::Observation;

    pub fn cancel_intent_durable(o: &Observation) { /* ... */ }
    pub fn cancel_handled_idempotent(o: &Observation) { /* ... */ }
    pub fn interrupt_exactly_once(o: &Observation) { /* ... */ }
    pub fn cascade_interrupts_only_running(o: &Observation) { /* ... */ }
    pub fn natural_terminal_stable_after_cancel(o: &Observation) { /* ... */ }
    pub fn interrupted_only_by_cascade(o: &Observation) { /* ... */ }
}

pub fn assert_all_safety(o: &Observation) {
    completion::bridge_terminal_unique(o);
    completion::projection_requires_b_durable_terminal(o);
    completion::projection_matches_lean_bridge_mapping(o);
    completion::notification_idempotent(o);
    completion::wakeup_coalesced(o);
    completion::wakeup_causal(o);
    completion::cancel_drain_preserves_user_pending(o);
    completion::parent_cancel_absorbs_late_terminal(o);
    cancel_propagation::cancel_intent_durable(o);
    cancel_propagation::cancel_handled_idempotent(o);
    cancel_propagation::interrupt_exactly_once(o);
    cancel_propagation::cascade_interrupts_only_running(o);
    cancel_propagation::natural_terminal_stable_after_cancel(o);
    cancel_propagation::interrupted_only_by_cascade(o);
}
```

For each invariant body, translate the TLA+ formula into a Rust assertion over the `Observation` fields. The TLA+ source (`crates/defra-agent/proofs/tla/SubagentCompletion.tla` and `SubagentCancelPropagation.tla`) is the source of truth; consult it for the exact predicate.

- [ ] **Step 2: Implement the liveness target.**

```rust
pub fn assert_liveness_after_convergence(history: &[Observation]) {
    let last = history.last().expect("non-empty history");

    // DurableTerminalSettles + LiveBridgeTerminalProjects
    for bridge in &last.a_bridge_rows {
        if bridge.child_durable_terminal.is_some() {
            assert!(
                bridge.terminal_source.is_some(),
                "DurableTerminalSettles violated for bridge {:?}",
                bridge.parent_tool_call_id,
            );
        }
    }

    // CancelDeliveryProgress + LiveCancelInterruptsOrNaturalWins
    for bridge in &last.a_bridge_rows {
        if bridge.cancel_cascade_intent_at.is_some() {
            // Child must eventually be terminalized (Interrupted or absorbed
            // against natural).
            let child = last.b_child_requests.iter()
                .find(|c| Some(&c.request_id) == bridge.child_request_id.as_ref())
                .expect("child eventually appears");
            assert!(
                child.state == "interrupted" || is_natural_terminal(&child.state),
                "LiveCancelInterruptsOrNaturalWins violated",
            );
        }
    }
}
```

- [ ] **Step 3: Run `cargo build --tests`.**

Expected: success.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/tests/support/r5_conformance/invariants.rs
git commit -m "$(cat <<'EOF'
R5 harness: implement TLA+ invariants

Each safety invariant from SubagentCompletion.tla §"Safety" and
SubagentCancelPropagation.tla §"Safety" becomes a Rust assertion;
the liveness target is checked once after WaitForConvergence. R5 plan
task 15.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 16: Scenario 1 — happy path

**Files:**
- Create: `crates/defra-agent/tests/fixtures/r5_scenarios/happy_path.json`
- Create: `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`

- [ ] **Step 1: Create the test driver.**

Create `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`:

```rust
mod support;

use std::path::PathBuf;

use support::r5_conformance::{Harness, Scenario};
use support::r5_conformance::invariants;

async fn run_scenario(filename: &str) {
    let path: PathBuf = ["tests", "fixtures", "r5_scenarios", filename]
        .iter()
        .collect();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing scenario {}", path.display()));
    let scenario: Scenario = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("invalid scenario {}: {e}", path.display()));
    let mut harness = Harness::start_two_nodes().await.expect("harness start");
    harness.run(&scenario).await.expect("scenario run");
    for snapshot in harness.observation_history() {
        invariants::assert_all_safety(&snapshot);
    }
    invariants::assert_liveness_after_convergence(&harness.observation_history());
}

#[tokio::test]
async fn r5_happy_path() {
    run_scenario("happy_path.json").await;
}
```

- [ ] **Step 2: Create the scenario JSON.**

Create `crates/defra-agent/tests/fixtures/r5_scenarios/happy_path.json`:

```json
{
  "name": "happy_path",
  "actions": [
    {
      "op": "OperatorWritePairing",
      "node": "A",
      "peer": "B",
      "collections": ["AgentRequest", "AgentResponse", "AgentToolCall", "AgentMessage"]
    },
    {
      "op": "OperatorWritePairing",
      "node": "B",
      "peer": "A",
      "collections": ["AgentRequest", "AgentResponse", "AgentToolCall", "AgentMessage"]
    },
    {
      "op": "WriteAgentRequest",
      "node": "A",
      "request_id": "parent-req-1",
      "agent_did": "did:agent:a-parent",
      "behavior_id": "parent-behavior",
      "state": "processing"
    },
    {
      "op": "WriteParentToolCall",
      "node": "A",
      "parent_request_id": "parent-req-1",
      "parent_tool_call_id": "tool-call-1",
      "child_request_id": "child-req-1",
      "behavior_id": "child-behavior",
      "unclaimed_deadline_at": null
    },
    {
      "op": "ReplicateDoc",
      "from": "A",
      "to": "B",
      "collection": "AgentRequest",
      "doc_id": "parent-req-1"
    },
    {
      "op": "ReplicateDoc",
      "from": "A",
      "to": "B",
      "collection": "AgentToolCall",
      "doc_id": "tool-call-1"
    },
    {
      "op": "WriteAgentRequest",
      "node": "B",
      "request_id": "child-req-1",
      "agent_did": "did:agent:b-child",
      "behavior_id": "child-behavior",
      "state": "processing",
      "caused_by_parent_request_id": "parent-req-1",
      "caused_by_parent_tool_call_id": "tool-call-1"
    },
    {
      "op": "ReplicateDoc",
      "from": "B",
      "to": "A",
      "collection": "AgentRequest",
      "doc_id": "child-req-1"
    },
    {
      "op": "TerminalizeChildOnB",
      "request_id": "child-req-1",
      "terminal": "completed",
      "final_response": "child completed successfully"
    },
    {
      "op": "ReplicateDoc",
      "from": "B",
      "to": "A",
      "collection": "AgentRequest",
      "doc_id": "child-req-1"
    },
    {
      "op": "ReplicateDoc",
      "from": "B",
      "to": "A",
      "collection": "AgentResponse",
      "doc_id": "child-req-1"
    },
    { "op": "RunBackgroundCompletionObserverOnA" },
    { "op": "WaitForConvergence", "timeout_secs": 10 }
  ]
}
```

- [ ] **Step 3: Run the test.**

Run: `cargo test -p defra-agent --test r5_cross_deployment_conformance -- r5_happy_path`
Expected initially: PASS (or FAIL if any of the earlier tasks left a partial implementation; iterate on those tasks until this scenario passes).

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/tests/r5_cross_deployment_conformance.rs crates/defra-agent/tests/fixtures/r5_scenarios/happy_path.json
git commit -m "$(cat <<'EOF'
R5 conformance: happy-path scenario

Exercises A spawn → replicate → B materialize → B terminalize →
replicate → A project. All 14 safety invariants hold throughout;
liveness target reached after convergence. R5 plan task 16.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 17: Scenario 2 — B-side crash mid-execution

**Files:**
- Create: `crates/defra-agent/tests/fixtures/r5_scenarios/b_crash_mid_execution.json`
- Modify: `crates/defra-agent/tests/r5_cross_deployment_conformance.rs` (add test entry)

- [ ] **Step 1: Add the test entry.**

```rust
#[tokio::test]
async fn r5_b_crash_mid_execution() {
    run_scenario("b_crash_mid_execution.json").await;
}
```

- [ ] **Step 2: Author the scenario JSON.**

Mirror the happy-path JSON, but inject a `{ "op": "Crash", "node": "B" }` between the spawn and the child-terminalize. Then add `{ "op": "RunRecoverySweepOn", "node": "B" }` to drive B's recovery — which should terminalize the in-flight child request as `.failed` per the existing #189 sweep. Then `ReplicateDoc` the terminal back to A and `RunBackgroundCompletionObserverOnA`. Verify liveness.

- [ ] **Step 3: Run, verify, commit.**

```bash
git add crates/defra-agent/tests/fixtures/r5_scenarios/b_crash_mid_execution.json crates/defra-agent/tests/r5_cross_deployment_conformance.rs
git commit -m "$(cat <<'EOF'
R5 conformance: B-side crash mid-execution

Exercises crash + recovery on B. The existing AgentRequest startup sweep
terminalizes the in-flight child; A's projection picks up the terminal
via replication and projects bridge_failure. R5 plan task 17.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 18: Scenario 3 — A-side crash mid-wait

**Files:**
- Create: `crates/defra-agent/tests/fixtures/r5_scenarios/a_crash_mid_wait.json`
- Modify: `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`

- [ ] **Step 1: Add the test entry and scenario.**

The scenario exercises **both** crash interleavings from spec §10.3 scenario 3: (a) crash before B terminalizes, (b) crash after B terminal has replicated to A but before A's observer fires. The JSON encodes both interleavings as two phases in one scenario, asserting equivalent post-states.

- [ ] **Step 2: Run, verify, commit.**

```bash
git add crates/defra-agent/tests/fixtures/r5_scenarios/a_crash_mid_wait.json crates/defra-agent/tests/r5_cross_deployment_conformance.rs
git commit -m "$(cat <<'EOF'
R5 conformance: A-side crash mid-wait

Exercises crash interleavings on A: (a) before B's terminal arrives —
re-subscription handles the future delivery; (b) after B's terminal has
replicated but before A's observer ticks — recovery sweep picks it up.
Both end equivalent. R5 plan task 18.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 19: Scenario 4 — partition during cancel

**Files:**
- Create: `crates/defra-agent/tests/fixtures/r5_scenarios/partition_during_cancel.json`
- Modify: `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`

- [ ] **Step 1: Add the test entry and scenario.**

Sequence: spawn → child materialized → cancel on A (bridge terminalizes immediately + `cancel_cascade_intent_at` set) → omit `ReplicateDoc` of the updated `AgentToolCall` → `AdvanceClockOn { node: "A", seconds: 360 }` → `RunCancelAckObserverOnA` (asserts `stuck_since` flips) → `ReplicateDoc` the updated bridge row to B → `RunCancelMirrorObserverOnB` (asserts `interruptRequestedAt` written) → B terminalizes child as `.interrupted` (or absorbs against earlier natural terminal if the scenario rolls that way) → `ReplicateDoc` back → assertion: A's `cancel_pending_remote_ack` cleared.

- [ ] **Step 2: Run, verify, commit.**

```bash
git add crates/defra-agent/tests/fixtures/r5_scenarios/partition_during_cancel.json crates/defra-agent/tests/r5_cross_deployment_conformance.rs
git commit -m "$(cat <<'EOF'
R5 conformance: partition during cancel

Exercises stuck_since flip past threshold while replication is dropped,
mirror once replication resumes, ack-cleared after child terminal
replicates back. Verifies cancel_pending_remote_ack and stuck_since
are observability-only (never block bridge state or child terminal).
R5 plan task 19.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 20: Scenario 5 — multi-completion coalesce

**Files:**
- Create: `crates/defra-agent/tests/fixtures/r5_scenarios/multi_completion_coalesce.json`
- Modify: `crates/defra-agent/tests/r5_cross_deployment_conformance.rs`

- [ ] **Step 1: Add the test entry and scenario.**

Sequence: two children spawned in the same session on A → both materialized on B → both terminalized simultaneously on B → both `AgentRequest` + `AgentResponse` docs replicate to A → tick the projector twice (once per child) → assert: two `<subagent-notification>` transcript messages exist; exactly one pending `background_completion:<session_id>` queue row exists.

- [ ] **Step 2: Run, verify, commit.**

```bash
git add crates/defra-agent/tests/fixtures/r5_scenarios/multi_completion_coalesce.json crates/defra-agent/tests/r5_cross_deployment_conformance.rs
git commit -m "$(cat <<'EOF'
R5 conformance: multi-completion coalesce

Two children terminalize on B simultaneously; A's projection emits two
transcript notifications and coalesces both wake-ups under one queue
row at (session_id, "background_completion:<session_id>"). Verifies
WakeupCoalesced from SubagentCompletion.tla. R5 plan task 20.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 9: Final validation

### Task 21: End-to-end verification

**Files:** none

- [ ] **Step 1: Run the entire test suite.**

Run: `cargo test --workspace`
Expected: all green, including the new R5 conformance tests and all pre-existing tests.

- [ ] **Step 2: Run Lean proofs.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success, no `sorry`s, no broken theorems.

- [ ] **Step 3: Run the pairing-conformance regression.**

Run: `cargo test -p defra-agent --test pairing_reconcile_conformance`
Expected: all green. This test predates R5 and must continue to pass to confirm the #107 substrate is unaffected.

- [ ] **Step 4: Manual review checklist.**

Cross-check each line of the spec §12 "Approval Checklist" against the merged code:

- Trusted-fleet trust posture? Verify §2 implementation in `subagent_source.rs` and `cross_deployment_cancel_mirror.rs` (both gated on `paired_peer_dids`).
- Spawn locus? Verify B creates the child via `SubagentSource` (unchanged path).
- Unclaimed-spawn timeout? Field present on `AgentToolCall`; resolver in `subagent_tools.rs`; reconciler in `background_completion.rs`.
- Doc-ownership discipline? Audit `git log` for any cross-peer field write — there should be none.
- One new `ToolRecoveryCause` variant? Verify `unclaimedCrossDeploymentSpawn` in `Sweeps.lean` and corresponding entry in `ContractCases.lean`.
- Five conformance scenarios? Verify all five JSON files exist and all five tests pass.

- [ ] **Step 5: Tag the design + plan delta.**

No code changes for this step. Confirm `git log --oneline | head -25` shows clean atomic commits with messages following the convention.

- [ ] **Step 6: No commit; report.**

End of plan. The branch is ready for `git push` + PR by the operator.

---

## Out of scope (do not implement in this plan)

- **Cross-deployment R6 backgrounded tools** (`background_tool` for `bash`/MCP across nodes). Future composition; not part of R5.
- **`detach` cancel policy** for cross-deployment. v1 cascade only.
- **Foreground cross-deployment subagents** (`await_mode = foreground`). Future-R5+.
- **Real-libp2p replication in the conformance harness.** Simulated `ReplicateDoc` is the v1 mechanism.
- **#180 NAC / multi-tenant.** Trusted-fleet only. When #180 lands, the trust check in Task 11 + Task 12 becomes a NAC gate; no schema changes.
- **`HostedBehavior` / `AgentBehavior` replication.** No discovery doc; operator's `subagent_targets` allowlist + `unclaimed_deadline_at` is the entire pre-flight contract.
- **A-side cancel-retry worker.** DefraDB replication is the retry; no application-layer retry loop.
- **R4c surfaces** (`list_*`, `read_subagent_transcript`, `steer_*`). Sibling brainstorm.
