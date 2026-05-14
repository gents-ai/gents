# R6 Agent-Facing Tool Backgrounding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` for this plan. Execute one task at a time with a fresh implementation subagent, then a spec-compliance reviewer, then a code-quality reviewer. Do not start a later task until the current task is reviewed, formatted, verified, and committed.

**Goal:** Implement the approved R6 design in `docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md`.

Do not start coding until Jack approves this plan.

## Sequencing Prerequisite

R6 implementation **must not begin until both #190 (streaming response state machine) and #184 (compaction tool-call-pair preservation) are merged to main.** R6 cites `StreamingResponse.Status` vocabulary from #190 (for v2 forward-compat) and inherits compaction's tool-call-pair preservation for the `<tool-completion>` transcript notification (#184). Starting earlier risks vocabulary drift the spec already references.

Confirm at kickoff:

```bash
git log --oneline main | rg -i "streaming.response.*lean|#190"
git log --oneline main | rg -i "compaction.*tool.call.pair|#184"
```

Both must show merged commits before R6 Task 0 begins.

R6 ships in a single PR with the first commit being a pure rename and every subsequent commit substantive. The rename commit is the reviewer-friendliness contract from the spec.

## Cadence

For every task:

1. Spawn a fresh implementation subagent with the task section as its prompt.
2. Tell the worker: "You are not alone in the codebase. Do not revert edits made by others. Own only the files listed in this task unless you discover a blocker."
3. After the implementation pass, run:

```bash
cargo fmt --all
```

4. Spawn a fresh spec-compliance reviewer. Ask it to compare the diff against the approved R6 spec and this task.
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

## Lean Properties To Re-Verify

R6 generalizes the four existing R4 bridge transitions parametrically and adds one new theorem (B7). After every Lean task, the following must remain green; the renamed module path is `Proofs/Background/` post-Task 0:

- **B1 single-terminal:** `Proofs/Background/Properties.lean::single_terminal` (renamed; semantics preserved)
- **B2 bridge row not native-completed:** `Proofs/Background/Properties.lean::bridge_not_native_completed`
- **B3 cascade preserves child terminal:** `Proofs/Background/Properties.lean::cascade_preserves_terminal`
- **B4 depth bound (Subagent kind only):** `Proofs/Background/Properties.lean::depth_bounded_subagent`
- **B5 bridge link symmetry:** `Proofs/Background/Properties.lean::link_symmetry`
- **B6 unique call IDs:** `Proofs/Background/Properties.lean::unique_call_ids`
- **B7 per-parent budget (new):** `Proofs/Background/Properties.lean::backgrounded_budget_bounded`
- **S1, S3, S4, S5, S6** request-lifecycle safety theorems must remain green; R6 does not change request transitions
- **L1 bounded termination, L3 recovery convergence** must remain green
- **#189 recovery enumeration coverage:** `Proofs/Recovery/Properties.lean::recovery_action_coverage` must include the new `TerminalizeBackgroundedAsInterrupted` variant

Verify after every Lean task:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r6-lean-contract.json
```

---

## R6 Task 0: Pure Rename — Proofs/Subagent → Proofs/Background And Rust Mirrors

**Purpose:** Move every file and update every import without any semantic change. This commit is the no-op review gate that proves the rename is reversible and the R4 conformance witnesses are unaffected.

**Files (all moves + import path edits only — zero behavioral change):**

- Move directory: `crates/defra-agent/proofs/Proofs/Subagent/` → `crates/defra-agent/proofs/Proofs/Background/`
- Move file: `crates/defra-agent/proofs/Proofs/Subagent.lean` → `crates/defra-agent/proofs/Proofs/Background.lean`
- Move file: `crates/defra-agent/src/subagent_completion.rs` → `crates/defra-agent/src/background_completion.rs`
- Move file: `crates/defra-agent/src/subagent_tools.rs` → `crates/defra-agent/src/background_tools.rs`
- Update every Lean import that names `Proofs.Subagent.*` → `Proofs.Background.*`
- Update every Rust `mod subagent_completion` → `mod background_completion` and `use crate::subagent_completion::*` → `use crate::background_completion::*` (same for `subagent_tools`)
- Update doc references in `crates/defra-agent/proofs/README.md` and any in-source `///` doc comments that name the old module path

