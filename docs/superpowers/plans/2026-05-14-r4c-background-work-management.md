# R4c Agent-Facing Background Work Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` for this plan. Execute one task at a time with a fresh implementation subagent, then a spec-compliance reviewer, then a code-quality reviewer. Do not start a later task until the current task is reviewed, formatted, verified, and committed.

**Goal:** Implement the approved R4c design in `docs/superpowers/specs/2026-05-14-r4c-background-work-management-design.md` — five agent-facing tools (`list_subagents`, `list_background_tools`, `read_subagent_transcript`, `read_tool_output`, `steer_subagent`) plus one new Lean theorem and six conformance witnesses.

**Architecture:** Hook-intercepted Rig tools with zero new `AgentToolCall` rows. Reuses R6's `Proofs/Background/*`, R4a's `Proofs/Session/*` (`QueueSource.steering`, `pendingAfterDrain`), R4's `interrupt` transition, and #191's `Proofs/Transcript/*`. One new theorem (`steer_subagent_interrupt_preserves_link_symmetry`) discharges B5 preservation across the `interrupt+cascade+drain+append` composition.

**Tech Stack:** Rust 2024 edition, Rig (LLM tools), DefraDB (GraphQL document store), Lean 4 / Lake, the existing conformance witness pipeline.

Do not start coding until Jack approves this plan.

---

## Sequencing Prerequisite

R4c implementation **must not begin until R6 (tool backgrounding, design `docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md`) is merged to main.** R4c imports `Proofs.Background.*` (R6's rename target), writes against `background_tools.rs`/`background_completion.rs` (R6's rename targets), and reads the `<tool-completion>` payload R6's projector persists.

Confirm at kickoff:

```bash
git log --oneline main | rg -i "Proofs/Subagent -> Proofs/Background|background_tool meta-tool|tool_backgrounding"
git log --oneline main | rg -i "#191|transcript persistence model"
```

The first command must show R6's Task 0 rename commit AND at least one substantive R6 commit. The second command must show #191 merged.

If R6 has not merged, **stop and surface the blocker to Jack**. Do not attempt to land R4c against the pre-rename surface.

R4c ships as one PR with substantive commits per task. No "pure rename" no-op commit (R6 owns that).

---

## Cadence

For every task:

1. Spawn a fresh implementation subagent with the task section as its prompt.
2. Tell the worker: "You are not alone in the codebase. Do not revert edits made by others. Own only the files listed in this task unless you discover a blocker."
3. After the implementation pass, run:

```bash
cargo fmt --all
```

4. Spawn a fresh spec-compliance reviewer. Ask it to compare the diff against the approved R4c spec and this task.
5. Spawn a fresh code-quality reviewer. Ask it to review only bugs, regressions, maintainability risks, and missing tests.
6. Address reviewer findings.
7. Run the task's focused verify commands.
8. Before commit, run the broader CI commands:

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

9. Commit one task at a time.

---

## Lean Properties To Re-Verify

After every Lean task, the following must remain green:

- **B1–B7** in `Proofs/Background/Properties.lean` (R6 parametric set)
- **S1, S3, S4, S5, S6** in `Proofs/Request/Properties.lean`
- **L1, L3** bounded termination / recovery convergence
- R4a session-queue invariants (`Proofs/Session/Properties.lean`)
- #189 recovery enumeration coverage in `Proofs/Recovery/Properties.lean`
- #191 transcript pair atomicity in `Proofs/Transcript/Properties.lean`
- **New (Task 1):** `steer_subagent_interrupt_preserves_link_symmetry` in `Proofs/Background/Properties.lean`

Verify after every Lean task:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4c-lean-contract.json
```

---

## File Structure

R4c is glue and rendering. New files are minimal; most work edits the post-R6 surface.

**New files:**

- `crates/defra-agent/src/background_tools/transcript_render.rs` — pure-function transcript renderer. Unit-testable in isolation.
- `crates/defra-agent/src/background_tools/r4c_args.rs` — argument/envelope types for the five R4c tools. Keeps `background_tools.rs` itself focused on hook-interceptor wiring.
- `crates/defra-agent/tests/r4c_list_subagents.rs` — integration tests for `list_subagents`.
- `crates/defra-agent/tests/r4c_list_background_tools.rs` — integration tests for `list_background_tools`.
- `crates/defra-agent/tests/r4c_read_subagent_transcript.rs` — integration tests for `read_subagent_transcript`.
- `crates/defra-agent/tests/r4c_read_tool_output.rs` — integration tests for `read_tool_output`.
- `crates/defra-agent/tests/r4c_steer_subagent.rs` — integration tests for both `steer_subagent` modes.

**Modified files:**

- `crates/defra-agent/proofs/Proofs/Background/Transition.lean` — add a `SteerWithInterrupt` derived transition that composes existing primitives.
- `crates/defra-agent/proofs/Proofs/Background/Properties.lean` — add the new theorem.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean` — emit six R4c witness rows.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` — JSON encoders for new witness types.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Types.lean` — Lean structs for the new witness types.
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` — six new ledger rows.
- `crates/defra-agent/src/background_tools.rs` — add five new tool definitions and their hook interceptors.
- `crates/defra-agent/src/hook.rs` — register the five new interceptor entries.
- `crates/defra-agent/src/hook/persistence.rs` — branch on the five new tool names before ordinary `AgentToolCall` persistence (R4 pattern carried forward).
- `crates/defra-agent/src/tool_surface/selection.rs` — register the five new tools per the per-tool gate rules (Section "Tool Surface > Registration gates" of the spec).
- `crates/defra-agent/tests/support/conformance_consumers.rs` — Rust consumers for the new witnesses.
- `crates/defra-agent/tests/state_machine_conformance.rs` — register the new consumers and assert ledger coverage.

---

## R4c Task 0: Kickoff Gate — Confirm R6 Prerequisite

**Purpose:** Verify R6 is fully merged to main before starting. This task does not write code or commit; it is a hard gate.

**Steps:**

- [ ] **Step 1: Confirm R6 rename commit on main**

Run:

```bash
git log --oneline main | rg -i "Rename Proofs/Subagent -> Proofs/Background|pure no-op rename"
```

Expected: one or more lines showing R6's Task 0 rename commit.

- [ ] **Step 2: Confirm R6 substance landed**

Run:

```bash
git log --oneline main | rg -i "background_tool meta-tool|wait_tool|cancel_tool"
```

Expected: at least three commit lines matching R6 Tasks 7, 8, 9.

- [ ] **Step 3: Confirm rename in current branch's working tree**

Run:

```bash
ls crates/defra-agent/src/background_tools.rs crates/defra-agent/src/background_completion.rs
ls crates/defra-agent/proofs/Proofs/Background/
```

Expected: all three exist; no `subagent_tools.rs` or `subagent_completion.rs` remain.

- [ ] **Step 4: If any of the above fails, stop**

If R6 is not on main or this branch isn't rebased onto post-R6 main, report the blocker to Jack and halt. Do not proceed.

**Verify:** None. This task gates the rest.

**Commit:** None. Move to Task 1 only if all checks pass.

---

## R4c Task 1: Add Lean Theorem `steer_subagent_interrupt_preserves_link_symmetry`

**Purpose:** Ship the one new R4c theorem. Discharge B5 preservation across the `interrupt + bridge_cancel_cascade + pendingAfterDrain + appendPending` composition that `steer_subagent(interrupt=true)` uses. This makes future regressions of the composition trip `lake build` before they trip Rust.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Background/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Properties.lean`

**Steps:**

- [ ] **Step 1: Define the composed transition in `Transition.lean`**

Add (after the existing `bridge_cancel_cascade` constructor):

```lean
/-- Composed transition for steer_subagent(interrupt=true). Sequences the
    existing interrupt + bridge_cancel_cascade + pendingAfterDrain +
    appendPending primitives. Not a new transition; a witness that the
    composition is reachable from `pre` and well-formed at `post`. -/
structure SteerWithInterrupt
    (pre post : BackgroundedState)
    (childRequestId : RequestId)
    (steeringRequestId : RequestId)
    (steeringMessage : String) : Prop where
  -- 1. The child has an active running request that the interrupt targets.
  h_child_active : ∃ child : BackgroundedRow,
    child ∈ pre.parent.children ∧
    child.requestId = childRequestId ∧
    ¬ terminal child.requestState
  -- 2. The post-state reflects the interrupt + cascade + drain + append
  --    composition (witness: any execution path that fires
  --    Request.interrupt then bridge_cancel_cascade on live edges then
  --    SessionQueue.drainAutomatedWakeups on the child session then
  --    SessionQueue.appendPending with QueueSource.steering produces
  --    `post`). The transition shape lives in `Background/Executable.lean`
  --    as the canonical executor; this Prop carries the witness for the
  --    properties layer.
  h_compose : ∃ mid₁ mid₂ mid₃ : BackgroundedState,
    Request.interruptStep pre mid₁ childRequestId ∧
    BridgeCancelCascade mid₁ mid₂ childRequestId ∧
    SessionQueue.drainAutomatedWakeupsStep mid₂ mid₃
      (childSessionOf pre childRequestId) ∧
    SessionQueue.appendPendingStep mid₃ post
      { requestId := steeringRequestId
      , source := .steering
      , policy := .append
      , queueKey := none
      , queuedAfter := none
      }
```

The four helper `*Step` propositions are thin wrappers around existing
transitions; if they do not exist in `Background/Executable.lean` yet,
add minimal ones that simply assert "the existing transition fires with
the listed arguments." Do not introduce new transition logic.

- [ ] **Step 2: State the theorem in `Properties.lean`**

Add (after the existing `link_symmetry` theorem, B5):

```lean
/-- B5 invariant survives the `steer_subagent(interrupt=true)` composition.
    Discharged from existing B5 plus the invariants of the four sub-steps. -/
theorem steer_subagent_interrupt_preserves_link_symmetry
    {pre post : BackgroundedState}
    {childRequestId steeringRequestId : RequestId}
    {message : String}
    (h_step : SteerWithInterrupt pre post childRequestId steeringRequestId message)
    (h_pre  : link_symmetry pre) :
    link_symmetry post := by
  rcases h_step.h_compose with ⟨mid₁, mid₂, mid₃, h_int, h_casc, h_drain, h_app⟩
  -- Step A: interrupt preserves B5 (existing lemma on Request.interruptStep)
  have h1 : link_symmetry mid₁ := link_symmetry_of_interrupt h_pre h_int
  -- Step B: bridge_cancel_cascade preserves B5 (existing B3 corollary)
  have h2 : link_symmetry mid₂ := link_symmetry_of_cascade h1 h_casc
  -- Step C: drainAutomatedWakeups touches only queue, not bridge rows
  have h3 : link_symmetry mid₃ := link_symmetry_of_drain h2 h_drain
  -- Step D: appendPending writes a new request with caused_by_parent_request_id
  --         pointing at the steering parent; B5 lifts directly.
  exact link_symmetry_of_appendPending h3 h_app
```

If any of `link_symmetry_of_interrupt`, `link_symmetry_of_cascade`,
`link_symmetry_of_drain`, `link_symmetry_of_appendPending` do not exist,
add them as small lemmas in `Properties.lean` proven by unfolding
`link_symmetry` and case-analyzing the sub-step. Each is a few lines.

- [ ] **Step 3: Run Lean and observe the new theorem compile**

Run:

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build succeeds. The new theorem is discharged.

- [ ] **Step 4: Confirm no existing theorem regressed**

Run:

```bash
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Background/Properties.lean
```

Expected: both succeed.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Background/Transition.lean \
       crates/defra-agent/proofs/Proofs/Background/Properties.lean
git commit -m "$(cat <<'EOF'
Add steer_subagent_interrupt_preserves_link_symmetry theorem

