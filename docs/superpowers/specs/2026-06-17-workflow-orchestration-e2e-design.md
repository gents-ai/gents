# Workflow orchestration: runtime-enforced primitives + north-star e2e (issue #378)

**Status:** design approved (Q1–Q3 settled), pre-implementation
**Issue:** sourcenetwork/defra-agent#378
**Branch / worktree:** `feat/workflow-orchestration-378` (`../defra-agent-orchestration-378`)
**Builds on:** #377/PR #382 (subagent substrate: spawn/wait/cancel, cross-deployment behind a flag, `Background.lean`, cascade-cancel), #513 (5-process fleet delegation e2e + harness), `Triggers.lean` (`ConcurrencyMode`). Nothing of the four primitives exists yet — no Rust, no `Proofs/Workflow/`.

## 1. Goal

Add a **workflow orchestration** layer: four runtime-enforced orchestration *primitives* an orchestrator behavior composes to coordinate a fleet of subagents. The execution DAG materializes as DefraDB lineage edges (`caused_by_parent_request_id` / `caused_by_parent_tool_call_id`); the **runtime — not the LLM — guarantees each primitive's semantics**.

1. **`fan_out_and_synthesize`** *(implement first)* — spawn N children, barrier until all terminal, then one synthesis step. Obligation: **barrier-completeness**.
2. **`pipeline`** — items flow through ordered stages, no barrier. Obligation: **no-barrier**.
3. **`verify`** — adversarial quorum; admit a finding iff a majority of independent verifiers agree. Obligation: **quorum-soundness**.
4. **`loop_until_done`** — repeat fan-out rounds until a stop predicate / budget. Obligation: **termination/liveness**.

The deliverable is **driven by an ambitious north-star e2e** — the test we *wish* passed — built by extending the #513 five-process fleet. Because orchestration is *not yet built*, the e2e is a **north-star spec, not a gap-finder**: each wall it hits names a real affordance we then build top-down, Lean-first (the #511/#513 method). The first artifact is a failing/ignored test; the missing layers tell us the build order.

## 2. The feature, precisely (what we are mirroring)

Anthropic's **dynamic workflows** in Claude Code are the canonical implementation. An orchestrator runs a script with runtime-provided primitives; the **runtime guarantees the semantics**, the LLM only decides *what* to fan out over:

- `parallel(thunks)` — **barrier**: awaits all before returning. This is `fan_out_and_synthesize`'s fan-in.
- `pipeline(items, ...stages)` — **no barrier**: each item flows through stages independently; item A can be in stage 3 while B is still in stage 1.
- **adversarial verify** — spawn k independent refuters per finding; admit by majority survivor.
- **loop-until-dry** — repeat rounds until K consecutive return nothing new / budget exhausted.
- Caps: **16 concurrent, 1000 total** per run. Intermediate results live in **script variables, not the model's context** — the conversation sees only the final answer. Resumable via cached agent results.

The six composable patterns Anthropic names: classify-and-act, fan-out-and-synthesize, adversarial verification, generate-and-filter, tournament, loop-until-done. The pi.dev ecosystem (`pi-orchestration`: `single`/`chain`/`parallel`/`fork` with per-agent-type depth caps) takes the **declarative-config** route — which is precisely the static DAG language #378 rules out of scope. Useful as contrast, not as a model.

**The defra-agent twist — the spine of this design.** Both Claude Code and pi keep orchestration state *ephemeral* (JS variables / in-process). Claude Code's barrier is `await Promise.all()` in a runtime that vanishes when the run ends. **Ours materializes the DAG as durable DefraDB lineage documents.** That changes the proof obligation in our favor:

> Claude Code enforces the barrier *operationally* (the await happens). defra-agent enforces it *and makes it independently verifiable from persisted lineage* — "the synthesis request exists only after all N children are terminal" is a **projection property over durable rows**, not an in-memory invariant. That is the defra-agent-native expression of `await parallel()`, and it is why the e2e asserts each primitive's semantics by querying rows back out — no in-process capture.