**Steps:**

- [ ] Confirm #190 and #184 are merged on main (see Sequencing Prerequisite).
- [ ] Inspect every consumer of the old paths:

```bash
rg -n "Proofs\.Subagent" crates/defra-agent/proofs
rg -n "subagent_completion|subagent_tools" crates/defra-agent/src crates/defra-agent/tests
```

- [ ] Perform the rename. Do NOT modify any module body, theorem statement, or Rust function signature in this commit. The diff should be entirely file moves and import-path edits.
- [ ] Build Lean and Rust, run the R4 conformance test suite. All must pass without any other change.

**Verify (CRITICAL — this gate is what makes the rename a no-op):**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --test r4_subagent_tools
cargo test -p defra-agent --test r4_subagent_completion
cargo test -p defra-agent --test subagent_source_conformance
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance
```

**Commit message:**

```text
Rename Proofs/Subagent -> Proofs/Background (pure no-op rename)
```

---

## R6 Task 1: Parameterize BridgedState Over BackgroundedKind In Lean

**Purpose:** Generalize `BridgedState` and the four bridge transitions to admit both Subagent and Tool kinds. Re-prove B1–B6 parametrically.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Background/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Bridge.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean` if exports change

**Steps:**

- [ ] Add the discriminator:

```lean
inductive BackgroundedKind where
  | Subagent
  | Tool
  deriving DecidableEq, Repr
```

- [ ] Generalize the second leg of `BridgedState`:

```lean
inductive SecondLeg where
  | subagent (child : ComposedState)
  | tool     (ctx   : ToolExecution.ToolCallContext)
```

For Tool kind, the leg's terminal is `ToolCallState`; for Subagent kind, the leg's terminal is the existing child request state.

- [ ] Define `terminalOf : SecondLeg → ChildTerminal` via kind dispatch.
- [ ] Lift the four bridge transitions in `Transition.lean` so:
  - `bridge_spawn`'s child-lineage hypotheses become a kind-conditional clause (Subagent only); Tool kind adds `awaitMode = .background` and B7 budget guard.
  - `bridge_complete`'s `pre.child.request.state = .completed` becomes `terminalOf pre.secondLeg = .completed`.
  - `bridge_failure`'s antecedent disjunction lifts the same way.
  - `bridge_cancel_cascade`'s cascade effect dispatches by kind (Subagent sets `interruptRequestedAt`; Tool's effect is a Lean predicate that the Rust executor mirrors).
- [ ] Re-prove B1–B6 parametrically. Existing proof tactics should carry; the lift is largely syntactic.
- [ ] Update `Executable.lean` to reflect the parametric form.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake build
```

**Commit message:**

```text
Parameterize BridgedState over BackgroundedKind in Lean
```

---

## R6 Task 2: Add B7 Per-Parent Budget Theorem

**Purpose:** Add the new theorem that bounds concurrent backgrounded tool rows per parent request to 8, and add its precondition to `bridge_spawn`.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Background/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Background/Basic.lean` if a `maxBackgroundedPerParent` constant is needed

**Steps:**

- [ ] Add the constant:

```lean
def maxBackgroundedPerParent : Nat := 8
```

- [ ] Add the Tool-kind clause to `bridge_spawn`'s preconditions:

```lean
-- Tool kind only: per-parent backgrounded ceiling
h_budget : kind row = .Tool →
  (pre.parent.tools.filter
    (λ t => t.awaitMode = .background ∧ ¬terminal t.state)).length
    < maxBackgroundedPerParent
```

- [ ] State and prove B7:

```lean
theorem backgrounded_budget_bounded
    (s : BridgedState) (h_reach : Reachable s) :
    (s.parent.tools.filter
      (λ t => t.awaitMode = .background ∧ ¬terminal t.state)).length
      ≤ maxBackgroundedPerParent
```