R4c's only new Lean theorem. Discharges B5 preservation across the
interrupt + bridge_cancel_cascade + pendingAfterDrain + appendPending
composition that steer_subagent(interrupt=true) uses.

No new transitions; the SteerWithInterrupt prop is a witness over
existing primitives.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 2: Emit R4c Conformance Witnesses

**Purpose:** Emit six deterministic Lean witnesses so Rust tests detect drift in the observable shapes that the R4c spec promises. No new Lean module; additions to the existing contracts surface.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

**Steps:**

- [ ] **Step 1: Define the six witness types in `Types.lean`**

Add:

```lean
namespace R4cWitnesses

structure ListSubagentsLineageRejects where
  callerRequestId : String
  siblingRequestId : String
  siblingChildId : String
  callerSeesSiblingChild : Bool  -- always false in emitted witness

structure ReadTranscriptCursorAdvances where
  childSessionId : String
  firstSinceSequence : Nat
  firstThroughSequence : Nat
  firstNextSequence : Nat
  secondSinceSequence : Nat       -- = firstNextSequence
  secondThroughSequence : Nat
  noGap : Bool                    -- always true
  noOverlap : Bool                -- always true

structure ReadTranscriptHidesBridgeRows where
  childSessionId : String
  bridgeCallId : String
  renderedTranscript : String     -- bridge call id absent from rendering

structure ReadToolOutputDispatchesByState where
  toolCallId : String
  runningSource : String          -- "ring_buffer"
  terminalSource : String         -- "persisted_tool_completion"

structure SteerAppendPreservesLineage where
  callerRequestId : String
  childSessionId : String
  queuedRequestId : String
  causedByParentRequestId : String  -- = callerRequestId
  queueSource : String              -- "steering"
  queuePolicy : String              -- "append"

structure SteerInterruptComposes where
  callerRequestId : String
  childSessionId : String
  interruptedActiveRequestId : String
  drainedWakeUpRequestIds : List String
  queuedRequestId : String
  queueInterruptedRequestId : String  -- = interruptedActiveRequestId

end R4cWitnesses
```

- [ ] **Step 2: Add JSON encoders in `Json.lean`**

For each of the six structs, add a `ToJson` instance that emits a stable field order. Follow the convention already established in this file (look at the R4/R6 encoders and mirror).

Example for the first:

```lean
instance : ToJson R4cWitnesses.ListSubagentsLineageRejects where
  toJson w :=
    Json.mkObj
      [ ("witness", Json.str "list_subagents_lineage_rejects")
      , ("caller_request_id", Json.str w.callerRequestId)
      , ("sibling_request_id", Json.str w.siblingRequestId)
      , ("sibling_child_id", Json.str w.siblingChildId)
      , ("caller_sees_sibling_child", Json.bool w.callerSeesSiblingChild)
      ]
```

Repeat for the other five with parallel field-order discipline.

- [ ] **Step 3: Emit the six witnesses in `Contracts.lean`**

Add (in the `emit` function or its equivalent):

```lean
-- R4c witness emissions
let r4c_w1 : R4cWitnesses.ListSubagentsLineageRejects :=
  { callerRequestId := "r4c-w1-caller"
  , siblingRequestId := "r4c-w1-sibling"
  , siblingChildId := "r4c-w1-sibling-child"
  , callerSeesSiblingChild := false
  }
emit_witness "r4c.list_subagents.lineage_rejects" r4c_w1

let r4c_w2 : R4cWitnesses.ReadTranscriptCursorAdvances :=
  { childSessionId := "r4c-w2-session"
  , firstSinceSequence := 0
  , firstThroughSequence := 5
  , firstNextSequence := 6
  , secondSinceSequence := 6
  , secondThroughSequence := 10
  , noGap := true
  , noOverlap := true
  }
emit_witness "r4c.read_subagent_transcript.cursor_advances" r4c_w2

let r4c_w3 : R4cWitnesses.ReadTranscriptHidesBridgeRows :=
  { childSessionId := "r4c-w3-session"
  , bridgeCallId := "r4c-w3-bridge-call"
  , renderedTranscript := "[assistant seq=2]\nplain assistant message\n"
  }
emit_witness "r4c.read_subagent_transcript.hides_bridge_rows" r4c_w3

let r4c_w4 : R4cWitnesses.ReadToolOutputDispatchesByState :=
  { toolCallId := "r4c-w4-tool-call"
  , runningSource := "ring_buffer"
  , terminalSource := "persisted_tool_completion"
  }
emit_witness "r4c.read_tool_output.dispatch_by_state" r4c_w4

let r4c_w5 : R4cWitnesses.SteerAppendPreservesLineage :=
  { callerRequestId := "r4c-w5-caller"
  , childSessionId := "r4c-w5-child-session"
  , queuedRequestId := "r4c-w5-queued"
  , causedByParentRequestId := "r4c-w5-caller"
  , queueSource := "steering"
  , queuePolicy := "append"
  }
emit_witness "r4c.steer_subagent.append_preserves_lineage" r4c_w5

let r4c_w6 : R4cWitnesses.SteerInterruptComposes :=
  { callerRequestId := "r4c-w6-caller"
  , childSessionId := "r4c-w6-child-session"
  , interruptedActiveRequestId := "r4c-w6-interrupted"
  , drainedWakeUpRequestIds := ["r4c-w6-wake-1", "r4c-w6-wake-2"]
  , queuedRequestId := "r4c-w6-queued"
  , queueInterruptedRequestId := "r4c-w6-interrupted"
  }
emit_witness "r4c.steer_subagent.interrupt_composes" r4c_w6
```

- [ ] **Step 4: Register six ledger rows in `CoverageLedger.lean`**

Add (in the ledger constant):

```lean
, "r4c.list_subagents.lineage_rejects"
, "r4c.read_subagent_transcript.cursor_advances"
, "r4c.read_subagent_transcript.hides_bridge_rows"
, "r4c.read_tool_output.dispatch_by_state"
, "r4c.steer_subagent.append_preserves_lineage"
, "r4c.steer_subagent.interrupt_composes"
```

- [ ] **Step 5: Add Rust consumers in `tests/support/conformance_consumers.rs`**

For each witness, add a consumer struct mirroring the Lean type and a parse-from-JSON impl. Follow the R4/R6 consumer patterns already in this file:

```rust
#[derive(Debug, serde::Deserialize)]
pub struct ListSubagentsLineageRejectsWitness {
    pub caller_request_id: String,
    pub sibling_request_id: String,
    pub sibling_child_id: String,
    pub caller_sees_sibling_child: bool,
}

// ... repeat for the other five
```

Register each in the `consumers!` macro or its equivalent so the contracts test discovers them.

- [ ] **Step 6: Write Rust assertions in `state_machine_conformance.rs`**

Add one test per witness that:
1. Parses the witness JSON.
2. Sets up the corresponding Rust scenario (real `EmbeddedNode`, behaviors, requests).
3. Calls the R4c tool path that should match the witness.
4. Asserts the observable Rust shape matches the witness.

Example for w1:

```rust
#[tokio::test]
async fn r4c_list_subagents_lineage_rejects_matches_witness() {
    let witness: ListSubagentsLineageRejectsWitness =
        load_witness("r4c.list_subagents.lineage_rejects");

    let ctx = TestContext::new().await;
    let caller = ctx.spawn_request(&witness.caller_request_id).await;
    let sibling = ctx.spawn_request(&witness.sibling_request_id).await;
    ctx.spawn_subagent(&sibling, &witness.sibling_child_id).await;

    // Caller's list_subagents must not return sibling's child.
    let result = ctx.list_subagents(&caller).await.expect("list ok");
    assert!(
        !result.entries.iter().any(|e| e.child_request_id == witness.sibling_child_id),
        "caller saw sibling's child; expected lineage scoping to reject"
    );
}
```

The other five tests follow the same pattern: load witness, set up scenario, call R4c tool, assert envelope matches witness expectations.

**Note:** The R4c tool implementations don't exist yet (Tasks 3-10). These tests will compile-error or fail until the corresponding tool tasks land. That's the intended TDD flow.

- [ ] **Step 7: Verify Lean compiles and witnesses emit**

