# Workflow Orchestration north-star e2e — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a workflow-orchestration layer of four runtime-enforced primitives (`fan_out_and_synthesize`, `pipeline`, `verify`, `loop_until_done`), driven by an aspirational north-star e2e, with `fan_out_and_synthesize` implemented first end-to-end (Lean → conformance → Rust → live green).

**Architecture:** An orchestrator is itself an agent behavior; it makes *one* thin tool call per primitive and the runtime sub-engine deterministically enforces the semantics (spawn N bridges, barrier, synthesize). The execution DAG materializes as DefraDB lineage; each primitive's guarantee is a **projection property over durable `AgentToolCall`/`AgentRequest` rows**, proven in Lean against the existing `Background.lean` bridge substrate.

**Tech Stack:** Rust (runtime crate `defra-agent`), Lean 4 (`crates/defra-agent/proofs`), DefraDB (GraphQL control plane), tokio multi-thread tests.

**Design doc:** `docs/superpowers/specs/2026-06-17-workflow-orchestration-e2e-design.md` (read it first — D1–D11 are load-bearing).

## Global Constraints

- **Foundation flow is Lean → conformance → Rust.** Anything changing what transitions are legal / what invariants hold starts in `crates/defra-agent/proofs/Proofs/`, **zero `sorry`s**, before code (CLAUDE.md).
- **Gate with the full package suite** `cargo test -p defra-agent` (+ `-p defra-agent-cli` where the e2e lives), **never `--lib`** — integration tests are separate compile units.
- **Lean gate:** `cd crates/defra-agent/proofs && lake build` is green (zero sorry) before the cut's Rust.
- Always `defra_agent::graphql::escape_graphql_string()` for anything interpolated into a GraphQL string.
- **Never emit `[]` in a DefraDB mutation** — emit `field: null` for an empty list (corrupts nillable array columns otherwise).
- `tracing`, never `println!` (test `eprintln!` for diagnostics is fine).
- **Persisted `lifecycle_state` is lowercase**: `pending`/`running`/`completed`/`failed`/`timedOut`/`cancelled`. Terminal set = `{completed, failed, timedOut, cancelled}`.
- **Caps reused from `Background.lean`:** `maxSubagentDepth = 3`, `maxBackgroundedPerParent = 8`. Fan-out width bound `1 ≤ N ≤ 8`.
- Live e2e is `#[ignore]` + env-gated (`DEFRA_AGENT_LIVE_WORKFLOW=1`); assertions are **structural**, never exact model text.

---

## File Structure

**New (cut 0):**
- `crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs` — single-node aspirational test (compile-clean, run-gated).
- `crates/defra-agent/tests/e2e_live.rs` — add a `#[path]` mod line.

**New (cut 1):**
- `crates/defra-agent/proofs/Proofs/Workflow.lean` — barrel.
- `crates/defra-agent/proofs/Proofs/Workflow/FanOut.lean` — `barrier_completeness`.
- `crates/defra-agent/proofs/Proofs/Workflow/Conformance.lean` — witness cases.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Workflow.lean` — JSON serializers.
- `crates/defra-agent/src/toolset/orchestration.rs` — `OrchestrationPrimitive` trait + `fan_out_and_synthesize` tool + sub-engine (split into a `toolset/orchestration/` dir only if it grows; the toolset modules are flat, no `mod.rs`).
- `crates/defra-agent/tests/workflow_conformance.rs` — Rust conformance fence.

**Modified (cut 1):**
- `crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql` — add `workflow_group_id`, `workflow_role`.
- `crates/defra-agent-schemas/schemas/agent/tool_selection.graphql` — add `orchestration_enabled: Boolean`.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` — register `workflow_cases`.
- `crates/defra-agent/src/document_config/tool_selection.rs` + CLI config surface (`config_import.rs`, `desired_state/convert.rs`, `config_writes/tool_selection.rs`, `commands/config/tools.rs`) — thread `orchestration_enabled` (Task 1.1).
- toolset surface-build/explain path + `crates/defra-agent/src/hook/persistence/prompt_hook.rs` (route the new tool name) + `agent/loop_stream.rs` dispatch (Task 1.4).