- [ ] Discharge B7 by case analysis on the most recent transition, using B6 (unique call IDs) and the `bridge_spawn` guard above.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake env lean --run Proofs/Background/Properties.lean
```

**Commit message:**

```text
Add B7 per-parent backgrounded budget theorem
```

---

## R6 Task 3: Extend Recovery Enumeration With TerminalizeBackgroundedAsInterrupted

**Purpose:** Add the v1 recovery action for backgrounded `running` rows on restart. Mirrors #189's enumeration pattern.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Recovery/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Recovery/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Recovery/Executable.lean`

**Steps:**

- [ ] Add the new `RecoveryAction` variant:

```lean
inductive RecoveryAction where
  -- existing variants...
  | TerminalizeBackgroundedAsInterrupted
```

- [ ] Add the predicate:

```lean
def isBackgroundedRunningWithLiveParent
    (row : ToolExecution.ToolCallContext) (parent : RequestContext) : Prop :=
  row.awaitMode = .background ∧
  row.state = .running ∧
  ¬terminal parent.state
```

- [ ] Add the corresponding clause to the recovery action selection function.
- [ ] Extend `recovery_action_coverage` so the new variant is reachable by some witness state.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake build
```

**Commit message:**

```text
Add TerminalizeBackgroundedAsInterrupted to recovery enumeration
```

---

## R6 Task 4: Emit R6 Conformance Witnesses

**Purpose:** Ensure Rust tests detect drift in B7 budget enforcement, recovery sweep, and the tool-completion transcript notification.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

**Steps:**

- [ ] Emit deterministic witness rows for:
  - per-parent budget admit (count < 8 → spawn allowed)
  - per-parent budget reject (count = 8 → spawn rejected with `background_tool_budget_exceeded`)
  - tool-kind bridge_complete reaches `.completed` with `result` payload
  - tool-kind bridge_failure on `.cancelled` via cascade cascade carries `parent_cancelled` reason
  - recovery sweep terminalizes backgrounded running row as `.interrupted`
  - queue source `background_completion` (new name) and back-compat read of `subagent_completion` both produce the same coalesce key
- [ ] Add `coverage_ledger` rows for each emitted group.
- [ ] Register Rust consumers in `conformance_consumers.rs`.
- [ ] Add Rust conformance tests that parse the new witnesses.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r6-contract.json
cargo test -p defra-agent --test state_machine_conformance
cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain
```

**Commit message:**

```text
Emit R6 budget, recovery, and tool-completion conformance witnesses
```

---

## R6 Task 5: Add `backgroundable` Tool Capability Bit

**Purpose:** Each `Tool` impl declares whether it is eligible for backgrounding.

**Files:**

- Modify: `crates/defra-agent/src/tool.rs` (or wherever the Rig `Tool` adapter lives)
- Modify each tool definition where `backgroundable = true` is needed: bash, MCP wrappers
- Add tests in: same modules as the tool definitions

**Steps:**

- [ ] Add the capability:

```rust
pub trait BackgroundableTool {
    fn backgroundable(&self) -> bool;
}
```

Default impl: `false`. Bash and MCP wrappers override to `true`.

- [ ] Wire the capability to tool registration so the runtime can query it.
- [ ] Add tests that assert the capability bit is `true` for bash and the MCP wrapper and `false` for `read_file`, `glob`, `grep`.

**Verify:**

```bash
cargo test -p defra-agent --lib tool
```

**Commit message:**

```text
Add backgroundable capability bit to Tool trait
```

---

## R6 Task 6: Add `backgroundable_tool_names` To ToolSelectionDocument

**Purpose:** Per-behavior operator allowlist, mirroring `subagent_targets`.

**Files:**

- Modify: `crates/defra-agent-protocol/schemas/tool_selection.graphql`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Modify: `crates/defra-agent/src/tool_surface/mod.rs`
- Modify: `crates/defra-agent/src/agent.rs`
- Modify: `crates/defra-agent-cli/src/config/tool_selection.rs` if the CLI applies tool selection
- Add tests in: `crates/defra-agent/src/tool_surface/tests.rs`

**Steps:**

- [ ] Add the new field to the GraphQL schema as `backgroundable_tool_names: [String!]!` with default `[]`.
- [ ] Plumb through `ToolSelectionDocument` parsing and runtime resolution.
- [ ] Add a runtime struct field:

```rust
#[derive(Debug, Clone, Default)]
pub(crate) struct BackgroundToolConfig {
    pub allowlist: Vec<String>,
}
```

- [ ] CLI: accept the new field in manifests under the same path as `subagent_targets`. Apply-time validation: every name in `backgroundable_tool_names` must be a registered tool whose `backgroundable()` returns `true`. Reject manifest apply otherwise.
- [ ] Add tests:
  - empty allowlist registers no R6 tools
  - non-empty allowlist registers all three R6 tools
  - allowlist containing a non-backgroundable tool name fails apply-time validation

**Verify:**

```bash
cargo test -p defra-agent --lib tool_surface
cargo test -p defra-agent-cli --lib config::tool_selection
```

**Commit message:**

```text
Add backgroundable_tool_names to ToolSelectionDocument
```

---

## R6 Task 7: Implement `background_tool` Meta-Tool

**Purpose:** Agent-facing `background_tool(tool_name, args)` creates a Tool-kind bridge row and launches the in-process executor with stdout/stderr buffering.

**Files:**

- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/runtime.rs`
- Modify: `crates/defra-agent/src/background_tools.rs` (renamed in Task 0; this task adds substance)
- Add: `crates/defra-agent/src/background_tools/buffer.rs` (in-memory ring buffer for stdout/stderr keyed by `tool_call_id`)
- Add tests in: `crates/defra-agent/tests/r6_background_tools.rs`

**Steps:**

- [ ] Define args:

```rust
#[derive(Debug, serde::Deserialize)]
struct BackgroundToolArgs {
    tool_name: String,
    args: serde_json::Value,
}
```

- [ ] Validate eligibility:
  - tool exists and `tool.backgroundable() = true`
  - `tool_name ∈ behavior.background_tool_config.allowlist`
  - parent's concurrent backgrounded-tool count ≤ 7 (B7 guard)
  - proxied tool's own argument validation passes
- [ ] Allocate `AgentToolCall` row via `ToolCallLifecycle::new_background_tool` (Tool-kind sibling of `new_subagent`):
  - `await_mode = background`
  - `cancel_policy = cascade`
  - `child_request_id = None`
- [ ] Launch the in-process executor; install a cancellation handle wired to `bridge_cancel_cascade`.
- [ ] Initialize the ring buffer for `tool_call_id` with `MAX_BACKGROUND_TOOL_OUTPUT_BYTES = 256 KB` per stream (stdout, stderr).
- [ ] Return immediately:

```json
{
  "tool_call_id": "...",
  "tool_name": "...",
  "await_mode": "background",
  "status": "running"
}
```

- [ ] Reject paths return the structured error envelopes specified in §"Authorization" and §"Budget Ceiling" of the spec.
- [ ] Add tests for: success path, unknown tool, non-backgroundable tool, allowlist-rejected tool, budget-exceeded.

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_tools background_tool
```

**Commit message:**

```text
Implement background_tool meta-tool
```

---

## R6 Task 8: Implement Hook-Intercepted `wait_tool` With Deadline-Out Cascade

**Purpose:** `wait_tool` foregrounds the existing backgrounded row without creating a new `AgentToolCall`; deadline-out fires `bridge_cancel_cascade` so the row state matches the envelope.

**Files:**

- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/background_tools.rs`
- Add tests in: `crates/defra-agent/tests/r6_background_tools.rs`

**Steps:**

- [ ] Register `wait_tool` in tool schema, but intercept it before ordinary tool-call lifecycle persistence.
- [ ] Args: `{ "tool_call_id": "..." }`.
- [ ] Authorize through the existing parent → tool-call edge.
- [ ] Behavior:
  - If row is already terminal: return its envelope immediately.
  - Otherwise wait on the bridge row terminal transition with timeout = parent_request_deadline.
  - On terminal: return the terminal envelope with `result` from the captured buffer.
  - On parent deadline: fire `bridge_cancel_cascade` on this row, await the cascaded `.cancelled` state, return `status = "cancelled"` with `error.reason = "parent_deadline_exceeded"`.
  - On parent cancellation: cascade already fires through the existing path; return `status = "cancelled"` with `error.reason = "parent_cancelled"`.
- [ ] Test that no `AgentToolCall` row exists with `tool_name = "wait_tool"` after the call.
- [ ] Test deadline-out cascade: the row's persisted state is `.cancelled` and the envelope reports `cancelled`, both consistent.

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_tools wait_tool
```