Run:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4c-witnesses.json
jq '.[] | select(.witness | startswith("r4c."))' /tmp/r4c-witnesses.json | head -60
```

Expected: six R4c witness objects appear in the JSON output.

- [ ] **Step 8: Verify ledger coverage**

Run:

```bash
cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain
```

Expected: PASS. The ledger accounts for every emitted witness.

- [ ] **Step 9: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/ \
       crates/defra-agent/tests/support/conformance_consumers.rs \
       crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
Emit R4c conformance witnesses for the six observable shapes

Witnesses cover:
- list_subagents lineage scoping rejects siblings
- read_subagent_transcript cursor advances cleanly across pages
- read_subagent_transcript hides bridge rows
- read_tool_output dispatches buffer vs persisted by state
- steer_subagent(interrupt=false) preserves lineage
- steer_subagent(interrupt=true) composes interrupt+cascade+drain+append

Rust consumers fail-fast for unimplemented tools; that drives the TDD
flow for the next eight tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 3: Define R4c Argument And Envelope Types

**Purpose:** Create the strongly-typed argument and return envelopes for the five R4c tools in a single focused module. Keeps `background_tools.rs` itself focused on hook-interceptor wiring.

**Files:**

- Create: `crates/defra-agent/src/background_tools/r4c_args.rs`
- Modify: `crates/defra-agent/src/background_tools.rs` (`mod r4c_args;`)

**Steps:**

- [ ] **Step 1: Write a failing serialization test**

Add at the bottom of `r4c_args.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_subagents_args_round_trip() {
        let args: ListSubagentsArgs = serde_json::from_value(json!({
            "status": "running",
            "limit": 20
        })).expect("parse");
        assert_eq!(args.status, ListStatusFilter::Running);
        assert_eq!(args.limit, 20);
    }

    #[test]
    fn list_subagents_args_defaults() {
        let args: ListSubagentsArgs = serde_json::from_value(json!({})).expect("parse");
        assert_eq!(args.status, ListStatusFilter::Running);
        assert_eq!(args.limit, 20);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p defra-agent --lib background_tools::r4c_args
```

Expected: compile error (the types don't exist yet).

- [ ] **Step 3: Define the types**

Write the module body:

```rust
//! Argument and envelope types for the five R4c agent-facing tools.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_LIST_LIMIT: u32 = 20;
const MAX_LIST_LIMIT: u32 = 50;
const DEFAULT_TRANSCRIPT_LIMIT: u32 = 20;
const MAX_TRANSCRIPT_LIMIT: u32 = 100;
const DEFAULT_TRANSCRIPT_MAX_CHARS: u32 = 6000;
const MAX_TRANSCRIPT_MAX_CHARS: u32 = 24000;
const DEFAULT_READ_TOOL_OUTPUT_BYTES: u32 = 16384;
const MAX_READ_TOOL_OUTPUT_BYTES: u32 = 262144;
pub(crate) const PER_TOOL_RESULT_SNIPPET_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ListStatusFilter {
    #[default]
    Running,
    Terminal,
    All,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListSubagentsArgs {
    #[serde(default)]
    pub status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub limit: u32,
}

fn default_list_limit() -> u32 { DEFAULT_LIST_LIMIT }

impl ListSubagentsArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.min(MAX_LIST_LIMIT).max(1)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListBackgroundToolsArgs {
    #[serde(default)]
    pub status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub limit: u32,
}

impl ListBackgroundToolsArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.min(MAX_LIST_LIMIT).max(1)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReadSubagentTranscriptArgs {
    pub child_request_id: String,
    #[serde(default)]
    pub since_sequence: u64,
    #[serde(default = "default_transcript_limit")]
    pub limit: u32,
    #[serde(default = "default_transcript_max_chars")]
    pub max_chars: u32,
    #[serde(default)]
    pub include_user_messages: bool,
    #[serde(default)]
    pub include_tool_results: bool,
}

fn default_transcript_limit() -> u32 { DEFAULT_TRANSCRIPT_LIMIT }
fn default_transcript_max_chars() -> u32 { DEFAULT_TRANSCRIPT_MAX_CHARS }

impl ReadSubagentTranscriptArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.min(MAX_TRANSCRIPT_LIMIT).max(1)
    }
    pub(crate) fn validated_max_chars(&self) -> u32 {
        self.max_chars.min(MAX_TRANSCRIPT_MAX_CHARS).max(64)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReadToolOutputArgs {
    pub tool_call_id: String,
    #[serde(default = "default_read_tool_output_bytes")]
    pub max_bytes_per_stream: u32,
}

fn default_read_tool_output_bytes() -> u32 { DEFAULT_READ_TOOL_OUTPUT_BYTES }

impl ReadToolOutputArgs {
    pub(crate) fn validated_max_bytes(&self) -> u32 {
        self.max_bytes_per_stream.min(MAX_READ_TOOL_OUTPUT_BYTES).max(256)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SteerSubagentArgs {
    pub child_request_id: String,
    pub message: String,
    #[serde(default)]
    pub interrupt: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListSubagentsEntry {
    pub child_request_id: String,
    pub child_session_id: String,
    pub behavior_id: String,
    pub deployment_id: String,
    pub await_mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_update: DateTime<Utc>,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListSubagentsResponse {
    pub read_at: DateTime<Utc>,
    pub truncated: bool,
    pub entries: Vec<ListSubagentsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsEntry {
    pub tool_call_id: String,
    pub tool_name: String,
    pub deployment_id: String,
    pub await_mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_update: DateTime<Utc>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsResponse {
    pub read_at: DateTime<Utc>,
    pub truncated: bool,
    pub entries: Vec<ListBackgroundToolsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadSubagentTranscriptResponse {
    pub child_request_id: String,
    pub child_session_id: String,
    pub from_sequence: u64,
    pub through_sequence: u64,
    pub next_sequence: u64,
    pub truncated: bool,
    pub transcript: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadToolOutputStream {
    pub bytes: String,
    pub truncated: bool,
    pub total_bytes_seen: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadToolOutputResponse {
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: String,
    pub stdout: ReadToolOutputStream,
    pub stderr: ReadToolOutputStream,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SteerSubagentResponse {
    pub child_request_id: String,
    pub child_session_id: String,
    pub queued_request_id: String,
    pub interrupted_active_request_id: Option<String>,
    pub drained_wake_up_request_ids: Vec<String>,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p defra-agent --lib background_tools::r4c_args
```

Expected: PASS (both `list_subagents_args_round_trip` and `list_subagents_args_defaults`).

- [ ] **Step 5: Add round-trip tests for the other arg types**

Add four more `#[test]` blocks mirroring `list_subagents_args_round_trip` for `ListBackgroundToolsArgs`, `ReadSubagentTranscriptArgs`, `ReadToolOutputArgs`, `SteerSubagentArgs`. Each should verify the field defaults and an explicit-fields round trip.

Run:

```bash
cargo test -p defra-agent --lib background_tools::r4c_args
```

Expected: all five round-trip tests PASS.

- [ ] **Step 6: Wire `mod r4c_args;` into `background_tools.rs`**

Add at the top of `background_tools.rs`:

```rust
mod r4c_args;
```

Run:

```bash
cargo check -p defra-agent
```

Expected: clean compile.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/src/background_tools/r4c_args.rs \
       crates/defra-agent/src/background_tools.rs
git commit -m "$(cat <<'EOF'
Add R4c argument and envelope types

Centralizes the strongly-typed arg/return shapes for list_subagents,
list_background_tools, read_subagent_transcript, read_tool_output, and
steer_subagent in background_tools/r4c_args.rs. Hard caps and defaults
mirror the R4c spec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 4: Implement Transcript Renderer

**Purpose:** Pure-function transcript renderer in its own module. Unit-testable without DB plumbing. Owns the compact-text formatting, filtering pipeline, and snippet caps.

**Files:**

- Create: `crates/defra-agent/src/background_tools/transcript_render.rs`
- Modify: `crates/defra-agent/src/background_tools.rs` (`mod transcript_render;`)

**Steps:**

- [ ] **Step 1: Define the inputs and outputs**

Add the file:

```rust
//! Pure-function transcript renderer for read_subagent_transcript.

use crate::background_tools::r4c_args::PER_TOOL_RESULT_SNIPPET_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageRoleView {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageKindView {
    Ordinary { body: String },
    AssistantWithToolCalls {
        body: String,
        tool_call_count: u32,
        /// Bridge call ids that must be hidden from rendering.
        bridge_call_ids: Vec<String>,
        /// Non-bridge tool calls (visible to renderer for the count suffix).
        non_bridge_tool_call_count: u32,
    },
    ToolResult { tool_name: String, body: String },
}

#[derive(Debug, Clone)]
pub(crate) struct MessageView {
    pub sequence: u64,
    pub role: MessageRoleView,
    pub kind: MessageKindView,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RenderOptions {
    pub include_user_messages: bool,
    pub include_tool_results: bool,
    pub limit: u32,
    pub max_chars: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct RenderOutput {
    pub transcript: String,
    pub from_sequence: u64,
    pub through_sequence: u64,
    pub next_sequence: u64,
    pub truncated: bool,
}

pub(crate) fn render_transcript(
    messages: &[MessageView],
    since_sequence: u64,
    opts: RenderOptions,
) -> RenderOutput {
    let mut transcript = String::new();
    let mut first_included: Option<u64> = None;
    let mut last_included: u64 = since_sequence;
    let mut included_count: u32 = 0;
    let mut truncated = false;

    for msg in messages {
        if msg.sequence <= since_sequence {
            continue;
        }
        if !opts.include_user_messages && msg.role == MessageRoleView::User {
            continue;
        }
        let block = match render_block(msg, opts) {
            Some(b) => b,
            None => continue, // bridge-only assistant messages or filtered rows
        };
        let projected_len = transcript.len()
            + block.len()
            + if transcript.is_empty() { 0 } else { 1 };
        if included_count + 1 > opts.limit
            || projected_len > opts.max_chars as usize
        {
            truncated = true;
            break;
        }
        if !transcript.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(&block);
        included_count += 1;
        if first_included.is_none() {
            first_included = Some(msg.sequence);
        }
        last_included = msg.sequence;
    }

    let from_sequence = first_included.unwrap_or(since_sequence);
    RenderOutput {
        transcript,
        from_sequence,
        through_sequence: last_included,
        next_sequence: last_included.saturating_add(1).max(since_sequence + 1),
        truncated,
    }
}

fn render_block(msg: &MessageView, opts: RenderOptions) -> Option<String> {
    match (&msg.role, &msg.kind) {
        (MessageRoleView::User, MessageKindView::Ordinary { body }) => {
            Some(format!("[user seq={}]\n{}", msg.sequence, body))
        }
        (MessageRoleView::Assistant, MessageKindView::Ordinary { body }) => {
            Some(format!("[assistant seq={}]\n{}", msg.sequence, body))
        }
        (MessageRoleView::Assistant, MessageKindView::AssistantWithToolCalls {
            body,
            non_bridge_tool_call_count,
            ..
        }) => {
            // Bridge call ids hidden by construction (filtered out before render).
            // If all tool calls in this message are bridge calls, render as plain assistant.
            if *non_bridge_tool_call_count == 0 {
                Some(format!("[assistant seq={}]\n{}", msg.sequence, body))
            } else {
                Some(format!(
                    "[assistant seq={} tool_calls={}]\n{}",
                    msg.sequence, non_bridge_tool_call_count, body
                ))
            }
        }
        (MessageRoleView::User, MessageKindView::ToolResult { tool_name, body }) => {
            if !opts.include_tool_results {
                None
            } else {
                let snippet: String = body
                    .chars()
                    .take(PER_TOOL_RESULT_SNIPPET_BYTES)
                    .collect();
                Some(format!(
                    "[tool-result seq={} tool={}]\n{}",
                    msg.sequence, tool_name, snippet
                ))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(seq: u64, body: &str) -> MessageView {
        MessageView {
            sequence: seq,
            role: MessageRoleView::Assistant,
            kind: MessageKindView::Ordinary { body: body.to_string() },
        }
    }

    fn user(seq: u64, body: &str) -> MessageView {
        MessageView {
            sequence: seq,
            role: MessageRoleView::User,
            kind: MessageKindView::Ordinary { body: body.to_string() },
        }
    }

    fn assistant_with_bridge_calls(
        seq: u64,
        body: &str,
        bridge_call_ids: Vec<&str>,
    ) -> MessageView {
        MessageView {
            sequence: seq,
            role: MessageRoleView::Assistant,
            kind: MessageKindView::AssistantWithToolCalls {
                body: body.to_string(),
                tool_call_count: bridge_call_ids.len() as u32,
                bridge_call_ids: bridge_call_ids.iter().map(|s| s.to_string()).collect(),
                non_bridge_tool_call_count: 0,
            },
        }
    }

    fn tool_result(seq: u64, tool: &str, body: &str) -> MessageView {
        MessageView {
            sequence: seq,
            role: MessageRoleView::User,
            kind: MessageKindView::ToolResult { tool_name: tool.to_string(), body: body.to_string() },
        }
    }

    const OPTS_DEFAULT: RenderOptions = RenderOptions {
        include_user_messages: false,
        include_tool_results: false,
        limit: 20,
        max_chars: 6000,
    };

    #[test]
    fn assistant_only_default() {
        let msgs = vec![assistant(1, "hello"), user(2, "ignored"), assistant(3, "world")];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(out.transcript.contains("[assistant seq=1]"));
        assert!(out.transcript.contains("[assistant seq=3]"));
        assert!(!out.transcript.contains("[user"));
        assert_eq!(out.from_sequence, 1);
        assert_eq!(out.through_sequence, 3);
        assert_eq!(out.next_sequence, 4);
        assert!(!out.truncated);
    }

    #[test]
    fn include_user_messages_when_opted_in() {
        let msgs = vec![assistant(1, "hello"), user(2, "real input"), assistant(3, "ok")];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions { include_user_messages: true, ..OPTS_DEFAULT },
        );
        assert!(out.transcript.contains("[user seq=2]"));
        assert!(out.transcript.contains("real input"));
    }

    #[test]
    fn bridge_only_assistant_renders_plain() {
        let msgs = vec![assistant_with_bridge_calls(5, "spawning child", vec!["bridge-1"])];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(out.transcript.contains("[assistant seq=5]"));
        assert!(!out.transcript.contains("tool_calls="));
        assert!(!out.transcript.contains("bridge-1"));
    }

    #[test]
    fn tool_result_hidden_by_default() {
        let msgs = vec![assistant(1, "hi"), tool_result(2, "bash", "stdout")];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(!out.transcript.contains("[tool-result"));
    }

    #[test]
    fn tool_result_snippet_capped() {
        let big_body = "x".repeat(1024);
        let msgs = vec![tool_result(1, "bash", &big_body)];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions { include_tool_results: true, ..OPTS_DEFAULT },
        );
        assert!(out.transcript.contains("[tool-result seq=1 tool=bash]"));
        let snippet_len = out.transcript
            .split("[tool-result seq=1 tool=bash]\n")
            .nth(1).expect("snippet body").len();
        assert!(snippet_len <= PER_TOOL_RESULT_SNIPPET_BYTES);
    }

    #[test]
    fn since_sequence_skips_earlier() {
        let msgs = vec![assistant(1, "a"), assistant(2, "b"), assistant(3, "c")];
        let out = render_transcript(&msgs, 1, OPTS_DEFAULT);
        assert!(!out.transcript.contains("[assistant seq=1]"));
        assert!(out.transcript.contains("[assistant seq=2]"));
        assert_eq!(out.from_sequence, 2);
    }

    #[test]
    fn truncated_when_limit_hit() {
        let msgs = vec![assistant(1, "a"), assistant(2, "b"), assistant(3, "c")];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions { limit: 2, ..OPTS_DEFAULT },
        );
        assert!(out.truncated);
        assert_eq!(out.through_sequence, 2);
        assert_eq!(out.next_sequence, 3);
    }

    #[test]
    fn truncated_when_max_chars_hit() {
        let long = "x".repeat(200);
        let msgs = vec![assistant(1, &long), assistant(2, &long), assistant(3, &long)];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions { max_chars: 250, ..OPTS_DEFAULT },
        );
        assert!(out.truncated);
        assert!(out.transcript.len() <= 250);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run:

```bash
cargo test -p defra-agent --lib background_tools::transcript_render
```

Expected: all eight tests PASS.

- [ ] **Step 3: Wire `mod transcript_render;` into `background_tools.rs`**

Add near the existing `mod r4c_args;`:

```rust
mod transcript_render;
```

Run:

```bash
cargo check -p defra-agent
```

Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/background_tools/transcript_render.rs \
       crates/defra-agent/src/background_tools.rs
git commit -m "$(cat <<'EOF'
Add pure-function transcript renderer for read_subagent_transcript

Renders compact LLM-facing text from a slice of MessageView rows.
Assistant-only by default; opt-in user messages and tool-result snippets
(256-byte cap). Bridge call rows hidden by construction. Pagination via
since_sequence; truncation when limit or max_chars hits.

Tests cover defaults, opt-in flags, bridge hiding, snippet caps, and
both truncation modes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 5: Implement `list_subagents` Tool

**Purpose:** First R4c tool. Hook-intercepted; per-parent-request lineage query; returns a snapshot envelope.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Create: `crates/defra-agent/tests/r4c_list_subagents.rs`

**Steps:**

- [ ] **Step 1: Write the integration test (failing)**

Create `crates/defra-agent/tests/r4c_list_subagents.rs`:

```rust
//! Integration tests for R4c list_subagents.

mod support;

use defra_agent::test_support::*;
use serde_json::json;

#[tokio::test]
async fn list_subagents_returns_running_children() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p1", "Solve X").await;
    let child_a = ctx.spawn_subagent(&parent, "amy-code", "do A", "background").await;
    let child_b = ctx.spawn_subagent(&parent, "amy-code", "do B", "background").await;

    let result = ctx.call_tool(&parent, "list_subagents", json!({})).await
        .expect("ok");

    let entries = result["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    let ids: Vec<&str> = entries.iter()
        .map(|e| e["child_request_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&child_a.request_id.as_str()));
    assert!(ids.contains(&child_b.request_id.as_str()));
    for e in entries {
        assert_eq!(e["deployment_id"].as_str().unwrap(), ctx.local_deployment_id());
        assert_eq!(e["await_mode"].as_str().unwrap(), "background");
        assert_eq!(e["status"].as_str().unwrap(), "running");
    }
}

#[tokio::test]
async fn list_subagents_rejects_sibling_children() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent1 = ctx.spawn_parent_request("p1", "first").await;
    let parent2 = ctx.spawn_parent_request("p2", "second").await;
    let _child_of_p2 = ctx.spawn_subagent(&parent2, "amy-code", "do X", "background").await;

    let result = ctx.call_tool(&parent1, "list_subagents", json!({})).await
        .expect("ok");
    let entries = result["entries"].as_array().expect("entries");
    assert!(entries.is_empty(), "parent1 must not see parent2's children");
}

#[tokio::test]
async fn list_subagents_status_filter() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "task", "background").await;
    ctx.terminalize_child(&child, "completed", "result").await;

    let running = ctx.call_tool(&parent, "list_subagents", json!({"status": "running"})).await
        .expect("ok");
    assert_eq!(running["entries"].as_array().unwrap().len(), 0);

    let terminal = ctx.call_tool(&parent, "list_subagents", json!({"status": "terminal"})).await
        .expect("ok");
    assert_eq!(terminal["entries"].as_array().unwrap().len(), 1);

    let all = ctx.call_tool(&parent, "list_subagents", json!({"status": "all"})).await
        .expect("ok");
    assert_eq!(all["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn list_subagents_limit_truncates() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    for i in 0..5 {
        ctx.spawn_subagent(&parent, "amy-code", &format!("task {i}"), "background").await;
    }
    let result = ctx.call_tool(&parent, "list_subagents", json!({"limit": 3})).await
        .expect("ok");
    assert_eq!(result["entries"].as_array().unwrap().len(), 3);
    assert_eq!(result["truncated"].as_bool().unwrap(), true);
}

#[tokio::test]
async fn list_subagents_no_parent_tool_call_row_written() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    ctx.spawn_subagent(&parent, "amy-code", "task", "background").await;
    let _ = ctx.call_tool(&parent, "list_subagents", json!({})).await;

    let tool_call_rows = ctx.fetch_tool_calls(&parent).await;
    assert!(
        !tool_call_rows.iter().any(|r| r.tool_name == "list_subagents"),
        "no AgentToolCall row should exist for list_subagents"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p defra-agent --test r4c_list_subagents
```

Expected: compile error (`call_tool("list_subagents", ...)` returns an error because the tool is not registered).

- [ ] **Step 3: Add the implementation in `background_tools.rs`**

Add (alongside the R4 subagent tool helpers):

```rust
use crate::background_tools::r4c_args::{
    ListSubagentsArgs, ListSubagentsEntry, ListSubagentsResponse, ListStatusFilter,
};
use chrono::Utc;
use defra_node::EmbeddedNode;

pub(crate) async fn handle_list_subagents(
    node: &EmbeddedNode,
    caller_request_id: &str,
    local_deployment_id: &str,
    args: ListSubagentsArgs,
) -> Result<ListSubagentsResponse> {
    let limit = args.validated_limit();

    let status_clause = match args.status {
        ListStatusFilter::Running => "status: { _eq: \"processing\" }",
        ListStatusFilter::Terminal => {
            "_or: [\
                { status: { _eq: \"completed\" } }, \
                { status: { _eq: \"failed\" } }, \
                { status: { _eq: \"dead\" } }, \
                { status: { _eq: \"interrupted\" } }, \
                { status: { _eq: \"superseded\" } } \
            ]"
        }
        ListStatusFilter::All => "_or: [{ status: { _neq: \"\" } }]",
    };

    let query = format!(
        r#"
        query {{
            AgentRequest(
                filter: {{
                    caused_by_parent_request_id: {{ _eq: "{caller}" }},
                    {status}
                }},
                order: {{ created_at: ASC }},
                limit: {limit_plus_one}
            ) {{
                _docID
                session_id
                behavior_id
                await_mode
                status
                created_at
                updated_at
                subagent_depth
            }}
        }}
        "#,
        caller = escape_graphql_string(caller_request_id),
        status = status_clause,
        limit_plus_one = limit + 1,
    );

    let raw = node.execute_query(&query).await
        .map_err(|e| anyhow!("list_subagents query failed: {e}"))?;

    let rows: Vec<RawSubagentRow> = parse_subagent_rows(&raw)?;
    let truncated = rows.len() > limit as usize;
    let entries: Vec<ListSubagentsEntry> = rows.into_iter()
        .take(limit as usize)
        .map(|r| ListSubagentsEntry {
            child_request_id: r.id,
            child_session_id: r.session_id,
            behavior_id: r.behavior_id,
            deployment_id: local_deployment_id.to_string(),
            await_mode: r.await_mode,
            status: r.status,
            created_at: r.created_at,
            last_update: r.updated_at,
            depth: r.subagent_depth,
        })
        .collect();

    Ok(ListSubagentsResponse {
        read_at: Utc::now(),
        truncated,
        entries,
    })
}

#[derive(Debug, serde::Deserialize)]
struct RawSubagentRow {
    #[serde(rename = "_docID")]
    id: String,
    session_id: String,
    behavior_id: String,
    await_mode: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    subagent_depth: u32,
}

fn parse_subagent_rows(raw: &serde_json::Value) -> Result<Vec<RawSubagentRow>> {
    let rows = raw
        .get("data")
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("AgentRequest field missing in list_subagents query"))?;
    rows.iter().cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("parse subagent rows: {e}"))
}
```

- [ ] **Step 4: Register the tool in `tool_surface/selection.rs`**

Locate the existing R4 subagent tool registrations and add (gated by the same `subagent_targets` non-empty check that gates `spawn_subagent`):

```rust
if !selection.subagent_targets.is_empty() {
    register_native_tool("list_subagents", list_subagents_tool_definition());
}
```

Where `list_subagents_tool_definition()` constructs the Rig tool struct with the JSON schema for `ListSubagentsArgs`.

- [ ] **Step 5: Intercept `list_subagents` in `hook/persistence.rs`**

Locate the existing branch that intercepts `wait_subagent` and `cancel_subagent` before ordinary tool-call persistence (R4 pattern). Add an analogous branch for `list_subagents`:

```rust
"list_subagents" => {
    let args: ListSubagentsArgs = serde_json::from_value(call.args.clone())
        .map_err(|e| structured_argument_invalid_error("list_subagents", "/", &e.to_string()))?;
    let response = background_tools::handle_list_subagents(
        node,
        &caller_request_id,
        &local_deployment_id,
        args,
    ).await?;
    return Ok(HookOutcome::ReturnedWithoutPersistence {
        result: serde_json::to_value(response)?,
    });
}
```

The key invariant: this branch returns `HookOutcome::ReturnedWithoutPersistence`, which prevents the ordinary tool-call lifecycle from creating an `AgentToolCall` row.

- [ ] **Step 6: Run the test to verify it passes**

Run:

```bash
cargo test -p defra-agent --test r4c_list_subagents
```

Expected: all five tests PASS.

- [ ] **Step 7: Run the conformance witness test for w1**

Run:

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_list_subagents_lineage_rejects
```

Expected: PASS. The witness from Task 2 now resolves against the running implementation.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/src/hook.rs \
       crates/defra-agent/src/hook/persistence.rs \
       crates/defra-agent/src/tool_surface/selection.rs \
       crates/defra-agent/tests/r4c_list_subagents.rs
git commit -m "$(cat <<'EOF'
Implement list_subagents agent-facing tool

Per-parent-request lineage scope; status filter (running|terminal|all);
limit hard cap 50; snapshot envelope with read_at and truncated flag.
Hook-intercepted before persistence; no AgentToolCall row written.

Resolves the r4c.list_subagents.lineage_rejects conformance witness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 6: Implement `list_background_tools` Tool

**Purpose:** Symmetric to `list_subagents` for the Tool kind. Queries `AgentToolCall` rows where `request_id = caller AND await_mode = .background`.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Create: `crates/defra-agent/tests/r4c_list_background_tools.rs`

**Steps:**

- [ ] **Step 1: Write integration tests (failing)**

Create `crates/defra-agent/tests/r4c_list_background_tools.rs` with five tests mirroring Task 5's test structure:

```rust
mod support;

use defra_agent::test_support::*;
use serde_json::json;

#[tokio::test]
async fn list_background_tools_returns_running_bg_tools() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let h1 = ctx.background_tool(&parent, "bash", json!({"cmd": "sleep 10"})).await;
    let h2 = ctx.background_tool(&parent, "bash", json!({"cmd": "sleep 20"})).await;

    let result = ctx.call_tool(&parent, "list_background_tools", json!({})).await
        .expect("ok");

    let entries = result["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let ids: Vec<&str> = entries.iter()
        .map(|e| e["tool_call_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&h1.tool_call_id.as_str()));
    assert!(ids.contains(&h2.tool_call_id.as_str()));
    for e in entries {
        assert_eq!(e["tool_name"].as_str().unwrap(), "bash");
        assert_eq!(e["await_mode"].as_str().unwrap(), "background");
        assert_eq!(e["status"].as_str().unwrap(), "running");
    }
}

#[tokio::test]
async fn list_background_tools_rejects_sibling_requests() {
    let ctx = TestContext::start_with_default_behavior().await;
    let p1 = ctx.spawn_parent_request("p1", "first").await;
    let p2 = ctx.spawn_parent_request("p2", "second").await;
    ctx.background_tool(&p2, "bash", json!({"cmd": "sleep 5"})).await;

    let result = ctx.call_tool(&p1, "list_background_tools", json!({})).await
        .expect("ok");
    assert!(result["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_background_tools_excludes_foreground_calls() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    ctx.run_foreground_tool(&parent, "read_file", json!({"path": "/etc/hostname"})).await;
    ctx.background_tool(&parent, "bash", json!({"cmd": "sleep 5"})).await;

    let result = ctx.call_tool(&parent, "list_background_tools", json!({})).await
        .expect("ok");
    let entries = result["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["tool_name"].as_str().unwrap(), "bash");
}

#[tokio::test]
async fn list_background_tools_status_filter() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let h = ctx.background_tool(&parent, "bash", json!({"cmd": "true"})).await;
    ctx.wait_for_terminal(&h).await;

    let running = ctx.call_tool(&parent, "list_background_tools", json!({"status": "running"})).await
        .expect("ok");
    assert!(running["entries"].as_array().unwrap().is_empty());
    let terminal = ctx.call_tool(&parent, "list_background_tools", json!({"status": "terminal"})).await
        .expect("ok");
    assert_eq!(terminal["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn list_background_tools_no_parent_tool_call_row_written() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    ctx.background_tool(&parent, "bash", json!({"cmd": "sleep 5"})).await;
    let _ = ctx.call_tool(&parent, "list_background_tools", json!({})).await;

    let tool_calls = ctx.fetch_tool_calls(&parent).await;
    assert!(!tool_calls.iter().any(|r| r.tool_name == "list_background_tools"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p defra-agent --test r4c_list_background_tools
```

Expected: compile error / tool not registered.

- [ ] **Step 3: Add `handle_list_background_tools` in `background_tools.rs`**

Mirror the structure of `handle_list_subagents`. The query targets `AgentToolCall`:

```rust
pub(crate) async fn handle_list_background_tools(
    node: &EmbeddedNode,
    caller_request_id: &str,
    local_deployment_id: &str,
    args: ListBackgroundToolsArgs,
) -> Result<ListBackgroundToolsResponse> {
    let limit = args.validated_limit();
    let status_clause = match args.status {
        ListStatusFilter::Running => "state: { _eq: \"running\" }",
        ListStatusFilter::Terminal => {
            "_or: [\
                { state: { _eq: \"completed\" } }, \
                { state: { _eq: \"failed\" } }, \
                { state: { _eq: \"cancelled\" } }, \
                { state: { _eq: \"interrupted\" } } \
            ]"
        }
        ListStatusFilter::All => "state: { _neq: \"\" }",
    };
    let query = format!(
        r#"
        query {{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{caller}" }},
                    await_mode: {{ _eq: "background" }},
                    {status}
                }},
                order: {{ created_at: ASC }},
                limit: {limit_plus_one}
            ) {{
                _docID
                tool_name
                await_mode
                state
                created_at
                updated_at
                stdout_bytes
                stderr_bytes
            }}
        }}
        "#,
        caller = escape_graphql_string(caller_request_id),
        status = status_clause,
        limit_plus_one = limit + 1,
    );
    let raw = node.execute_query(&query).await
        .map_err(|e| anyhow!("list_background_tools query failed: {e}"))?;
    let rows: Vec<RawBgToolRow> = parse_bg_tool_rows(&raw)?;
    let truncated = rows.len() > limit as usize;
    let entries: Vec<ListBackgroundToolsEntry> = rows.into_iter()
        .take(limit as usize)
        .map(|r| ListBackgroundToolsEntry {
            tool_call_id: r.id,
            tool_name: r.tool_name,
            deployment_id: local_deployment_id.to_string(),
            await_mode: r.await_mode,
            status: r.state,
            created_at: r.created_at,
            last_update: r.updated_at,
            stdout_bytes: r.stdout_bytes,
            stderr_bytes: r.stderr_bytes,
        })
        .collect();
    Ok(ListBackgroundToolsResponse { read_at: Utc::now(), truncated, entries })
}

#[derive(Debug, serde::Deserialize)]
struct RawBgToolRow {
    #[serde(rename = "_docID")]
    id: String,
    tool_name: String,
    await_mode: String,
    state: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

fn parse_bg_tool_rows(raw: &serde_json::Value) -> Result<Vec<RawBgToolRow>> {
    let rows = raw.get("data").and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("AgentToolCall field missing"))?;
    rows.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("parse bg tool rows: {e}"))
}
```

**Note on `stdout_bytes`/`stderr_bytes`:** R6's `background_completion.rs` persists these on `AgentToolCall` rows when terminal. For running rows the schema must surface the ring buffer's current occupancy. If R6 did not add these fields to the `AgentToolCall` schema, this task absorbs the migration: add `stdout_bytes: Int` and `stderr_bytes: Int` to the `AgentToolCall` GraphQL schema with default 0, and update R6's executor to write them as the buffer grows. Coordinate with the R6 implementation; if the schema fields exist, no migration needed.

- [ ] **Step 4: Register and intercept**

In `tool_surface/selection.rs`:

```rust
if !selection.backgroundable_tool_names.is_empty() {
    register_native_tool("list_background_tools", list_background_tools_tool_definition());
}
```

In `hook/persistence.rs`, add a branch analogous to `list_subagents` that calls `handle_list_background_tools` and returns without persistence.

- [ ] **Step 5: Run integration tests**

Run:

```bash
cargo test -p defra-agent --test r4c_list_background_tools
```

Expected: all five tests PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/src/hook/persistence.rs \
       crates/defra-agent/src/tool_surface/selection.rs \
       crates/defra-agent/tests/r4c_list_background_tools.rs \
       crates/defra-agent-protocol/schemas/  # if schema migration needed
git commit -m "$(cat <<'EOF'
Implement list_background_tools agent-facing tool

Symmetric to list_subagents for Tool kind. Per-parent-request lineage;
filters await_mode = background; status filter; snapshot envelope.
Hook-intercepted; no AgentToolCall row written.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 7: Implement `read_subagent_transcript` Tool

**Purpose:** Query the child session's transcript, hide bridge calls, render via the Task 4 renderer, return compact text.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Create: `crates/defra-agent/tests/r4c_read_subagent_transcript.rs`

**Steps:**

- [ ] **Step 1: Write integration tests (failing)**

Create the test file. Key scenarios from the spec:

```rust
mod support;

use defra_agent::test_support::*;
use serde_json::json;

#[tokio::test]
async fn read_transcript_assistant_only_default() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    ctx.append_assistant_message(&child.session_id, "first thought").await;
    ctx.append_user_message(&child.session_id, "feedback").await;
    ctx.append_assistant_message(&child.session_id, "second thought").await;

    let result = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
    })).await.expect("ok");
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("first thought"));
    assert!(transcript.contains("second thought"));
    assert!(!transcript.contains("feedback"));
}

#[tokio::test]
async fn read_transcript_includes_user_when_opted_in() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    ctx.append_assistant_message(&child.session_id, "a1").await;
    ctx.append_user_message(&child.session_id, "u1").await;

    let result = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
        "include_user_messages": true,
    })).await.expect("ok");
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("u1"));
}

#[tokio::test]
async fn read_transcript_hides_bridge_rows() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    // Child backgrounds a tool (creates a bridge AgentToolCall row).
    ctx.append_assistant_with_bridge_call(&child.session_id, "spawning child", "bridge-tc-1").await;

    let result = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
    })).await.expect("ok");
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("spawning child"));
    assert!(!transcript.contains("bridge-tc-1"));
    assert!(!transcript.contains("tool_calls="));
}

#[tokio::test]
async fn read_transcript_cursor_advances_cleanly() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    for i in 1..=10 {
        ctx.append_assistant_message(&child.session_id, &format!("turn {i}")).await;
    }

    let first = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
        "limit": 5,
    })).await.expect("ok");
    let next = first["next_sequence"].as_u64().unwrap();
    assert_eq!(first["truncated"].as_bool().unwrap(), true);

    let second = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
        "since_sequence": next,
        "limit": 5,
    })).await.expect("ok");
    let combined = format!(
        "{}\n{}",
        first["transcript"].as_str().unwrap(),
        second["transcript"].as_str().unwrap()
    );
    for i in 1..=10 {
        assert!(combined.contains(&format!("turn {i}")), "missing turn {i}");
    }
}

#[tokio::test]
async fn read_transcript_rejects_unauthorized_child() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent1 = ctx.spawn_parent_request("p1", "first").await;
    let parent2 = ctx.spawn_parent_request("p2", "second").await;
    let child = ctx.spawn_subagent(&parent2, "amy-code", "do X", "background").await;

    let result = ctx.call_tool(&parent1, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
    })).await;
    assert!(result.is_err() || result.as_ref().unwrap()["ok"] == false);
}

#[tokio::test]
async fn read_transcript_no_parent_tool_call_row_written() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    let _ = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": child.request_id,
    })).await;
    let tool_calls = ctx.fetch_tool_calls(&parent).await;
    assert!(!tool_calls.iter().any(|r| r.tool_name == "read_subagent_transcript"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p defra-agent --test r4c_read_subagent_transcript
```

Expected: compile error / tool not registered.

- [ ] **Step 3: Implement the handler**

In `background_tools.rs`, add `handle_read_subagent_transcript`:

```rust
use crate::background_tools::transcript_render::{
    MessageKindView, MessageRoleView, MessageView, RenderOptions, render_transcript,
};
use crate::background_tools::r4c_args::{
    ReadSubagentTranscriptArgs, ReadSubagentTranscriptResponse,
};

pub(crate) async fn handle_read_subagent_transcript(
    node: &EmbeddedNode,
    caller_request_id: &str,
    args: ReadSubagentTranscriptArgs,
) -> Result<ReadSubagentTranscriptResponse> {
    // 1. Authorize: child_request_id must be owned by caller.
    let child = fetch_child_subagent_row(node, caller_request_id, &args.child_request_id).await?
        .ok_or_else(|| structured_tool_not_allowed_error(
            "read_subagent_transcript",
            "/child_request_id",
            "child not owned by this parent request",
        ))?;

    // 2. Query child session messages above since_sequence.
    let limit = args.validated_limit();
    let max_chars = args.validated_max_chars();
    let query_limit = limit * 2 + 10; // overhead for filtered rows
    let query = format!(
        r#"
        query {{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{session}" }},
                    sequence: {{ _gt: {since} }}
                }},
                order: {{ sequence: ASC }},
                limit: {limit}
            ) {{
                sequence
                role
                kind
                content
                tool_call_ids
                tool_name
                bridge_call_ids
            }}
        }}
        "#,
        session = escape_graphql_string(&child.session_id),
        since = args.since_sequence,
        limit = query_limit,
    );
    let raw = node.execute_query(&query).await
        .map_err(|e| anyhow!("read_subagent_transcript query failed: {e}"))?;

    // 3. Decode rows into MessageView, hiding bridge calls.
    let views = decode_message_views(&raw)?;

    // 4. Render.
    let out = render_transcript(
        &views,
        args.since_sequence,
        RenderOptions {
            include_user_messages: args.include_user_messages,
            include_tool_results: args.include_tool_results,
            limit,
            max_chars,
        },
    );

    Ok(ReadSubagentTranscriptResponse {
        child_request_id: args.child_request_id,
        child_session_id: child.session_id,
        from_sequence: out.from_sequence,
        through_sequence: out.through_sequence,
        next_sequence: out.next_sequence,
        truncated: out.truncated,
        transcript: out.transcript,
    })
}

fn decode_message_views(raw: &serde_json::Value) -> Result<Vec<MessageView>> {
    let rows = raw.get("data").and_then(|d| d.get("AgentMessage"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("AgentMessage missing"))?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let sequence = row["sequence"].as_u64().unwrap_or(0);
        let role_str = row["role"].as_str().unwrap_or("");
        let kind_str = row["kind"].as_str().unwrap_or("ordinary");
        let body = row["content"].as_str().unwrap_or("").to_string();
        let role = match role_str {
            "user" => MessageRoleView::User,
            _ => MessageRoleView::Assistant,
        };
        let kind = match kind_str {
            "tool_result" => {
                MessageKindView::ToolResult {
                    tool_name: row["tool_name"].as_str().unwrap_or("").to_string(),
                    body,
                }
            }
            "assistant_with_tool_calls" => {
                let bridge_ids: Vec<String> = row["bridge_call_ids"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let all_ids: Vec<String> = row["tool_call_ids"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let non_bridge_count = all_ids.iter()
                    .filter(|id| !bridge_ids.contains(id))
                    .count() as u32;
                MessageKindView::AssistantWithToolCalls {
                    body,
                    tool_call_count: all_ids.len() as u32,
                    bridge_call_ids: bridge_ids,
                    non_bridge_tool_call_count: non_bridge_count,
                }
            }
            _ => MessageKindView::Ordinary { body },
        };
        views.push(MessageView { sequence, role, kind });
    }
    Ok(views)
}
```

`fetch_child_subagent_row` is a helper that queries `AgentRequest` for `child_request_id` and checks `caused_by_parent_request_id = caller_request_id`. Likely already exists from R4; reuse if so.

**Schema note on `bridge_call_ids`:** This task assumes `AgentMessage` carries a `bridge_call_ids` field that names which `tool_call_ids` are bridge rows. If that field does not exist, this task absorbs the migration: add `bridge_call_ids: [String!]` to the `AgentMessage` schema, populate it from `hook/persistence.rs` when an assistant message reserves a bridge call, and backfill via a script (the alternative is a second query against `AgentToolCall` per message, which is slower but does not require migration; pick based on prod data volume).

- [ ] **Step 4: Register and intercept**

In `tool_surface/selection.rs`:

```rust
if !selection.subagent_targets.is_empty() && selection.subagent_steering_enabled {
    register_native_tool("read_subagent_transcript", read_subagent_transcript_tool_definition());
}
```

In `hook/persistence.rs`, add the interception branch returning without persistence.

- [ ] **Step 5: Run integration tests**

Run:

```bash
cargo test -p defra-agent --test r4c_read_subagent_transcript
```

Expected: all six tests PASS.

- [ ] **Step 6: Run conformance witness tests w2, w3**

Run:

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_read_subagent_transcript
```

Expected: both `cursor_advances` and `hides_bridge_rows` witness tests PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/src/hook/persistence.rs \
       crates/defra-agent/src/tool_surface/selection.rs \
       crates/defra-agent/tests/r4c_read_subagent_transcript.rs \
       crates/defra-agent-protocol/schemas/  # if migration applied
git commit -m "$(cat <<'EOF'
Implement read_subagent_transcript agent-facing tool

Renders child session messages as compact text. Assistant-only by
default; opt-in user messages and tool-result snippets (256-byte cap).
Bridge call rows always hidden. Pagination via since_sequence cursor;
truncated flag when limit or max_chars hit.

Resolves r4c.read_subagent_transcript.cursor_advances and
r4c.read_subagent_transcript.hides_bridge_rows witnesses.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 8: Implement `read_tool_output` Tool

**Purpose:** Snapshot a backgrounded tool's stdout/stderr. Dispatch by row state: ring buffer for running, persisted `<tool-completion>` payload for terminal.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Create: `crates/defra-agent/tests/r4c_read_tool_output.rs`

**Steps:**

- [ ] **Step 1: Write integration tests (failing)**

Create the test file:

```rust
mod support;

use defra_agent::test_support::*;
use serde_json::json;

#[tokio::test]
async fn read_tool_output_running_reads_ring_buffer() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let h = ctx.background_tool(&parent, "bash", json!({
        "cmd": "for i in 1 2 3; do echo line$i; sleep 0.1; done; sleep 5"
    })).await;
    ctx.wait_for_buffer_bytes(&h, "stdout", 18).await; // "line1\nline2\nline3\n"

    let result = ctx.call_tool(&parent, "read_tool_output", json!({
        "tool_call_id": h.tool_call_id,
    })).await.expect("ok");

    assert_eq!(result["status"].as_str().unwrap(), "running");
    assert!(result["stdout"]["bytes"].as_str().unwrap().contains("line1"));
    assert!(result["stdout"]["bytes"].as_str().unwrap().contains("line3"));
    assert_eq!(result["stdout"]["truncated"].as_bool().unwrap(), false);
    assert!(result["stdout"]["total_bytes_seen"].as_u64().unwrap() >= 18);
    assert!(result["exit_code"].is_null());
}

#[tokio::test]
async fn read_tool_output_terminal_reads_persisted() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let h = ctx.background_tool(&parent, "bash", json!({
        "cmd": "echo done"
    })).await;
    ctx.wait_for_terminal(&h).await;

    let result = ctx.call_tool(&parent, "read_tool_output", json!({
        "tool_call_id": h.tool_call_id,
    })).await.expect("ok");

    assert_eq!(result["status"].as_str().unwrap(), "completed");
    assert!(result["stdout"]["bytes"].as_str().unwrap().contains("done"));
    assert_eq!(result["exit_code"].as_i64().unwrap(), 0);
}

#[tokio::test]
async fn read_tool_output_truncated_flag_on_overflow() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let h = ctx.background_tool(&parent, "bash", json!({
        "cmd": format!("head -c 300000 /dev/urandom | base64 ; sleep 5")
    })).await;
    ctx.wait_for_buffer_bytes(&h, "stdout", 262144).await; // 256 KB cap

    let result = ctx.call_tool(&parent, "read_tool_output", json!({
        "tool_call_id": h.tool_call_id,
        "max_bytes_per_stream": 262144,
    })).await.expect("ok");

    assert_eq!(result["stdout"]["truncated"].as_bool().unwrap(), true);
    assert!(result["stdout"]["total_bytes_seen"].as_u64().unwrap() > 262144);
}

#[tokio::test]
async fn read_tool_output_rejects_non_backgrounded() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let fg_call_id = ctx.run_foreground_tool(&parent, "read_file",
        json!({"path": "/etc/hostname"})).await.tool_call_id;

    let result = ctx.call_tool(&parent, "read_tool_output", json!({
        "tool_call_id": fg_call_id,
    })).await.expect("ok");
    assert_eq!(result["ok"].as_bool().unwrap(), false);
    assert_eq!(result["failure_class"].as_str().unwrap(), "argument_invalid");
}

#[tokio::test]
async fn read_tool_output_rejects_unauthorized() {
    let ctx = TestContext::start_with_default_behavior().await;
    let p1 = ctx.spawn_parent_request("p1", "go").await;
    let p2 = ctx.spawn_parent_request("p2", "go").await;
    let h = ctx.background_tool(&p2, "bash", json!({"cmd": "sleep 5"})).await;

    let result = ctx.call_tool(&p1, "read_tool_output", json!({
        "tool_call_id": h.tool_call_id,
    })).await.expect("ok");
    assert_eq!(result["ok"].as_bool().unwrap(), false);
    assert_eq!(result["failure_class"].as_str().unwrap(), "tool_not_allowed");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p defra-agent --test r4c_read_tool_output
```

Expected: compile error / not registered.

- [ ] **Step 3: Implement the handler**

```rust
use crate::background_tools::r4c_args::{
    ReadToolOutputArgs, ReadToolOutputResponse, ReadToolOutputStream,
};

pub(crate) async fn handle_read_tool_output(
    node: &EmbeddedNode,
    caller_request_id: &str,
    args: ReadToolOutputArgs,
    buffer_registry: &BackgroundToolBufferRegistry, // R6's buffer registry
) -> Result<ReadToolOutputResponse> {
    // 1. Authorize: tool_call_id must be owned by caller AND await_mode=background.
    let row = fetch_tool_call_row(node, &args.tool_call_id).await?
        .ok_or_else(|| structured_tool_not_allowed_error(
            "read_tool_output",
            "/tool_call_id",
            "tool call not found",
        ))?;
    if row.request_id != caller_request_id {
        return Err(structured_tool_not_allowed_error(
            "read_tool_output",
            "/tool_call_id",
            "tool call not owned by this parent request",
        ));
    }
    if row.await_mode != "background" {
        return Err(structured_argument_invalid_error(
            "read_tool_output",
            "/tool_call_id",
            "tool call is not backgrounded",
        ));
    }

    let max_bytes = args.validated_max_bytes() as usize;
    let (stdout, stderr, exit_code, status) = match row.state.as_str() {
        "running" => {
            let snapshot = buffer_registry.snapshot(&args.tool_call_id, max_bytes)
                .ok_or_else(|| anyhow!("buffer registry missing tool_call_id"))?;
            (
                ReadToolOutputStream {
                    bytes: utf8_tail_trim(&snapshot.stdout_tail),
                    truncated: snapshot.stdout_truncated,
                    total_bytes_seen: snapshot.stdout_total_bytes_seen,
                },
                ReadToolOutputStream {
                    bytes: utf8_tail_trim(&snapshot.stderr_tail),
                    truncated: snapshot.stderr_truncated,
                    total_bytes_seen: snapshot.stderr_total_bytes_seen,
                },
                None,
                "running".to_string(),
            )
        }
        terminal_state => {
            let persisted = fetch_persisted_tool_completion(node, &args.tool_call_id).await?
                .ok_or_else(|| anyhow!("no persisted tool completion for terminal row"))?;
            let stdout_bytes = persisted.stdout
                .chars().rev().take(max_bytes).collect::<String>()
                .chars().rev().collect::<String>();
            let stderr_bytes = persisted.stderr
                .chars().rev().take(max_bytes).collect::<String>()
                .chars().rev().collect::<String>();
            (
                ReadToolOutputStream {
                    bytes: stdout_bytes,
                    truncated: persisted.stdout_truncated,
                    total_bytes_seen: persisted.stdout_total_bytes_seen,
                },
                ReadToolOutputStream {
                    bytes: stderr_bytes,
                    truncated: persisted.stderr_truncated,
                    total_bytes_seen: persisted.stderr_total_bytes_seen,
                },
                persisted.exit_code,
                terminal_state.to_string(),
            )
        }
    };

    Ok(ReadToolOutputResponse {
        tool_call_id: args.tool_call_id,
        tool_name: row.tool_name,
        status,
        stdout,
        stderr,
        exit_code,
    })
}

/// Trim partial UTF-8 sequences at the head and tail of a byte slice's UTF-8 decoding.
fn utf8_tail_trim(bytes: &[u8]) -> String {
    // Walk forward from start to first valid UTF-8 boundary.
    let mut start = 0;
    while start < bytes.len() {
        if std::str::from_utf8(&bytes[start..]).is_ok() {
            break;
        }
        start += 1;
    }
    // Walk backward from end to last valid UTF-8 boundary.
    let mut end = bytes.len();
    while end > start {
        if std::str::from_utf8(&bytes[start..end]).is_ok() {
            break;
        }
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

#[cfg(test)]
mod utf8_trim_tests {
    use super::utf8_tail_trim;

    #[test]
    fn ascii_unchanged() {
        assert_eq!(utf8_tail_trim(b"hello world"), "hello world");
    }

    #[test]
    fn trims_partial_at_tail() {
        let s = "abc🎉".as_bytes();
        let trimmed = utf8_tail_trim(&s[..s.len() - 1]);
        assert_eq!(trimmed, "abc");
    }
}
```

`BackgroundToolBufferRegistry::snapshot` is R6's API for reading the ring buffer; if R6 named it differently, adapt to that name. `fetch_persisted_tool_completion` queries the `<tool-completion>` payload R6's projector writes — likely a DB read against an `AgentMessage` kind `tool_completion` or a side-table; consult R6's `background_completion.rs` for the exact storage.

- [ ] **Step 4: Register and intercept**

In `tool_surface/selection.rs`:

```rust
if !selection.backgroundable_tool_names.is_empty() {
    register_native_tool("read_tool_output", read_tool_output_tool_definition());
}
```

In `hook/persistence.rs`, add the interception branch.

- [ ] **Step 5: Run integration tests**

```bash
cargo test -p defra-agent --test r4c_read_tool_output
```

Expected: all five tests PASS.

- [ ] **Step 6: Run conformance witness w4**

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_read_tool_output_dispatch
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/src/hook/persistence.rs \
       crates/defra-agent/src/tool_surface/selection.rs \
       crates/defra-agent/tests/r4c_read_tool_output.rs
git commit -m "$(cat <<'EOF'
Implement read_tool_output agent-facing tool

Snapshot of stdout/stderr for a backgrounded tool. Dispatches by row
state: ring buffer for running, persisted <tool-completion> payload for
terminal. Per-stream byte cap (16 KB default, 256 KB hard cap).
total_bytes_seen monotonic counter. UTF-8 safe tail trim.

Resolves r4c.read_tool_output.dispatch_by_state witness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 9: Implement `steer_subagent` (`interrupt=false` Mode)

**Purpose:** Append the steering message to the child session as a queued `AgentRequest` with `QueueSource.steering, QueuePolicy.append`. Plus the durable `user`-role `AgentMessage` in child history.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Create: `crates/defra-agent/tests/r4c_steer_subagent.rs`

**Steps:**

- [ ] **Step 1: Write integration tests for append mode (failing)**

Create the test file:

```rust
mod support;

use defra_agent::test_support::*;
use serde_json::json;

#[tokio::test]
async fn steer_subagent_append_enqueues_with_steering_source() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;

    let result = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "also check the staging config",
        "interrupt": false,
    })).await.expect("ok");

    let queued_id = result["queued_request_id"].as_str().unwrap();
    assert!(result["interrupted_active_request_id"].is_null());
    assert!(result["drained_wake_up_request_ids"].as_array().unwrap().is_empty());

    let queued = ctx.fetch_request(queued_id).await;
    assert_eq!(queued.session_id, child.session_id);
    assert_eq!(queued.behavior_id, child.behavior_id);
    assert_eq!(queued.subagent_depth, child.subagent_depth);
    assert_eq!(queued.caused_by_parent_request_id.as_deref(), Some(parent.request_id.as_str()));
    assert!(queued.caused_by_parent_tool_call_id.is_none());
    assert_eq!(queued.metadata["queue"]["source"].as_str(), Some("steering"));
    assert_eq!(queued.metadata["queue"]["policy"].as_str(), Some("append"));
    assert_eq!(queued.status, "pending");
}

#[tokio::test]
async fn steer_subagent_append_writes_user_message() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;

    let _ = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "also check the staging config",
    })).await.expect("ok");

    let messages = ctx.fetch_messages(&child.session_id).await;
    let last_user = messages.iter().rev().find(|m| m.role == "user")
        .expect("user message appended");
    assert!(last_user.content.contains("also check the staging config"));
}

#[tokio::test]
async fn steer_subagent_rejects_terminal_child() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    ctx.terminalize_child(&child, "completed", "result").await;

    let result = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "do more",
    })).await.expect("ok");
    assert_eq!(result["ok"].as_bool().unwrap(), false);
    assert_eq!(result["failure_class"].as_str().unwrap(), "argument_invalid");
}