**Later cuts (2–5):** new `Proofs/Workflow/{Pipeline,Quorum,Loop}.lean`, sibling Rust primitives behind the trait, and the fleet capstone in `crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs` (extends #513). Detailed task breakdown deferred per the instrument method (§ "Cuts 2–5").

---

## Cut 0 — Aspirational test skeleton + docs (this session, Lean-neutral)

**Goal:** A compile-clean, `#[ignore]` + `DEFRA_AGENT_LIVE_WORKFLOW`-gated single-node test encoding the `fan_out_and_synthesize` target shape, whose first explicit run fails meaningfully ("no orchestration tool call observed") — the first wall. No transition/invariant touched → no Lean.

**Files:**
- Create: `crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs`
- Modify: `crates/defra-agent/tests/e2e_live.rs`

**Interfaces:**
- Consumes: `support::create_runtime_request`; `EmbeddedNode::execute(&str)`; `defra_agent::graphql::escape_graphql_string`; `defra_agent::subagent_target_entry`; the file-local helper pattern from `subagent_delegation_live.rs` (`boot_document_agent`, `configure_behavior`, `wait_for_request_terminal`, `first_optional_row`).
- Produces: `workflow_orchestration_live::fan_out_and_synthesize_barrier_live` (the aspirational test); a `fetch_orchestration_tool_calls(node, session_id)` staged query helper that cut 1 extends with the `workflow_group_id`/`workflow_role` barrier projection.

- [ ] **Step 1: Create the test file with the env gate and module doc.**

Create `crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs`:

```rust
//! Single-node aspirational e2e for `fan_out_and_synthesize` (issue #378, cut 0).
//!
//! NORTH-STAR / FAILING BY DESIGN: the `fan_out_and_synthesize` orchestration
//! tool does not exist yet (cut 1). This test COMPILES with the package and is
//! skipped in normal runs (`#[ignore]` + early-return unless
//! `DEFRA_AGENT_LIVE_WORKFLOW=1`). When explicitly run it FAILS at the staged
//! query "no orchestration tool call observed" — naming cut 1's first work. It
//! queries only fields that exist today (it never selects `workflow_group_id`
//! before cut 1 adds it). To run:
//!
//! ```bash
//! DEFRA_AGENT_LIVE_WORKFLOW=1 \
//!   cargo test -p defra-agent --test e2e_live \
//!   workflow_orchestration_live -- --ignored --nocapture
//! ```
use std::time::Duration;

use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;

use crate::support;

const ORCH_TOOL: &str = "fan_out_and_synthesize";

fn live_enabled() -> bool {
    std::env::var("DEFRA_AGENT_LIVE_WORKFLOW").as_deref() == Ok("1")
}
```

- [ ] **Step 2: Add the staged orchestration-tool-call query helper (existing fields only).**

Append:

```rust
use defra_agent::defra_node::EmbeddedNode;   // NOT defra_agent::EmbeddedNode

#[derive(Debug, Deserialize)]
struct OrchToolCallRow {
    tool_call_id: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    child_request_id: Option<String>,
}

/// Cut-0 staged query: all `fan_out_and_synthesize` tool calls on a session
/// using ONLY fields that exist today. Returns rows so cut 1 can extend the
/// assertion with `workflow_group_id`/`workflow_role` once those columns land.
async fn fetch_orchestration_tool_calls(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<OrchToolCallRow> {
    let session = escape_graphql_string(session_id);
    let tool = escape_graphql_string(ORCH_TOOL);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "{tool}" }} }},
                order: {{ started_at: ASC }}
            ) {{ tool_call_id lifecycle_state started_at completed_at child_request_id }}
        }}"#
    );
    let resp = node.execute(&query).await;       // returns defra_node::QueryResponse
    assert!(!resp.has_errors(), "AgentToolCall query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value(row.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}
```

(`EmbeddedNode::execute` returns `defra_node::QueryResponse` with `.data: Option<Value>` + `.has_errors()` — NOT a raw `serde_json::Value`, so there is no `.pointer(...)`. Extract via `resp.data` exactly as `support::first_optional_row` does. This is the shape used in the committed skeleton below.)

- [ ] **Step 3: Write the aspirational test body (the target shape; barrier assertions staged).**

Append:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_WORKFLOW=1 and pass --ignored"]
async fn fan_out_and_synthesize_barrier_live() -> Result<()> {
    if !live_enabled() {
        eprintln!("DEFRA_AGENT_LIVE_WORKFLOW != 1; skipping workflow orchestration e2e");
        return Ok(());
    }

    // Boot a single node so the staged query runs against a real control plane.
    let db: TestDb = test_db("workflow-fanout-live").await;   // real support helper
    let session_id = "session-workflow-fanout-live";

    // ---- CUT 1 FILLS IN (the north-star setup) --------------------------------
    // ensure principal + configure orchestrator/researcher/synthesizer behaviors
    // (orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled);
    // create_runtime_request(... session_id ...) with a constrained prompt that
    // elicits ONE fan_out_and_synthesize over N=3; wait for terminal.
    // ---------------------------------------------------------------------------

    let orch_calls = fetch_orchestration_tool_calls(db.node.as_ref(), session_id).await;
    assert!(
        !orch_calls.is_empty(),
        "no `{ORCH_TOOL}` tool call observed — the orchestration primitive is not built yet (cut 1)"
    );
    Ok(())
}
```

> **Compile-clean note (verified):** the committed skeleton compiles — `cargo test -p defra-agent --test e2e_live --no-run` exits 0. It uses the real `defra_agent::defra_node::EmbeddedNode`, the real `support::{test_db, TestDb}`, and the `resp.data` extraction; it references **no** not-yet-defined Rust symbol and **no** `workflow_group_id`/`workflow_role` column. The source of truth is the committed file `crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs` — read it rather than re-typing from this block.

- [ ] **Step 4: Register the module.**

Modify `crates/defra-agent/tests/e2e_live.rs`, add after the existing mod lines:

```rust
#[path = "e2e_live/workflow_orchestration_live.rs"]
mod workflow_orchestration_live;
```

- [ ] **Step 5: Cut-0 gate — COMPILES + LISTED + skipped by default.**

Run: `cargo test -p defra-agent --test e2e_live --no-run` → Expected: exits 0 (compiles; **verified**).
Run: `cargo test -p defra-agent --test e2e_live -- --list 2>&1 | grep workflow` → Expected: the test is listed.
Run: `cargo test -p defra-agent --test e2e_live` (no env) → Expected: the workflow test is skipped (ignored + early-return); the suite stays green.

> **Scope note (review fix):** the cut-0 *gate is compile + list-only*. The skeleton boots a node and runs the staged query, but cut 0 does **not** configure inference / submit a request, so an explicit `DEFRA_AGENT_LIVE_WORKFLOW=1` run at cut 0 fails only trivially (empty DB). The **meaningful first wall** — "no `fan_out_and_synthesize` tool call observed" *after a real orchestrator request ran* — is realized in **Cut 1, Task 1.5 Step 1**, which boots+configures+submits+waits, then asserts. Do not claim a meaningful run-failure at cut 0.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs crates/defra-agent/tests/e2e_live.rs
git commit -m "test(#378): aspirational fan_out_and_synthesize e2e skeleton — compile+list-only (cut 0)"
```

---

## Cut 1 — `fan_out_and_synthesize` end-to-end (the foundation)

**Goal:** Prove `barrier_completeness` in Lean, fence it with conformance, then build the runtime sub-engine + tool so the cut-0 test goes green single-node. Stub the other three primitives behind the trait.

**Obligation:** `barrier_completeness` — in any reachable state where the synthesis bridge exists, all N group bridges are terminal (no partial-fan-in synthesis); group non-empty.
**Conformance fence:** `workflow_cases` witnesses — legal (synthesis after all terminal, incl. a `failed` sibling); illegal (synthesis before any non-terminal sibling).

### Task 1.1 — Schema fields + full ToolSelection plumbing

> **Scope (review fix):** `orchestration_enabled` is **not just schema**. `subagent_spawn_enabled` is threaded through the `ToolSelectionDocument` struct and ~7 selector/writer sites; `orchestration_enabled` must follow the identical path or it silently never loads. Use `subagent_spawn_enabled` as the exact template — grep it and mirror every hit.

**Files:**
- Modify: `crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql` (bridge fields — runtime-written, no config plumbing).
- Modify: `crates/defra-agent-schemas/schemas/agent/tool_selection.graphql` (the field).
- Modify: `crates/defra-agent/src/document_config/tool_selection.rs` — `ToolSelectionDocument` struct (`:223`, field beside `subagent_spawn_enabled` `:267`), every load/query selector (`:468`, `:523`, `:578`, `:627`) and every write/upsert site (`:714`, `:810`).
- Modify (CLI config surface): `crates/defra-agent-cli/src/config_import.rs`, `crates/defra-agent-cli/src/desired_state/convert.rs`, `crates/defra-agent-cli/src/config_writes/tool_selection.rs`, `crates/defra-agent-cli/src/commands/config/tools.rs` (and `commands/init.rs` defaults) — wherever `subagent_spawn_enabled` appears.
- Check (desktop/desired-state queries + migration): grep `subagent_spawn_enabled` across `crates/defra-agent-desktop*/` and `crates/defra-agent-cli/src/desired_state/`; default `orchestration_enabled` to `None`/false for backward-compat (existing ToolSelection rows must keep working — nullable, defaults off).

**Interfaces:**
- Produces: `AgentToolCall.workflow_group_id: String @index`, `AgentToolCall.workflow_role: String @index`; `ToolSelection.orchestration_enabled: Boolean`; `ToolSelectionDocument.orchestration_enabled: Option<bool>`.

- [ ] **Step 1: Add the bridge group/role fields.** In `agent_tool_call.graphql`, beside `child_request_id`:

```graphql
    workflow_group_id: String @index
    workflow_role: String @index
```

- [ ] **Step 2: Add the privilege field.** In `tool_selection.graphql`, beside `subagent_spawn_enabled`:

```graphql
    orchestration_enabled: Boolean
```

- [ ] **Step 3: Thread the field through `ToolSelectionDocument`.** Add `pub orchestration_enabled: Option<bool>` to the struct (`tool_selection.rs:223`) and mirror `subagent_spawn_enabled` at every selector (`:468/:523/:578/:627`) and writer (`:714/:810`). Grep to confirm zero remaining asymmetry: `grep -n subagent_spawn_enabled crates/defra-agent/src/document_config/tool_selection.rs` vs the same for `orchestration_enabled` — counts must match.
- [ ] **Step 4: Thread the CLI config surface** (import/export, desired-state convert, config_writes, `config tools`, init defaults) the same way; update the desired-state golden/snapshot tests.
- [ ] **Step 5: Write a backward-compat test:** an existing ToolSelection row with no `orchestration_enabled` loads as `None`/off (no panic, no surface change). Run → Expected: PASS.
- [ ] **Step 6: Build + full suite.** Run: `cargo build -p defra-agent-schemas && cargo test -p defra-agent -p defra-agent-cli` → Expected: PASS.
- [ ] **Step 7: Commit.** `git commit -m "feat(#378): AgentToolCall workflow group/role + ToolSelection.orchestration_enabled plumbing (cut 1)"`

### Task 1.2 — Lean: `barrier_completeness` (Lean-first; no Rust yet)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Workflow.lean`, `Proofs/Workflow/FanOut.lean`
- Modify: import the barrel where the suite root imports modules (follow `Background.lean` registration).

**Interfaces (verified against the proof tree):**
- The file is `Proofs/Background.lean` but its **namespace is `Subagent`** (`Background/State.lean:13`), so `open Subagent` after `import Proofs.Background`.
- `isTerminal` is the **generic `HasTerminal` export** (`Basic.lean:25`), and `ToolCallState` has a `HasTerminal` instance (`ToolExecution/State.lean:61`) — write `isTerminal s`, **not** `ToolCallState.isTerminal s`.
- Consumes: `Subagent` (`BridgedState`, `ChildTerminal`, `BridgedState.terminalOf`, `ChildTerminal.projectedToolState`), `Subagent.maxBackgroundedPerParent` (`State.lean:115`), `ToolExecution.ToolCallState` + its `HasTerminal` instance.
- Produces: `Workflow.FanOutGroup`, NEW glue `Workflow.bridgeToolState` / `Workflow.Reachable`, theorem `Workflow.barrier_completeness`.

- [ ] **Step 1: Author the NEW glue first** (these do not exist — they must be defined before the theorem can even typecheck). In `FanOut.lean`: `bridgeToolState : BridgedState → ToolExecution.ToolCallState` (the bridge's projected tool state: read the bridge tool's `state` from `parent.tools` at `bridgeCallId`, or project `terminalOf`/`projectedToolState` for a terminated child — match `Background/Bridge.lean`); and a `Reachable` predicate/relation = the spawn→child-step→`synthesis_spawn` closure built on `Subagent`'s `Transition`. Build them so `lake build Proofs.Workflow.FanOut` typechecks (no theorem yet).

- [ ] **Step 2: State the model + theorem.**

```lean
import Proofs.Background

/-! # Workflow.FanOut — fan_out_and_synthesize barrier-completeness (#378, cut 1).
    A fan-out group is a non-empty set of bridges sharing one workflow_group_id;
    synthesis is enabled only when every group bridge is terminal. -/
namespace Workflow

open Subagent            -- the namespace declared in Background/State.lean:13
open ToolExecution       -- for ToolCallState + its HasTerminal instance

/-- A fan-out group: the group bridges plus an optional synthesis bridge. -/
structure FanOutGroup where
  groupId    : ToolCallId
  bridges    : List BridgedState           -- the N fan-out bridges
  synthesis  : Option BridgedState          -- present once spawned
  hne        : bridges ≠ []                 -- 1 ≤ N  (rules out the vacuous barrier)

/-- Every fan-out bridge has reached a terminal bridge state (generic `isTerminal`
    via the `HasTerminal ToolCallState` instance). -/
def allTerminal (g : FanOutGroup) : Prop :=
  ∀ b ∈ g.bridges, isTerminal (bridgeToolState b)

/-- THE OBLIGATION: a synthesis bridge exists only when all fan-out bridges
    are terminal. Proven over reachable states of the spawn/complete relation. -/
theorem barrier_completeness {g : FanOutGroup} (r : Reachable g) :
    g.synthesis.isSome → allTerminal g := by
  sorry  -- discharge in Step 4; zero sorry is the gate

end Workflow
```

- [ ] **Step 3: Run the build to see the `sorry` flagged.** Run: `cd crates/defra-agent/proofs && lake build Proofs.Workflow.FanOut` → Expected: builds with only a `sorry` warning (glue typechecks; theorem stated, not proven).
- [ ] **Step 4: Discharge the proof.** Model `synthesis_spawn` so its only constructor carries `allTerminal g` as a hypothesis; then `barrier_completeness` follows by inversion on `Reachable`. Iterate until **zero sorry**.
- [ ] **Step 5: Create the barrel `Proofs/Workflow.lean`** (`import Proofs.Workflow.FanOut`) and register it where `Background.lean` is registered (the suite root import list).
- [ ] **Step 6: Full Lean build.** Run: `cd crates/defra-agent/proofs && lake build` → Expected: PASS, zero sorry.
- [ ] **Step 7: Commit.** `git commit -m "proof(#378): Workflow.FanOut barrier_completeness, zero sorry (cut 1)"`

### Task 1.3 — Conformance witnesses (the fence between model and code)

**Files:**
- Create: `Proofs/Workflow/Conformance.lean`, `Proofs/Conformance/Contracts/Json/Workflow.lean`
- Modify: `Proofs/Conformance/Contracts/Json/Snapshot.lean` (add `workflow_cases` field + import)
- Create: `crates/defra-agent/tests/workflow_conformance.rs`

**Interfaces:**
- Consumes: the witness pattern from `Proofs/Conformance/ContractCases/` + serializer helpers (`jsonString`, `jsonArray`).
- Produces: a `workflow_cases` JSON array (each: `{name, group_terminal_states: [..], synthesis_present, legal}`); Rust test `workflow_conformance::barrier_cases_match_projection`.

- [ ] **Step 1: Define witness cases in Lean.** In `Workflow/Conformance.lean`, encode at minimum: `all_terminal_then_synthesis` (legal), `failed_sibling_then_synthesis` (legal — D10), `pending_sibling_then_synthesis` (illegal), `empty_group` (illegal — N=0). Serialize via `Workflow.lean` JSON in `Contracts/Json/Workflow.lean`; register in `Snapshot.lean`.
- [ ] **Step 2: Emit + eyeball the contract JSON.** Run: `cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean | grep -A3 workflow_cases` → Expected: the cases appear.
- [ ] **Step 3: Write the Rust conformance test** that loads the contract and asserts the **projection predicate** (synthesis_present ⟹ all group states ∈ terminal set, lowercase) agrees with each witness's `legal`. Run: `cargo test -p defra-agent --test workflow_conformance` → Expected: FAIL (predicate not yet implemented).
- [ ] **Step 4: Implement the projection predicate** as a pure fn (input: group bridge states + synthesis_present; output: legal) in the runtime, used by both the test and the engine. Run the test → Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -m "proof+test(#378): Workflow conformance witnesses + barrier projection predicate (cut 1)"`

### Task 1.4 — Runtime: `fan_out_and_synthesize` tool + sub-engine

> **Existing surfaces to change (review fix) — a tool *definition* alone is inert; the durable bridge behavior the proof/test depend on lives in the persistence interception:**
> - **Tool surface build/selection/explain:** the toolset modules live flat under `crates/defra-agent/src/toolset/` (e.g. `subagent.rs`, `shared.rs` — there is **no** `toolset/mod.rs`). Add the orchestration tool to the same surface-build path that assembles `spawn_subagent` from `ToolSelection`, and to the tool *explain*/listing path, mirroring `subagent.rs`.
> - **Persistence interception:** `crates/defra-agent/src/hook/persistence/prompt_hook.rs:58` routes `on_tool_call` by name (`if tool_name == SPAWN_SUBAGENT_TOOL_NAME { persist_spawn_subagent_tool_call(...) }`). The orchestration tool must be intercepted here so its N child bridges + synthesis bridge are persisted as `AgentToolCall` rows with `workflow_group_id`/`workflow_role` — reuse `message_spawn.rs`'s spawn persistence, do not reimplement bridging.
> - **Loop dispatch:** `agent/loop_stream.rs` executes/await the tool result (the synthesis result becomes the orchestration tool call's result, D5).

**Files:**
- Create: `crates/defra-agent/src/toolset/orchestration.rs` (+ `toolset/orchestration/` submodule dir if it grows): `OrchestrationPrimitive` trait + `fan_out_and_synthesize` definition + sub-engine.
- Modify: the toolset surface-build + explain path (mirror where `subagent.rs`/`SPAWN_SUBAGENT_TOOL_NAME` are wired in); `hook/persistence/prompt_hook.rs` (route the new tool name); reuse `hook/persistence/message_spawn.rs` spawn persistence.

**Interfaces:**
- Consumes: the subagent spawn persistence (`message_spawn.rs::persist_spawn_subagent_tool_call`, `tool_call_lifecycle::subagent_request`), the barrier projection predicate (Task 1.3), `escape_graphql_string`, the bridge lifecycle (`bridge_complete`/`bridge_failure`).
- Produces: `OrchestrationPrimitive` trait; `FanOutAndSynthesize` impl; `FanOutArgs { tasks: Vec<FanOutTask>, synthesis_prompt: String }` where `FanOutTask { target_name, prompt }`; a `FAN_OUT_AND_SYNTHESIZE_TOOL_NAME` const (mirroring `SPAWN_SUBAGENT_TOOL_NAME`).

- [ ] **Step 1: Write a runtime test for N-bounds + gating** (`1 ≤ N ≤ maxBackgroundedPerParent`; tool absent from the built surface unless `orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled`). Run → Expected: FAIL (tool not defined).
- [ ] **Step 2: Define `FanOutArgs`/`FanOutTask` + the `OrchestrationPrimitive` trait + the tool-name const** in `orchestration.rs`; stub `pipeline`/`verify`/`loop_until_done` as trait impls returning "not yet implemented".
- [ ] **Step 3: Gate the tool surface** in the surface-build path where `subagent_spawn_enabled` gates `spawn_subagent`: add `fan_out_and_synthesize` only when `orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled`. Add it to the explain/listing path too.
- [ ] **Step 4: Intercept in persistence + implement the sub-engine.** In `prompt_hook.rs::on_tool_call`, route `FAN_OUT_AND_SYNTHESIZE_TOOL_NAME` to the engine. The engine: validate `1 ≤ N ≤ 8`; spawn N child bridges via `message_spawn`'s persistence, each `AgentToolCall` tagged `workflow_group_id = <this tool call id>`, `workflow_role = "fan_out_child"`; poll the barrier projection (Task 1.3) over the group's bridge `lifecycle_state`/`completed_at`; on all-terminal, spawn the synthesis child (`workflow_role = "synthesis"`, same group) with the N structured outcomes (completed via result; non-completed via `projectedToolState` mapping, D10); complete the orchestration tool call with the synthesis result.
- [ ] **Step 5: Run the runtime test** → Expected: PASS. Then `cargo test -p defra-agent` (full suite) → Expected: PASS.
- [ ] **Step 6: Commit.** `git commit -m "feat(#378): fan_out_and_synthesize tool + barrier sub-engine (cut 1)"`

### Task 1.5 — Green the cut-0 e2e (single-node)

**Files:**
- Modify: `crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs`

- [ ] **Step 1: Fill in the cut-0 setup** (boot node, orchestrator + researcher + synthesizer behaviors with the three gating flags on, `create_runtime_request` with a constrained prompt eliciting one `fan_out_and_synthesize` over N=3).
- [ ] **Step 2: Extend the staged assertion to the full barrier projection** now that the columns exist: select `workflow_group_id`/`workflow_role`; assert exactly 3 `fan_out_child` bridges in group G all terminal, 1 `synthesis` bridge, `synthesis.started_at ≥ max(fan_out.completed_at)`, and a failed child still yields synthesis (D10).
- [ ] **Step 3: Run the live e2e.** Run: `DEFRA_AGENT_LIVE_WORKFLOW=1 cargo test -p defra-agent --test e2e_live workflow_orchestration_live -- --ignored --nocapture` → Expected: PASS.
- [ ] **Step 4: Commit.** `git commit -m "test(#378): fan_out_and_synthesize barrier e2e green single-node (cut 1)"`

---

## Cuts 2–5 — Roadmap (detailed plans authored after cut 1 lands)

Per the instrument method (the design's §1 / `ambitious-e2e-as-instrument`): each cut's wall teaches the next layer's exact shape, so these get their own bite-sized planning pass once cut 1's sub-engine and projection exist. Each is named here with its obligation, fence, and files so the build order is fixed.

### Cut 2 — `pipeline` (no-barrier)
- **Obligation:** `no_barrier` — an item's stage-(k+1) bridge may be live while another item's stage-k bridge is live; no transition introduces cross-item synchronization.
- **Fence:** `pipeline_cases` witnesses (independent per-item advancement is legal; an introduced join is illegal).
- **Files:** `Proofs/Workflow/Pipeline.lean`; `orchestration/pipeline.rs`; extend `workflow_conformance.rs`.

### Cut 3 — `verify` (quorum-soundness)
- **Obligation:** `quorum_soundness` — a finding is admitted iff a strict majority of *independent* (distinct-bridge) verifiers survive.
- **Fence:** `quorum_cases` witnesses (k=3: 2/3 survive → admit; 1/3 → reject; non-independent duplicates don't count).
- **Files:** `Proofs/Workflow/Quorum.lean`; `orchestration/verify.rs`; extend `workflow_conformance.rs`.

### Cut 4 — `loop_until_done` (termination)
- **Obligation:** `loop_terminates` — every run reaches a state with no enabled `next_round` in ≤ `budget` rounds (decreasing measure). Rounds are sequential fan-outs (depth stays flat); `workflow_group_id` carries the round discriminator.
- **Fence:** `loop_cases` witnesses (stop-predicate hit → halt; budget exhausted → halt; new-findings → continue).
- **Files:** `Proofs/Workflow/Loop.lean`; the workflow-level total-spawn budget; `orchestration/loop_until_done.rs`; extend `workflow_conformance.rs`.

### Cut 5 — Fleet north-star green (capstone)
- **Goal:** Extend #513's `cli_fleet_delegation_live.rs` into `five_process_workflow_orchestration_live`: the coordinator is the orchestrator, the 4 subagent deployments are the fan-out targets; barrier-completeness asserted as a **convergence projection on the coordinator's authoritative view** (bounded-deadline polling, not global no-partial-observation).
- **Fence:** the same projection predicate, applied to replicated rows; reuse `bring_up_fleet`/`establish_reconciler_pairing`.
- **Files:** `crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs` (+ harness reuse).

---

## Self-Review

**Spec coverage:** D1 (fleet north-star)→Cut 5; D2 (single-node first)→Cut 0/1.5; D3 (sub-engine)→Task 1.4; D4 (group/role fields, no new collection)→Task 1.1/1.4; D5 (distinct synthesis returns to orchestrator)→Task 1.4 Step 3; D6 (1≤N≤8, depth)→Task 1.2/1.4; D7 (live gated)→Cut 0; D8 (incremental, stub three)→Task 1.4 Step 2; D9 (3 artifacts this session)→Cut 0 + this plan + design; D10 (structured failure, projectedToolState)→Task 1.2/1.3/1.4; D11 (orchestration_enabled ∧ spawn ∧ background)→Task 1.1/1.4. Four obligations→Tasks 1.2 (barrier) + Cuts 2/3/4.

**Placeholder scan:** Cut 0 / Cut 1 carry full code and exact commands. The Lean theorem in Task 1.2 ships with `sorry` *by design as a TDD step* (Step 2 states it, Step 4 discharges it to zero — the gate); the new `bridgeToolState`/`Reachable` glue is authored first (Step 1) so the statement typechecks. The cut-0 skeleton is **committed and compiles** (`--no-run` exits 0); the plan's code blocks match it and name it the source of truth (no `unimplemented_*` placeholder). Cuts 2–5 are intentionally roadmap-level (obligation + fence + files), not fake bite-sized code — over-specifying unbuilt cuts would be false precision; they get their own planning pass after cut 1 (the instrument method).

**Review-fix consistency (verified against code):** Lean namespace is `Subagent` (`Background/State.lean:13`), `isTerminal` is the generic `HasTerminal` export (`Basic.lean:25`) — Task 1.2 uses both correctly. `ToolSelection.orchestration_enabled` threading mirrors `subagent_spawn_enabled` across `tool_selection.rs` (`:223/:267/:468/:523/:578/:627/:714/:810`) + the CLI config surface (Task 1.1). Persistence interception is `prompt_hook.rs:58` and the toolset modules are flat (no `mod.rs`) — Task 1.4. Cut-0 gate is compile+list-only; the meaningful run-wall is Task 1.5 Step 1.

**Type consistency:** `workflow_group_id`/`workflow_role` field names, `fan_out_and_synthesize` tool name, the lowercase terminal set `{completed, failed, timedOut, cancelled}`, the `1 ≤ N ≤ 8` bound, and the `orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled` gate are used identically across Tasks 1.1–1.5 and match design D4/D6/D10/D11.