**Commit message:**

```text
Implement wait_tool with deadline-out cascade-cancel
```

---

## R6 Task 9: Implement `cancel_tool`

**Purpose:** Hook-intercepted `cancel_tool` fires `bridge_cancel_cascade` on the authorized row.

**Files:**

- Modify: `crates/defra-agent/src/background_tools.rs`
- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/interrupt.rs`
- Add tests in: `crates/defra-agent/tests/r6_background_tools.rs`

**Steps:**

- [ ] Args:

```json
{
  "tool_call_id": "...",
  "reason": "optional human-readable reason"
}
```

- [ ] Authorize through parent → tool-call edge.
- [ ] If row is already terminal: return current state envelope (no-op).
- [ ] Otherwise fire `bridge_cancel_cascade` on the Tool-kind bridge row. The cascade effect dispatches into the executor: bash subprocess → kill; MCP call → MCP cancel.
- [ ] After cascade completes (row reaches `.cancelled`), write `<tool-completion ... status="cancelled" reason="explicit_cancel">` transcript notification and enqueue coalesced wake-up under `background_completion:<parent_session_id>` (deferred until parent terminalizes per existing queue semantics).
- [ ] Return:

```json
{
  "tool_call_id": "...",
  "status": "cancelled"
}
```

- [ ] Test: cancel of running row sets state `.cancelled`, kills executor, writes notification, enqueues wake-up.

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_tools cancel_tool
cargo test -p defra-agent --lib interrupt
```

**Commit message:**

```text
Implement cancel_tool with bridge_cancel_cascade
```

---

## R6 Task 10: Generalize Background Completion Projector And Rename Queue Source

**Purpose:** Extend `background_completion.rs` (renamed in Task 0) to project tool terminals as well as subagent terminals. Rename the queue source string `subagent_completion` → `background_completion` with one-release back-compat alias.

**Files:**

- Modify: `crates/defra-agent/src/background_completion.rs`
- Modify: `crates/defra-agent/src/lifecycle/queue.rs` (queue source enum + parse alias)
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs` (observer registration)
- Modify: `crates/defra-agent/src/session/history.rs` (new `<tool-completion>` element)
- Add tests in: `crates/defra-agent/tests/r6_background_completion.rs`

**Steps:**

- [ ] Add a Tool-kind observer that watches Tool-kind bridge rows for terminal transitions.
- [ ] On Tool terminal, project bridge state:
  - `.completed` → `bridge_complete` (parametric, Tool kind) — persists captured result envelope in metadata or a side-table indexed by `tool_call_id` so `wait_tool` and `<tool-completion>` notification can read it.
  - `.failed | .cancelled | .timedOut` → `bridge_failure` with appropriate `ChildTerminal` (`Failed`, `Interrupted`, `Dead`).
- [ ] Append `<tool-completion>` user-role transcript message:

```xml
<tool-completion
  tool_call_id="..."
  tool_name="..."
  status="completed|failed|cancelled|interrupted">
  <stdout truncated="...">...</stdout>
  <stderr truncated="...">...</stderr>
  <exit_code>0</exit_code>
  <reason>parent_cancelled|parent_deadline_exceeded|...</reason>