The four obligations map cleanly: **barrier-completeness** ≙ `parallel()` barrier; **no-barrier** ≙ `pipeline()` independence; **quorum-soundness** ≙ adversarial-verify majority; **termination** ≙ loop-until-dry budget.

## 3. How the substrate works today, and the gap

The subagent substrate (#377/PR #382) is **live and sufficient** as the execution layer:

- **Tool surface** (`crates/defra-agent/src/toolset/subagent.rs`): `spawn_subagent`, `wait_subagent`, `cancel_subagent`, `list_subagents`, `read_subagent`, `steer_subagent`. `SpawnSubagentArgs { name, prompt, await_mode, deadline }`.
- **Bridge** (`AgentToolCall`): one tool call ↔ one child via `child_request_id`; persisted `lifecycle_state` `pending → running → {completed, failed, timedOut, cancelled}` (`tool_call_lifecycle/transition/bridge.rs`: `bridge_complete` / `bridge_failure` / `bridge_cancel_cascade`).
- **Child materialization** (`tool_call_lifecycle/subagent_request.rs`): a `SubagentSource` observes `AgentToolCall` rows with a non-empty `child_request_id` and creates the child `AgentRequest` with lineage stamped: `caused_by_parent_request_id`, `caused_by_parent_tool_call_id`, `caused_by_trigger_id = parent_tool_call_id`, `caused_by_trigger_kind = "subagent"`, `subagent_depth = parent_depth + 1`.
- **Invariants already proven** (`Background.lean`): `inv_depth` (`maxSubagentDepth = 3`), `inv_link` (symmetric parent↔child lineage), `cascade_cancels_child` (B3), `backgrounded_budget_bounded` (B7, `maxBackgroundedPerParent = 8`).
- **Owned loop** (`agent/loop_stream.rs` `run_loop_stream`): executes tools inline; `on_tool_call` persists the bridge row, `on_tool_result` drives `bridge_complete`/`bridge_failure`.
- **Fleet e2e** (`crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs`): the #513 `five_process_filtered_conversation_delegation_live` test *already* brings up a paired 5-process fleet and fans out 4 background `spawn_subagent` calls cross-deployment, asserting child lineage **purely from durable `AgentToolCall` + `AgentRequest` rows** (`assert_child_lineage`).

**The gap.** Subagents form a *tree*; trigger fires are *independent traces*. There is **no fan-in barrier, no multi-stage composition, no quorum primitive** anywhere in the runtime or proofs. The #513 coordinator delegates and replies — there is no runtime-enforced fan-in: nothing guarantees a synthesis step waits for all N children, nothing counts verifier votes, nothing bounds a loop. The four primitives are exactly that missing layer.

## 4. The Lean we will lead with

`Proofs/Workflow/` does not exist; this arc creates it Lean-first (zero `sorry`), composing on the proven substrate rather than reinventing it. Convention (mirrors `Background.lean`/`Triggers.lean`): a barrel `Proofs/Workflow.lean` re-exports submodules; conformance witnesses register through `Proofs/Conformance/Contracts/Json/Snapshot.lean`.

- **`Workflow/FanOut.lean`** (cut 1) — model a fan-out group as a **non-empty** set (`1 ≤ N ≤ maxBackgroundedPerParent`, D6) of `Background.BridgedState` bridges sharing one orchestrator parent **and one `workflow_group_id`** (= the `fan_out_and_synthesize` tool call id, D4), plus an optional synthesis bridge tagged `workflow_role = synthesis`. Stated over the **bridge** vocabulary, not raw child states: `synthesis_spawn` is **enabled only when every group bridge's `ToolExecution.ToolCallState` is terminal** (the projected bridge state — `completed`/`failed`/`timedOut`/`cancelled`). Each child outcome is carried into the synthesis payload by the existing projection: a `completed` child via `bridge_complete`, a non-completed child via `BridgedState.terminalOf` → `ChildTerminal.projectedToolState` (D10). Theorem **`barrier_completeness`**: in any reachable state where the synthesis bridge exists, all N group bridges are terminal (no partial-fan-in synthesis); the non-empty-group hypothesis rules out the vacuous `N=0` barrier. The synthesis input is the N **structured outcomes** (success or failure record), so the theorem is over *terminality*, not *success*. Composes on `bridge_complete`/`bridge_failure`.
- **`Workflow/Pipeline.lean`** (cut 2) — items advance through ordered stages independently. Theorem **`no_barrier`**: an item's stage-(k+1) bridge can be live while another item's stage-k bridge is still live — no transition introduces cross-item synchronization. Reuses `ConcurrencyMode.parallel` semantics from `Triggers.lean`.
- **`Workflow/Quorum.lean`** (cut 3) — a finding carries k independent verifier bridges; an `admit` transition is enabled iff `survivors > k/2`. Theorem **`quorum_soundness`**: an admitted finding has a strict majority of surviving independent verifiers, and `verifier independence` (distinct bridges) is required.
- **`Workflow/Loop.lean`** (cut 4) — rounds are sequential fan-outs sharing the orchestrator (depth stays flat). A `stop` predicate or a monotone-decreasing `budget` gates the next round. Theorem **`loop_terminates`**: every run reaches a state with no enabled `next_round` transition, in ≤ `budget` rounds (liveness via a decreasing measure).

**Model/impl boundary.** Lean proves the *primitive substrate* (a barrier really waits, a pipeline really has no barrier, a quorum really counts, a loop really stops). It does **not** prove the orchestrator LLM's *choice* of what to fan out over — that is the composition, and is deliberately the model's job (mirrors Claude Code's "the script composes guaranteed primitives"). The proof boundary is the same as `Background.lean`: from the point where runtime state is visible as persisted rows.

## 5. Resolved decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | Ambition ceiling / north-star | **Stack orchestration on the #513 5-process fleet** (cross-deployment fan-out) as the eventual north-star (cut 5). It reuses the pairing/discovery/harness machinery and forces the harder affordance (barrier projection across replicated rows) rather than building a parallel single-node setup. |
| D2 | Cut-1 development target | **Single-node first, then fleet** (user steer). Develop + prove `fan_out_and_synthesize` against a single-node multi-subagent substrate (the existing `crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs` is the example), green there, *then* make the same shape green cross-deployment on the fleet. Decouples the barrier logic from P2P replication/transport timing. |
| D3 (Q1) | Primitives = tools or sub-engine | **Runtime sub-engine driven through a thin tool surface.** The orchestrator LLM makes *one* tool call (`fan_out_and_synthesize`) carrying N child tasks + a synthesis prompt; the runtime handler deterministically spawns the N bridges, enforces the barrier, and spawns synthesis. If the barrier were "call spawn N× then call synthesize," the LLM could synthesize early — the non-enforcement #378 rules out. |
| D4 (Q3) | Barrier persistence + group identity | **No new collection, but a durable group identity is required now** (review fix). Parent-request lineage alone is ambiguous if the orchestrator has *any other* subagent edge, so cut 1 adds two minimal **fields** to the bridge `AgentToolCall`: `workflow_group_id` (= the `fan_out_and_synthesize` tool call id — the durable anchor) and `workflow_role` ∈ {`fan_out_child`, `synthesis`}. The fan-out group is exactly `{ AgentToolCall : workflow_group_id == G ∧ role == fan_out_child }`. These are **runtime-written fields on an existing collection**, so the `Collection` enum + `ApplyReconcile`/`Collections.lean` parity contract (collection-level) stay **untouched**. The same `workflow_group_id` carries the round discriminator `loop_until_done` (cut 4) needs. |
| D5 | Synthesis shape | **Distinct synthesis subagent whose result returns to the orchestrator continuation** (user steer; Claude Code's `const r = await parallel(...); return synthesize(r)` shape). Synthesis is a *spawned child request* (depth+1) whose bridge is tagged `workflow_role = synthesis, workflow_group_id = G`, fed the N structured child outcomes (D10). This is what makes barrier-completeness assertable from rows (§6). The synthesis bridge's result then completes the `fan_out_and_synthesize` orchestration tool call, so the orchestrator continues its own loop. |
| D6 (Q2) | Budget / fan-out caps | Fan-out **width bounded `1 ≤ N ≤ maxBackgroundedPerParent (= 8)`** — the lower bound is load-bearing: `N = 0` yields a vacuous barrier (all-terminal trivially holds) and immediate synthesis, so cut 1 validates `N ≥ 1` at the tool boundary and `FanOut.lean` requires a non-empty group. **Depth reuses `maxSubagentDepth = 3`**: orchestrator at depth d → children + synthesis at d+1, so an orchestrator must sit at depth ≤ 2 to fan out — a real constraint we state and conformance-test. `loop_until_done` rounds are *sequential* fan-outs (depth stays flat per round) bounded by a new **workflow-level total-spawn budget** = the termination proof's `budget` (added in cut 4). |
| D7 | Inference | Real endpoint (DeepSeek, same as #513); the e2e is `#[ignore]` + env-gated (new `DEFRA_AGENT_LIVE_WORKFLOW=1`, mirroring `DEFRA_AGENT_LIVE_SUBAGENT` / `DEFRA_AGENT_LIVE_OPENAI`), not default CI. Assertions are **structural** (barrier ordering, lineage, vote counts), never exact model text. |
| D8 | Delivery | Cuts land incrementally on `feat/workflow-orchestration-378`; `fan_out_and_synthesize` (cut 1) ships first fully (Lean→conformance→Rust), the other three primitives stubbed behind the common orchestration-tool trait per #378's "stub all four, implement one." |
| D9 | This session's scope | **Three artifacts only:** this design doc, the cut plan, and the ignored/env-gated aspirational test skeleton. No Lean/Rust this session. |
| D10 | Failed-child semantics | **Structured failure record; synthesis always runs post-barrier** (review fix). A child that reaches any non-`completed` terminal — `failed`, `dead`, `interrupted`, **`superseded`** (the full `ChildTerminal.isFailure` set, `Background/State.lean:35`) — becomes a structured failure outcome in the synthesis input; the barrier counts it terminal and synthesis still runs over the N outcomes. The child→bridge projection splits by outcome: `completed` is projected by **`bridge_complete`** to bridge `lifecycle_state = completed`; every **non-completed** terminal goes through the *existing* `ChildTerminal.projectedToolState` (`Background/Bridge.lean:55`) — `interrupted → cancelled`, `failed`/`dead`/`superseded → failed` (note `projectedToolState` is defined only for the failure constructors; `completed` is *not* routed through it). Mirrors Claude Code's `parallel()` + `.filter(Boolean)` resilience, and keeps `barrier_completeness` a statement about *terminality*, not *success*. (Alternative — fail-the-primitive-without-synthesis — is rejected: it discards partial work and would couple the barrier to model output quality.) |
| D11 | Tool surface + background privilege | **New `orchestration_enabled: Boolean` field on `ToolSelection`, default off** (review fix — there is no orchestration selector today). Orchestration is a *distinct privilege* (the power to drive a fleet), so it is **not** auto-derived from subagent enablement and **not** folded into `write_tools` (file-write tools). Because fan-out spawns background-style live children counted against `maxBackgroundedPerParent`, the gate **also requires `subagent_background_enabled`**: orchestration tools are added to the per-behavior surface at reconcile time only when `orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled` (and, for fleet fan-out, `subagent_allow_cross_deployment`). `orchestration_enabled` thus grants *controlled* runtime backgrounding through the existing subagent path — it does not invent a new backgrounding mechanism. Adding a field to an existing apply-path collection touches the config renderer, not the `Collection`/parity contract. |

## 6. The north-star test shape

The aspirational test encodes the **target shape** so the first wall is visible. Per D2 the *skeleton this session* is the single-node `fan_out_and_synthesize` shape (the cut-1 wall); the cross-deployment fleet variant (D1) is the cut-5 capstone that reuses #513's `bring_up_fleet`/`establish_reconciler_pairing`.

**Single-node cut-1 shape** (`crates/defra-agent/tests/e2e_live/workflow_orchestration_live.rs`, gated `#[ignore]` + `DEFRA_AGENT_LIVE_WORKFLOW=1`):

1. One daemon; configure an **orchestrator behavior** whose tool surface includes `fan_out_and_synthesize` and a **researcher subagent behavior** (the fan-out target) + a **synthesizer behavior** (the synthesis target).
2. Drive the orchestrator with a prompt that elicits **one** `fan_out_and_synthesize` call over N=3 sub-questions with a synthesis prompt.
3. The runtime sub-engine spawns 3 researcher children, barriers, then spawns 1 synthesizer child fed the 3 answers; the synthesizer result bridges back to the orchestrator.

**Assertions — purely from durable rows (the projection).** The barrier observable is the **bridge `AgentToolCall`**, not the request: `AgentRequest` has no terminal-time field (`agent_request.graphql` — only `created_at`), whereas each bridge carries `lifecycle_state`, `started_at`, and `completed_at` (`agent_tool_call.graphql:12,13,15`). The bridge's terminal `completed_at` is precisely the moment the orchestrator *observed* the child terminal (when `bridge_complete`/`bridge_failure` fired) — the right barrier clock.
- *Group + lineage:* exactly 3 bridge `AgentToolCall`s carry `workflow_group_id = G ∧ workflow_role = fan_out_child` (G = the `fan_out_and_synthesize` tool call id); their 3 child `AgentRequest`s share `caused_by_parent_request_id = orchestrator_request` with `caused_by_trigger_kind = "subagent"`. The discriminator disambiguates from any other subagent edge on the orchestrator.
- **barrier-completeness (the headline):** the synthesis bridge (`workflow_role = synthesis, workflow_group_id = G`) exists **and** all 3 fan-out bridges are in a terminal `lifecycle_state` — the persisted lowercase set `{completed, failed, timedOut, cancelled}` (`transition/native.rs`; `AgentRequest`/bridge states are lowercase, not `Completed`) — **and** `synthesis_bridge.started_at ≥ max(fan_out_bridge.completed_at)`. On the orchestrator's authoritative view, no state is observable where the synthesis bridge exists with a non-terminal sibling.
- *Failed-child (D10):* if a fan-out child terminates non-`completed`, the barrier still admits synthesis and the synthesis input carries its structured failure record — synthesis runs over 3 outcomes regardless of per-child success.
- *Synthesis return:* the synthesis bridge result completes the `fan_out_and_synthesize` orchestration tool call (its own `completed_at` set); the orchestrator parent terminates cleanly with no orphaned bridges.
- *Budget/depth:* orchestrator at depth 0, children + synthesis at depth 1; N=3 ≤ `maxBackgroundedPerParent`.

**Cross-deployment fleet shape (cut 5):** the #513 coordinator becomes the orchestrator; the 4 subagent deployments are the fan-out targets; the coordinator learns child terminal via replication, as the existing bridge-complete path already does. Barrier-completeness is asserted on the **coordinator's authoritative view** — the node that owns the orchestrator request and drives the barrier. The cross-deployment assertion is a **convergence projection** (poll until the coordinator's rows satisfy the barrier predicate, bounded deadline), *not* a global "no peer ever observes synthesis before all terminal bridges replicate" claim: under partial replication another peer may transiently observe the synthesis bridge before a sibling's terminal update has propagated, which is not a barrier violation. The guarantee is: on the orchestrator's authoritative ordering, synthesis follows all-terminal; replicas converge to that ordering.

**The first wall is a *run* failure, not a compile failure** (review fix). Rust `#[ignore]` tests still compile with the package, and the suite is gated on the full build (§9), so the skeleton must compile clean: it uses only existing harness helpers, string tool names (`"fan_out_and_synthesize"`), and GraphQL queries over **fields that exist today**. To keep the first failure *intentional* rather than a "field does not exist" GraphQL error, the cut-0 skeleton **stages its query**: it filters `AgentToolCall` by `tool_name == "fan_out_and_synthesize"` (an existing field) and asserts an orchestration tool call is present — which fails cleanly with "no orchestration tool call observed" because the tool does not exist yet. The `workflow_group_id` / `workflow_role` projection assertions are **layered in at cut 1**, once the fields land. So the explicit run (`DEFRA_AGENT_LIVE_WORKFLOW=1 -- --ignored`) surfaces the missing orchestration affordance as a meaningful empty-result assertion, naming cut 1's first work; it never queries a not-yet-existing column.

## 7. The cuts

Each cut is Lean-fenced where it touches legal transitions/invariants, conformance-tested against the Lean contract, then Rust-satisfied. Detailed steps live in the plan doc.

### Cut 0 — Aspirational test skeleton + docs (this session, Lean-neutral)
Design doc (this file) + plan doc + the ignored/env-gated `workflow_orchestration_live.rs` skeleton encoding §6. The skeleton **compiles clean** with the package (existing helpers + string tool names + row queries) and is `#[ignore]` + `DEFRA_AGENT_LIVE_WORKFLOW`-gated, so the full suite stays green; it **fails only when explicitly run** (the tool does not exist yet). No transition/invariant touched → no Lean.

### Cut 1 — `fan_out_and_synthesize` end-to-end (the foundation)
- **Schema:** add runtime fields `workflow_group_id` + `workflow_role` to `AgentToolCall` (D4), and `orchestration_enabled: Boolean` to `ToolSelection` (D11). Field additions to existing collections — no `Collection`-enum/parity change; the `ToolSelection` field threads the config renderer + per-behavior tool-surface resolution.
- **Lean:** `Proofs/Workflow/FanOut.lean` — `barrier_completeness` (0 sorry), composing on `Background` bridges; the group is keyed by `workflow_group_id`, terminality includes failure (D10). Register conformance witnesses (legal: synthesis-after-all-terminal, incl. a failed sibling; illegal: synthesis-before-any-terminal).
- **Conformance:** Rust tests drive the witness cases against the projected barrier predicate (the `AgentToolCall.completed_at`/`lifecycle_state` projection of §6).
- **Rust:** a common `OrchestrationPrimitive` trait + the `fan_out_and_synthesize` tool (thin surface, D3), gated by `orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled` (D11) and validating `1 ≤ N ≤ maxBackgroundedPerParent` at the tool boundary (D6); a runtime sub-engine that spawns N group-tagged bridges (reusing `SubagentSource`), projects the barrier from the lowercase terminal `lifecycle_state` set on the bridges (D4), spawns the synthesis child with the N structured outcomes (D5/D10), and completes the orchestration tool call with its result. Other three primitives stubbed behind the trait.
- **Green:** single-node `workflow_orchestration_live.rs` barrier assertion passes (D2).

### Cut 2 — `pipeline`
- **Lean:** `Proofs/Workflow/Pipeline.lean` — `no_barrier`. **Conformance** + **Rust** per-item staged spawn with no fan-in.

### Cut 3 — `verify`
- **Lean:** `Proofs/Workflow/Quorum.lean` — `quorum_soundness` (majority over independent verifiers). **Conformance** + **Rust** k-verifier spawn + majority admit.

### Cut 4 — `loop_until_done`
- **Lean:** `Proofs/Workflow/Loop.lean` — `loop_terminates` (stop-predicate / decreasing budget). Introduces the workflow-level total-spawn budget (D6) and, if needed, the minimal round discriminator field (D4). **Conformance** + **Rust** round loop.

### Cut 5 — Fleet north-star green (the capstone)
Extend #513's harness: orchestrator coordinator fans out across the 4 subagent deployments; barrier-completeness projected over replicated rows; the four primitives composable end-to-end under real inference. The `five_process_workflow_orchestration_live` test goes green.

## 8. Risks

- **Real-inference elicitation is the primary flake vector.** A strongly-constrained orchestrator prompt + tool schema must reliably elicit *one* `fan_out_and_synthesize` call with N sub-tasks. Assert structure (one tool call, N children materialized, barrier ordering), never exact text. (Mirrors #513's `COORDINATOR_SYSTEM_PROMPT` discipline.)
- **Barrier projection vs. in-engine state.** D4 makes the barrier a projection, but the *sub-engine* still needs in-flight state to know when to spawn synthesis. The risk is divergence between the engine's view and the durable projection. Mitigation: the engine's spawn-synthesis trigger reads the *same* lineage predicate the proof/conformance assert — the projection is authoritative, the engine merely polls it (the #503 "projection not capture" discipline).
- **Depth-budget interplay** (D6): an orchestrator at depth 2 fanning out puts synthesis at depth 3 (the cap) — valid but tight; a synthesis that itself fans out would exceed it. Conformance must test the depth-2 orchestrator boundary explicitly.
- **Cross-deployment barrier (cut 5)** depends on child-terminal rows replicating back to the orchestrator — the same path #513's `bridge_complete` already exercises, but the *barrier* now waits on N of them. The guarantee is scoped to the **orchestrator's authoritative view**, asserted as a convergence projection (poll until the coordinator's rows satisfy the barrier, bounded deadlines, no sleeps); the spec does **not** claim global no-partial-observation across replicas mid-replication (a transient peer view of synthesis-before-sibling-terminal is replication lag, not a barrier violation).
- **Schema-field surface (cut 1).** `workflow_group_id`/`workflow_role` on `AgentToolCall` are runtime-written (no apply-path impact), but `orchestration_enabled` on `ToolSelection` threads the config renderer and per-behavior tool-surface resolution — keep both off the `Collection`-enum parity path (they are field additions, not new collections) and fence the tool-surface gating (`orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled`, D11) by conformance. The same `workflow_group_id` carries `loop_until_done`'s round discriminator (cut 4) — no further field needed.
- **Catch-up-spec hazard.** Lean leads every cut; we must not let a spec written after a Rust sketch absorb its own conclusion (the T5 incident). Each obligation is stated and proven *before* the corresponding Rust.

## 9. Sequencing & delivery

Build order **0 → 1 → 2 → 3 → 4 → 5**. Cut 1 is the load-bearing foundation; cuts 2–4 are independent primitives; cut 5 is green only once 1–4 land. Gate with the **full package suite** (`cargo test -p defra-agent`, plus `-p defra-agent-cli` for the e2e), never `--lib` (integration tests are separate compile units). Per long-plan review calibration: skip per-task code-quality reviewers, keep spec-compliance checks + one final branch review. Lean gate: `lake build` zero-sorry before each cut's Rust.

## 10. Deferred / non-goals

- A static, operator-authored declarative workflow DAG language (revisit only if the imperative-orchestrator model proves insufficient).
- Changes to the trigger engine's dispatch or concurrency modes (the primitives are a *peer* concern).
- Cross-tenant / untrusted-fleet orchestration (gated on #180, like cross-deployment subagents).
- A JS/script interpreter — the orchestrator's reasoning *is* the script.
- Multi-hop synthesis (a synthesizer that itself fans out) — out for depth-budget cleanliness.
- Default-CI hermetic inference — this is a gated live test (D7); a hermetic mock variant is a possible follow-up.