#[tokio::test]
async fn steer_subagent_rejects_unauthorized() {
    let ctx = TestContext::start_with_default_behavior().await;
    let p1 = ctx.spawn_parent_request("p1", "go").await;
    let p2 = ctx.spawn_parent_request("p2", "go").await;
    let child = ctx.spawn_subagent(&p2, "amy-code", "do X", "background").await;

    let result = ctx.call_tool(&p1, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "hi",
    })).await.expect("ok");
    assert_eq!(result["ok"].as_bool().unwrap(), false);
    assert_eq!(result["failure_class"].as_str().unwrap(), "tool_not_allowed");
}

#[tokio::test]
async fn steer_subagent_no_parent_tool_call_row_written() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    let _ = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "x",
    })).await.expect("ok");
    let tool_calls = ctx.fetch_tool_calls(&parent).await;
    assert!(!tool_calls.iter().any(|r| r.tool_name == "steer_subagent"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p defra-agent --test r4c_steer_subagent
```

Expected: compile error / tool not registered.

- [ ] **Step 3: Implement the append handler**

```rust
use crate::background_tools::r4c_args::{SteerSubagentArgs, SteerSubagentResponse};
use crate::lifecycle::queue::{QueueHints, QueuePolicy, QueueSource};

pub(crate) async fn handle_steer_subagent(
    node: &EmbeddedNode,
    caller_request_id: &str,
    args: SteerSubagentArgs,
) -> Result<SteerSubagentResponse> {
    // 1. Authorize: child owned by caller.
    let child = fetch_child_subagent_row(node, caller_request_id, &args.child_request_id).await?
        .ok_or_else(|| structured_tool_not_allowed_error(
            "steer_subagent", "/child_request_id",
            "child not owned by this parent request",
        ))?;

    // 2. Reject terminal child.
    if is_terminal_status(&child.status) {
        return Err(structured_argument_invalid_error(
            "steer_subagent", "/child_request_id",
            &format!("child is in terminal state '{}'; spawn a new subagent instead", child.status),
        ));
    }

    // 3. Reject foreground child.
    if child.await_mode != "background" {
        return Err(structured_tool_not_allowed_error(
            "steer_subagent", "/child_request_id",
            "foreground subagents cannot be steered; call cancel_subagent first",
        ));
    }

    let (interrupted_id, drained_ids) = if args.interrupt {
        // Task 10 fills this in. For Task 9 (interrupt=false only), return None/empty.
        return Err(anyhow!("interrupt=true not yet implemented; landing in R4c Task 10"));
    } else {
        (None::<String>, Vec::<String>::new())
    };

    // 4. Append durable user-role message to child session transcript.
    let message_id = append_user_message_to_session(node, &child.session_id, &args.message).await?;

    // 5. Compose new AgentRequest in child session.
    let queued_request_id = generate_request_id();
    create_steering_request(
        node,
        &child.session_id,
        &child.behavior_id,
        child.subagent_depth,
        caller_request_id,
        &queued_request_id,
        &args.message,
        QueueHints {
            source: QueueSource::Steering,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: None,
        },
        interrupted_id.clone(),
    ).await?;

    Ok(SteerSubagentResponse {
        child_request_id: args.child_request_id,
        child_session_id: child.session_id,
        queued_request_id,
        interrupted_active_request_id: interrupted_id,
        drained_wake_up_request_ids: drained_ids,
    })
}

fn is_terminal_status(s: &str) -> bool {
    matches!(s, "completed" | "failed" | "dead" | "interrupted" | "superseded")
}
```

The helper functions `append_user_message_to_session`, `create_steering_request`, and `QueueSource::Steering` already exist or have R4a-equivalent surface — wire to whatever the codebase exposes today. The `QueueSource::Steering` enum variant is already in the Lean model (`Proofs/Session/State.lean`) and likely already in Rust via R4a; if not, add it as a one-line enum addition.

- [ ] **Step 4: Register and intercept**

In `tool_surface/selection.rs`:

```rust
if !selection.subagent_targets.is_empty()
    && selection.subagent_steering_enabled
    && selection.subagent_background_enabled
{
    register_native_tool("steer_subagent", steer_subagent_tool_definition());
}
```

In `hook/persistence.rs`, add the interception branch.

- [ ] **Step 5: Run append tests**

```bash
cargo test -p defra-agent --test r4c_steer_subagent steer_subagent_append
cargo test -p defra-agent --test r4c_steer_subagent steer_subagent_rejects
cargo test -p defra-agent --test r4c_steer_subagent steer_subagent_no_parent
```

Expected: PASS for all append-mode tests.

- [ ] **Step 6: Run conformance witness w5**

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_steer_subagent_append
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/src/hook/persistence.rs \
       crates/defra-agent/src/tool_surface/selection.rs \
       crates/defra-agent/tests/r4c_steer_subagent.rs \
       crates/defra-agent/src/lifecycle/queue.rs  # if Steering source added
git commit -m "$(cat <<'EOF'
Implement steer_subagent append mode (interrupt=false)

Authorizes through parent-child edge; rejects terminal/foreground
children. Appends a durable user-role AgentMessage to child session;
enqueues a new AgentRequest with QueueSource.steering and
QueuePolicy.append, preserving lineage and depth. Hook-intercepted; no
parent-side AgentToolCall row.

interrupt=true mode lands in the next task.

Resolves r4c.steer_subagent.append_preserves_lineage witness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 10: Add `interrupt=true` Mode To `steer_subagent`

**Purpose:** Compose existing primitives — `interrupt` transition + `bridge_cancel_cascade` + `pendingAfterDrain` + steering append — to interrupt the child's active request and queue the steering message as replacement.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/tests/r4c_steer_subagent.rs` (add the interrupt-mode tests)

**Steps:**

- [ ] **Step 1: Add the interrupt-mode integration tests**

Append to `r4c_steer_subagent.rs`:

```rust
#[tokio::test]
async fn steer_subagent_interrupt_cancels_active_child_request() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    let active = ctx.start_child_request(&child).await; // child claims its first request

    let result = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "stop, do this instead",
        "interrupt": true,
    })).await.expect("ok");

    assert_eq!(
        result["interrupted_active_request_id"].as_str().unwrap(),
        active.request_id.as_str(),
    );
    let queued_id = result["queued_request_id"].as_str().unwrap();

    let interrupted = ctx.fetch_request(&active.request_id).await;
    assert_eq!(interrupted.status, "interrupted");

    let queued = ctx.fetch_request(queued_id).await;
    assert_eq!(queued.metadata["queue"]["source"].as_str(), Some("steering"));
    assert_eq!(
        queued.metadata["queue"]["interrupted_request_id"].as_str().unwrap(),
        active.request_id.as_str(),
    );
}

#[tokio::test]
async fn steer_subagent_interrupt_drains_automated_wakeups() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    let _active = ctx.start_child_request(&child).await;
    let grandchild = ctx.spawn_subagent_inside(&child, "amy-code", "deep", "background").await;
    ctx.terminalize_child(&grandchild, "completed", "g done").await;
    // The grandchild terminal projects a coalesced wake-up into the child session.

    let result = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "redirect",
        "interrupt": true,
    })).await.expect("ok");

    let drained = result["drained_wake_up_request_ids"].as_array().unwrap();
    assert!(!drained.is_empty(), "expected at least one drained wake-up");
}

#[tokio::test]
async fn steer_subagent_interrupt_cascades_to_grandchild_tools() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "go").await;
    let child = ctx.spawn_subagent(&parent, "amy-code", "do X", "background").await;
    let _active = ctx.start_child_request(&child).await;
    let gh_bash = ctx.background_tool_inside(&child, "bash", json!({"cmd": "sleep 30"})).await;

    let _ = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": child.request_id,
        "message": "redirect",
        "interrupt": true,
    })).await.expect("ok");

    let bash_row = ctx.fetch_tool_call(&gh_bash.tool_call_id).await;
    assert_eq!(bash_row.state, "cancelled");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p defra-agent --test r4c_steer_subagent steer_subagent_interrupt
```

Expected: tests FAIL (Task 9's handler returns the "not yet implemented" error).

- [ ] **Step 3: Replace the placeholder branch in `handle_steer_subagent`**

Replace:

```rust
let (interrupted_id, drained_ids) = if args.interrupt {
    return Err(anyhow!("interrupt=true not yet implemented; landing in R4c Task 10"));
} else {
    (None::<String>, Vec::<String>::new())
};
```

With:

```rust
let (interrupted_id, drained_ids) = if args.interrupt {
    // Find the child's active request, if any.
    let active = fetch_active_request_in_session(node, &child.session_id).await?;
    let interrupted = if let Some(a) = active {
        // Fire request interrupt (existing transition).
        interrupt_request(node, &a.request_id).await?;
        // Cascade through live tool/subagent edges (existing parametric path).
        cascade_cancel_request_descendants(node, &a.request_id).await?;
        Some(a.request_id)
    } else {
        None
    };

    // Drain pending automated wake-ups for child session.
    let queue_key = format!("background_completion:{}", child.session_id);
    let drained = drain_automated_wakeups(
        node,
        &child.session_id,
        QueueSource::SubagentCompletion, // back-compat alias of BackgroundCompletion per R6
        Some(&queue_key),
    ).await?;

    (interrupted, drained)
} else {
    (None::<String>, Vec::<String>::new())
};
```

Then, when constructing `QueueHints` for the steering request, set the `metadata.queue.interrupted_request_id` field if `interrupted_id` is `Some`. This means extending `QueueHints` with an optional field or writing the metadata directly. The cleanest path: extend `QueueHints` once and pass `interrupted_request_id: interrupted_id.clone()` into it.

The helpers `interrupt_request`, `cascade_cancel_request_descendants`, `fetch_active_request_in_session`, `drain_automated_wakeups` already exist or have direct R4/R6 surface equivalents. Wire to whatever the codebase exposes; if any is missing, add it as a thin adapter over the existing transition path, do not invent a new transition.

- [ ] **Step 4: Run interrupt-mode tests**

```bash
cargo test -p defra-agent --test r4c_steer_subagent steer_subagent_interrupt
```

Expected: all three interrupt-mode tests PASS.

- [ ] **Step 5: Re-run all r4c_steer_subagent tests**

```bash
cargo test -p defra-agent --test r4c_steer_subagent
```

Expected: all eight tests (5 from Task 9 + 3 from Task 10) PASS.

- [ ] **Step 6: Run conformance witness w6**

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_steer_subagent_interrupt_composes
```

Expected: PASS.

- [ ] **Step 7: Run Lean to confirm the Task 1 theorem still holds**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: PASS. The composition the theorem covers is now exercised by the Rust path.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/defra-agent/src/background_tools.rs \
       crates/defra-agent/tests/r4c_steer_subagent.rs \
       crates/defra-agent/src/lifecycle/queue.rs  # if QueueHints extended
git commit -m "$(cat <<'EOF'
Add steer_subagent interrupt mode (interrupt=true)

Composes existing primitives: interrupt request transition,
bridge_cancel_cascade over the interrupted request's live edges,
pendingAfterDrain for child-session automated wake-ups, then
appendPending with QueueSource.steering. Zero new transitions; B5
preservation already verified by Task 1's
steer_subagent_interrupt_preserves_link_symmetry theorem.

Resolves r4c.steer_subagent.interrupt_composes witness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 11: End-To-End R4c Integration Coverage

**Purpose:** Cover the integrated path across all five tools, registration gates, lineage scoping, and the conformance witness round-trip.

**Files:**

- Modify: `crates/defra-agent/tests/r4c_list_subagents.rs`
- Modify: `crates/defra-agent/tests/r4c_list_background_tools.rs`
- Modify: `crates/defra-agent/tests/r4c_read_subagent_transcript.rs`
- Modify: `crates/defra-agent/tests/r4c_read_tool_output.rs`
- Modify: `crates/defra-agent/tests/r4c_steer_subagent.rs`
- Modify: `crates/defra-agent/tests/support/mod.rs`

**Scenarios to add (extend existing test files; do not create new ones):**

- [ ] **Registration matrix in a new test under any existing file (place in `r4c_list_subagents.rs`):**

```rust
#[tokio::test]
async fn registration_matrix_subagent_family() {
    use defra_agent::test_support::*;

    let ctx_empty = TestContext::start_with_selection(json!({
        "subagent_targets": [],
        "subagent_spawn_enabled": false,
        "subagent_steering_enabled": false,
        "subagent_background_enabled": false,
        "backgroundable_tool_names": [],
    })).await;
    assert!(!ctx_empty.tool_registered("list_subagents"));
    assert!(!ctx_empty.tool_registered("read_subagent_transcript"));
    assert!(!ctx_empty.tool_registered("steer_subagent"));

    let ctx_targets = TestContext::start_with_selection(json!({
        "subagent_targets": ["amy-code"],
        "subagent_spawn_enabled": true,
        "subagent_steering_enabled": false,
        "subagent_background_enabled": false,
        "backgroundable_tool_names": [],
    })).await;
    assert!(ctx_targets.tool_registered("list_subagents"));
    assert!(!ctx_targets.tool_registered("read_subagent_transcript"));
    assert!(!ctx_targets.tool_registered("steer_subagent"));

    let ctx_steering = TestContext::start_with_selection(json!({
        "subagent_targets": ["amy-code"],
        "subagent_spawn_enabled": true,
        "subagent_steering_enabled": true,
        "subagent_background_enabled": true,
        "backgroundable_tool_names": [],
    })).await;
    assert!(ctx_steering.tool_registered("list_subagents"));
    assert!(ctx_steering.tool_registered("read_subagent_transcript"));
    assert!(ctx_steering.tool_registered("steer_subagent"));
}

#[tokio::test]
async fn registration_matrix_tool_family() {
    use defra_agent::test_support::*;

    let ctx_empty = TestContext::start_with_selection(json!({
        "subagent_targets": [],
        "backgroundable_tool_names": [],
    })).await;
    assert!(!ctx_empty.tool_registered("list_background_tools"));
    assert!(!ctx_empty.tool_registered("read_tool_output"));

    let ctx_bg = TestContext::start_with_selection(json!({
        "subagent_targets": [],
        "backgroundable_tool_names": ["bash"],
    })).await;
    assert!(ctx_bg.tool_registered("list_background_tools"));
    assert!(ctx_bg.tool_registered("read_tool_output"));
}
```

- [ ] **End-to-end scenario in `r4c_steer_subagent.rs`:**

```rust
#[tokio::test]
async fn end_to_end_supervise_then_steer() {
    let ctx = TestContext::start_with_default_behavior().await;
    let parent = ctx.spawn_parent_request("p", "supervise a worker").await;
    let worker = ctx.spawn_subagent(&parent, "amy-code", "long-running task", "background").await;
    let _ = ctx.background_tool_inside(&worker, "bash", json!({"cmd": "sleep 30"})).await;

    // 1. Parent lists; sees the worker.
    let list = ctx.call_tool(&parent, "list_subagents", json!({})).await.expect("ok");
    let entries = list["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);

    // 2. Parent reads transcript; gets compact text.
    ctx.append_assistant_message(&worker.session_id, "I'm working on it").await;
    let transcript = ctx.call_tool(&parent, "read_subagent_transcript", json!({
        "child_request_id": worker.request_id,
    })).await.expect("ok");
    assert!(transcript["transcript"].as_str().unwrap().contains("working on it"));

    // 3. Parent steers; new request enqueued.
    let steer = ctx.call_tool(&parent, "steer_subagent", json!({
        "child_request_id": worker.request_id,
        "message": "actually focus on Y first",
    })).await.expect("ok");
    let queued_id = steer["queued_request_id"].as_str().unwrap();

    // 4. Parent re-lists; sees worker still running and the new pending request as part of lineage.
    let list2 = ctx.call_tool(&parent, "list_subagents", json!({"status": "all"})).await
        .expect("ok");
    let ids: Vec<&str> = list2["entries"].as_array().unwrap().iter()
        .map(|e| e["child_request_id"].as_str().unwrap()).collect();
    assert!(ids.contains(&worker.request_id.as_str()));
    assert!(ids.contains(&queued_id)); // steering request is also a child of parent
}
```

- [ ] **Run all R4c integration tests**

```bash
cargo test -p defra-agent --test r4c_list_subagents
cargo test -p defra-agent --test r4c_list_background_tools
cargo test -p defra-agent --test r4c_read_subagent_transcript
cargo test -p defra-agent --test r4c_read_tool_output
cargo test -p defra-agent --test r4c_steer_subagent
```

Expected: all green.

- [ ] **Run all conformance witness tests**

```bash
cargo test -p defra-agent --test state_machine_conformance r4c_
```

Expected: six witness tests PASS.

- [ ] **Commit**

```bash
cargo fmt --all
git add crates/defra-agent/tests/r4c_*.rs \
       crates/defra-agent/tests/support/mod.rs
git commit -m "$(cat <<'EOF'
Add end-to-end R4c coverage: registration matrix and supervise-and-steer

Verifies the five-tool registration matrix per the spec's gate rules,
and an end-to-end supervise-then-steer scenario that exercises list,
read, and steer in one parent session.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## R4c Task 12: Final Polish And Full CI

**Purpose:** Run broad verification, sanity-check that no out-of-scope surface accidentally landed, update docs if code drifted from the approved design.

**Files:**

- Modify only if needed:
  - `docs/superpowers/specs/2026-05-14-r4c-background-work-management-design.md`
  - `docs/superpowers/plans/2026-05-14-r4c-background-work-management.md`
  - `crates/defra-agent/proofs/README.md`
  - `CLAUDE.md` (if the architecture summary needs an R4c mention)

**Steps:**

- [ ] **Step 1: Format and diff check**

```bash
cargo fmt --all
git diff --check
```

Expected: no whitespace errors.

- [ ] **Step 2: Run Lean**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4c-final-contract.json
```

Expected: all green.

- [ ] **Step 3: Broader CI**

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

Expected: all green.

- [ ] **Step 4: Sanity-check no accidental out-of-scope surface landed**

```bash
rg -n "list_background_work|read_background_transcript|since_byte|replace_subagent" crates/defra-agent/src crates/defra-agent/tests
```

Expected: no live tool registrations, no since_byte protocol, no replace_subagent verb. Only doc references in the spec/plan files (which discuss them as out-of-scope) are acceptable.

```bash
rg -n "deployment_id" crates/defra-agent/src
```

Expected: present in `r4c_args.rs` envelopes; populated with the local deployment id in the list handlers; no cross-deployment routing.

- [ ] **Step 5: Sanity-check no accidental subagent-named ghosts**

```bash
rg -n "subagent_completion\.rs|subagent_tools\.rs|Proofs\.Subagent" crates/defra-agent
```

Expected: zero hits in code (only in older doc files, which are immutable references).

- [ ] **Step 6: Verify ledger coverage**

```bash
cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain
```

Expected: PASS.

- [ ] **Step 7: Update `CLAUDE.md` architecture summary if needed**

If the State Machines or Document-Driven Control Plane sections summarize the bridge model, add a one-line mention of R4c management tools as glue over the existing model. Do not add a section about R4c itself; the spec and plan files are the canonical reference.

- [ ] **Step 8: Final commit**

```bash
git add docs/ CLAUDE.md crates/defra-agent/proofs/README.md
git commit -m "$(cat <<'EOF'
Polish R4c background work management implementation

Final CI, no out-of-scope surface, ledger coverage confirmed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

If no documentation changes were needed, skip the commit; there is no value in an empty polish commit.