</tool-completion>
```

- [ ] Enqueue/coalesce wake-up:

```rust
QueueHints {
    source: QueueSource::BackgroundCompletion,
    policy: QueuePolicy::Coalesce,
    key: Some(format!("background_completion:{parent_session_id}")),
    queued_after_request_id: Some(parent_request_id),
}
```

- [ ] Queue source rename:
  - Add `QueueSource::BackgroundCompletion` (the new canonical variant).
  - Keep `QueueSource::SubagentCompletion` in the enum **for one release**, but on parse map both `"subagent_completion"` and `"background_completion"` to the new canonical variant.
  - Write only `"background_completion"` going forward; never emit `"subagent_completion"`.
  - Add a deprecation log line on the parse alias hit so the count can be tracked.
- [ ] In-flight migration check: a manual test that loads an `AgentRequest` with `metadata.queue.source = "subagent_completion"` already in the DB and confirms it routes to the same coalesce key as a freshly-written `"background_completion"` row.
- [ ] Test the interleaving case from the spec: foreground parent action runs while a backgrounded tool completes; the tool notification appends immediately, the wake-up stays pending until the parent request terminalizes.

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_completion
cargo test -p defra-agent --lib lifecycle::queue
```

**Commit message:**

```text
Generalize background completion projector for tool kind
```

---

## R6 Task 11: Recovery Sweep — Terminalize Backgrounded Running Rows As Interrupted

**Purpose:** On process restart, terminalize every `awaitMode=background ∧ state=running ∧ ¬terminal parent.state` row as `.interrupted` with the v1 semantics from §"Recovery Contract".

**Files:**

- Modify: `crates/defra-agent/src/tool_call_lifecycle/recovery.rs`
- Modify: `crates/defra-agent/src/background_completion.rs`
- Add tests in: `crates/defra-agent/tests/r6_background_recovery.rs`

**Steps:**

- [ ] In `recover_all`, add a new clause that matches the predicate and applies the new `RecoveryAction::TerminalizeBackgroundedAsInterrupted` variant.
- [ ] Effect:
  - Set row state `.interrupted`.
  - Write `<tool-completion ... status="interrupted" reason="interrupted_on_restart">` with empty payload (the in-memory buffer is gone).
  - Enqueue coalesced wake-up under `background_completion:<parent_session_id>`.
- [ ] Allowlist downgrade case: if the parent behavior's `backgroundable_tool_names` no longer authorizes the row's tool, still terminalize (the row exists; the action is one-shot) and tag the notification with `reason="tool_not_allowed_at_recovery"`.
- [ ] Test cases:
  - backgrounded running + live parent → `TerminalizeBackgroundedAsInterrupted` fires
  - backgrounded running + terminal parent → existing #189 stuck-running path fires (no change)
  - backgrounded running + allowlist downgrade → terminalizes with `tool_not_allowed_at_recovery`

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_recovery
cargo test -p defra-agent --lib tool_call_lifecycle::recovery
```

**Commit message:**

```text
Add backgrounded-row recovery sweep predicate and action
```

---

## R6 Task 12: Two-Level Cascade Dispatch In Executor

**Purpose:** When a subagent reaches terminal (e.g., interrupted via its own parent's cascade), every Tool-kind bridge row on that subagent must fire `bridge_cancel_cascade` and signal its executor to cancel. Same trigger; kind-dispatched effect.

**Files:**

- Modify: `crates/defra-agent/src/background_completion.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/runtime.rs`
- Modify: `crates/defra-agent/src/interrupt.rs`
- Add tests in: `crates/defra-agent/tests/r6_background_cascade.rs`

**Steps:**

- [ ] On any request reaching terminal, enumerate Tool-kind bridge rows owned by that request whose state is non-terminal and whose `cancel_policy = cascade`. For each, fire `bridge_cancel_cascade` (Tool kind) and signal the executor.
- [ ] Executor cancel effect:
  - bash subprocess → `kill -SIGTERM` then `SIGKILL` after grace period
  - MCP call → MCP client cancel call
- [ ] Test: spawn subagent that spawns three backgrounded bash tools; cancel the subagent's parent; assert all three bash subprocesses terminate; assert all three bridge rows reach `.cancelled`; assert one `<tool-completion>` notification per tool is appended to the subagent's session; assert subagent's parent gets the cascade-cancel notification on its own bridge row.

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_cascade
```

**Commit message:**

```text
Two-level cascade dispatch through Tool-kind bridge rows
```

---

## R6 Task 13: End-To-End R6 Tool Runtime Tests

**Purpose:** Cover the integrated path across registration, hook interception, queueing, lifecycle rows, cancellation, recovery, and the tool-completion notification.

**Files:**

- Add or extend: `crates/defra-agent/tests/r6_background_tools.rs`
- Add or extend: `crates/defra-agent/tests/r6_background_completion.rs`
- Add or extend: `crates/defra-agent/tests/r6_background_recovery.rs`
- Add or extend: `crates/defra-agent/tests/r6_background_cascade.rs`
- Modify: `crates/defra-agent/tests/support/mod.rs`

**Scenarios:**

- [ ] Tool registration:
  - empty allowlist → R6 tools absent
  - non-empty allowlist → `background_tool`, `wait_tool`, `cancel_tool` registered
  - allowlist contains a non-backgroundable name → apply rejected
- [ ] `background_tool`:
  - success returns handle; row state `.running`; await_mode `.background`
  - unknown tool → structured error `tool_not_allowed`
  - non-backgroundable tool → structured error `tool_not_allowed`
  - parent at budget=8 → structured error `background_tool_budget_exceeded`; no row written
- [ ] `wait_tool`:
  - already-terminal row returns immediately
  - running row → blocks until terminal → returns terminal envelope
  - parent deadline-out → row state `.cancelled`, envelope `status=cancelled` reason `parent_deadline_exceeded`
  - parent cancel → row state `.cancelled`, envelope `status=cancelled` reason `parent_cancelled`
  - no `AgentToolCall` row with `tool_name = "wait_tool"` written
- [ ] `cancel_tool`:
  - cancels running row, signals executor, writes notification, enqueues wake-up
  - already-terminal row → no-op (returns current state)
- [ ] Background completion projection:
  - tool reaches `.completed` → bridge row `.completed` → notification with captured payload → wake-up enqueued
  - tool reaches `.failed` → bridge row `.failed` → notification with failure class → wake-up enqueued
  - wake-ups coalesce: three concurrent backgrounded tool completions in the same session produce one pending wake-up
- [ ] Recovery sweep:
  - backgrounded running row with live parent on restart → `.interrupted` notification with empty payload
  - backgrounded running row with terminal parent on restart → existing #189 path
- [ ] Two-level cascade:
  - subagent's backgrounded tools cancel when subagent's parent is cancelled
- [ ] Queue source rename:
  - in-flight `subagent_completion` row routes through back-compat alias
  - new rows write only `background_completion`

**Verify:**

```bash
cargo test -p defra-agent --test r6_background_tools
cargo test -p defra-agent --test r6_background_completion
cargo test -p defra-agent --test r6_background_recovery
cargo test -p defra-agent --test r6_background_cascade
```

**Commit message:**

```text
Add end-to-end R6 tool backgrounding coverage
```

---

## R6 Task 14: Final Polish And Full CI

**Purpose:** Run broad verification, update docs if code drifted from the approved design, and prepare the final implementation branch for review.

**Files:**

- Modify only if needed:
  - `docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md`
  - `docs/superpowers/plans/2026-05-14-tool-backgrounding.md`
  - `crates/defra-agent/proofs/README.md`
  - `CLAUDE.md` (if architecture summary needs update for the rename)

**Steps:**

- [ ] Run formatting and diff checks:

```bash
cargo fmt --all
git diff --check
```

- [ ] Run Lean:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r6-final-contract.json
```

- [ ] Run broader CI:

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

- [ ] Sanity-check no accidental v1.1 surface landed:

```bash
rg -n "read_tool_output|resumable" crates/defra-agent/src crates/defra-agent/tests
```

Expected: only spec/plan references; no registered tool, no capability bit on `Tool` trait, no resume logic in recovery.

- [ ] Sanity-check the rename is clean:

```bash
rg -n "Proofs\.Subagent|subagent_completion\.rs|subagent_tools\.rs" crates/defra-agent
```

Expected: only doc references in older specs; no live imports or module declarations.

- [ ] Update `CLAUDE.md` architecture summary if needed (e.g., add R6 to the state machine section's discussion of `BridgedState`).
- [ ] Commit final docs/polish if needed.

**Commit message:**

```text
Polish R6 tool backgrounding implementation
```
