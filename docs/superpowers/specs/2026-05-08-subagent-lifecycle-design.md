# Subagent Lifecycle — Lean Spec Design

**Status:** Design
**Date:** 2026-05-08
**Tracks:** subagent-management design branch (depends on issue 149's B1 ToolCall lifecycle spec)
**Scope:** Lean spec only. Implementation tracked in a follow-on plan.

## Background

Today, "subagent" in defra-agent is implicit-only: an agent could write a new `AgentRequest` document, and `ToolSelection.delegate_to` can route a single tool call to another DID. There is no parent ↔ child request linkage, no specialized lifecycle, and no formal model. The runtime has no notion of "the parent agent is awaiting another agent's work" beyond what falls out of session message reads.

In parallel, the issue 149 program is landing a full `ToolCall` lifecycle (B1 spec at `defra-agent-issue-149-native-glob-deadline/docs/superpowers/specs/2026-05-08-toolcall-lifecycle-spec-design.md`). The 6-state machine (`pending | running | completed | failed | timedOut | cancelled`) plus composition theorems C1, C1', C2, C3 give us a proven foundation for individual tool dispatch.

This spec extends that foundation in three orthogonal directions to handle subagents *and* backgroundable long-running tools as a single unified concept:

1. **Multi-flight.** A request may have many concurrently live tool calls.
2. **Foreground / background.** A tool call may or may not block its parent's narrative progress.
3. **Child-request linkage.** A tool call may bridge to a child `AgentRequest`, making it a subagent invocation.

The unifying observation: **a subagent IS a tool call** whose underlying execution is a child request. Long-running native tools backgrounded for ergonomics share the same lifecycle, the same handle abstraction, and the same registry. Modeling them once means one set of theorems carries both.

The design draws on a study of three external systems:
- **OpenAI Codex** — `spawn_agent` tool family, `ExecutionStatus` (`Running | Completed | Failed | Cancelled | Aborted`), explicit-close cancellation policy, `parent_thread_id` lineage, hierarchical task tree.
- **Tintinweb pi-subagents** — same-session scoped context, automatic abort cascade, max-turn graceful steering, persistent memory scopes.
- **Nicobailon pi-subagents** — separate process, real session fork, structured `Details` result, `PI_SUBAGENT_DEPTH` recursion guard, model-fallback chains.

We adopt: structured parent-child lineage with explicit fields (Codex), automatic cascade as the default with per-call opt-out (compromise between Codex's explicit and pi-subagents' automatic), depth-bound structural invariant (Nicobailon), tools-as-the-interface (Codex/Tintinweb).

Per `CLAUDE.md`'s "Lean proofs are the source of truth for all state machine behavior" rule, the design starts in the spec.

## Goals

- Add a daemon-visible model for subagent invocation by extending the `ToolCall` lifecycle, not by inventing a new state machine.
- Prove that auto-cascade cancellation reaches the child request and that detach mode does not.
- Prove a structural depth bound on subagent recursion.
- Make backgroundable long-running tools and subagents share the same handle abstraction, registry, and theorems.
- Keep the existing T1–T5 (single-machine) and C1, C1', C2, C3 (composed) properties unchanged. Adding multi-flight quantifies them ∀; it does not weaken them.

## Non-goals (out of scope for this spec)

1. **Cross-principal delegation.** Lands with sourcenetwork/defra-agent#9 (AgentPrincipal/AgentBehavior split). Schema fields here are forward-compatible; B-theorems do not change when #9 lands.
2. **Output-shape projection.** What counts as a subagent's "result" (last assistant message vs structured object vs tool-defined schema) is a runtime contract, not a state-machine concern.
3. **Token / cost budget propagation.** Parent's budget vs child's budget. Subagents inherit `deadline` and `max_tokens` from spawn args.
4. **Streaming partial output from child to parent.** B1 below only models terminal projection. Mid-run streaming is a future extension to `read_subagent_transcript`.
5. **Detach orphan reaper.** A detached subagent whose parent dies relies on its own deadline. A "lost-parent reaper" is a separate runtime concern.
6. **Subagent retry semantics.** When the parent retries its own request via `retry_parent_request`, in-flight subagents' fate (transfer / restart / orphan) is out of scope.
7. **Persistent subagents across daemon restarts.** Mirrors B4 deferral on the ToolCall spec.
8. **Steering message fairness / ordering bounds.** `send_message_to_subagent` is best-effort; no Lean property bounds delivery latency.
9. **Foreground multi-fanout.** INV-FG below pins single-flight foreground. Parallel subagents must run in background.

## Architecture

### Three orthogonal extensions to `ToolCallContext`

The 6-state `ToolCallState` from B1 is unchanged. New behavior is encoded as added fields, transitions, and invariants.

**Multi-flight.** `ComposedState.tool : Option ToolCallContext` becomes `tools : List ToolCallContext`. C1 / C1' / C2 / C3 are restated to quantify ∀ over linked-and-live tools.

**Foreground / background.** A new `awaitMode : AwaitMode` field on `ToolCallContext` and a derived "is the parent blocked" predicate at the composed layer. The parent's `RequestContext.advance` and `begin_inference` transitions add a guard: no foreground non-terminal tool may exist.

**Child-request linkage.** A new `childRequestId : Option RequestId` field on `ToolCallContext`. When non-`none`, this tool call IS a subagent invocation. The `complete` transition gains a disjunctive precondition: native tool (no child) → existing semantics; subagent → child request must be terminal *and* observed by the parent.

### Session-level registry — no new entity

The existing `AgentToolCall` collection IS the registry. A new direct `request_id` field makes "all live tool calls for request R" a single-index query. The parent's `list_tasks` tool is just a GraphQL filter.

### Lineage on `AgentRequest`

Two new fields (sibling to existing `caused_by_trigger_id`, `retry_parent_request`):
- `caused_by_parent_request_id` — immediate parent in the subagent tree.
- `caused_by_parent_tool_call_id` — the specific bridge edge.
- `subagent_depth : Int` — 0 for top-level; child = parent + 1. Supports the structural depth bound.

The existing `caused_by_trigger_kind` enum gains `"subagent"`. `caused_by_trigger_id` is **not reused** for the subagent edge — it stays for `Schedule` / `EventTrigger` / `Manual` lineage. A subagent inherits `caused_by_trigger_id` from its root parent and adds the new parent fields for the immediate edge.

## State vocabulary

```lean
inductive AwaitMode where
  | foreground   -- parent's narrative is blocked on this tool's terminal state
  | background   -- parent advances independently; tool runs concurrently
  deriving DecidableEq, Repr

inductive CancelPolicy where
  | cascade      -- parent terminal ⇒ child interrupted (default)
  | detach       -- child outlives parent; orphans bound by their own deadline
  deriving DecidableEq, Repr
```

`ToolCallState` is unchanged. New fields on `ToolCallContext`:

```lean
structure ToolCallContext where
  ...                                    -- existing B1 fields
  awaitMode      : AwaitMode             -- default .foreground
  cancelPolicy   : CancelPolicy          -- default .cascade
  childRequestId : Option RequestId      -- some iff this is a subagent invocation
  deriving Repr
```

New fields on `RequestContext`:

```lean
structure RequestContext where
  ...                                              -- existing fields
  subagentDepth                : Nat               -- 0 for top-level
  causedByParentRequestId      : Option RequestId  -- new
  causedByParentToolCallId     : Option ToolCallId -- new
  deriving Repr
```

`ComposedState.tool : Option ToolCallContext` → `tools : List ToolCallContext`.

## Cross-request modeling — `BridgedState`

Cross-request reasoning needs a paired state. Embedding child into parent recursively would balloon `ComposedState`; modeling the cross-edge as an oracle predicate is too loose. Instead:

```lean
structure BridgedState where
  parent       : ComposedState
  child        : ComposedState
  bridgeCallId : ToolCallId
  -- Structural guards (proved by construction; not preconditions):
  -- • some t ∈ parent.tools, t.callId = bridgeCallId ∧
  --     t.childRequestId = some child.request.requestId
  -- • child.request.causedByParentRequestId = some parent.request.requestId
  -- • child.request.causedByParentToolCallId = some bridgeCallId
```

Subagent fan-out (one parent, N children) is a list of `BridgedState`s sharing a parent — no recursive type. Deeper recursion (a child that has its own children) is a tree of `BridgedState`s; `subagentDepth` keeps the tree bounded.

`bridgeObservedTerminal` (used as a precondition on the bridge `complete`):
```lean
def bridgeObservedTerminal (s : BridgedState) : Prop :=
  isTerminal s.child.request.state
```

## Transitions

### Single-machine (`ToolCallContext.Transition`) additions

The B1 lifecycle's 7 state-changing constructors are unchanged. Three new mode/policy transitions advance no state:

```lean
| background
    (h_state : pre.state = .running)
    (h_mode  : pre.awaitMode = .foreground)
    (h_post  : post = { pre with awaitMode := .background })
    : Transition pre post

| foreground
    (h_state : pre.state = .running)
    (h_mode  : pre.awaitMode = .background)
    (h_post  : post = { pre with awaitMode := .foreground })
    : Transition pre post

| detach
    (h_live  : pre.state = .pending ∨ pre.state = .running)
    (h_pol   : pre.cancelPolicy = .cascade)
    (h_post  : post = { pre with cancelPolicy := .detach })
    : Transition pre post
```

The `complete` constructor is restricted to native tools. Subagent tools (where `childRequestId = some _`) reach `.completed` only through the composed-layer `bridge_complete` constructor on `BridgedState.Transition`, which has access to the child's terminal state and discharges the projection in one step:

```lean
| complete  -- native-tool completion only
    (h_state    : pre.state = .running)
    (h_persist  : pre.persistence = .committed)
    (h_native   : pre.childRequestId = none)
    (h_post     : post = { pre with state := .completed })
    : Transition pre post
```

For native tools this is exactly today's B1 `complete` semantics — no regression. The single-machine T1–T5 properties are unchanged because they reason over this inner transition. The bridge-layer `bridge_complete` provides a parallel reachability path for subagent tools, and a corresponding bridge-layer reachability theorem (B1) covers it.

Note that `detach` is structurally one-way: it's the only constructor that mutates `cancelPolicy`, and its precondition `pre.cancelPolicy = .cascade` plus its effect `.detach` mean a tool can flip cascade → detach but never back. No separate "irreversibility" theorem is needed — it falls out of constructor exhaustiveness.

### Composed-layer (`ComposedState.Transition`) modifications

`tool_step` now operates on a `tools : List`; `pre.tools` and `post.tools` differ by exactly the one tool the inner `ToolCallContext.Transition` applies to.

The `RequestContext.advance` and `begin_inference` lifts at the composed layer add a guard:

```lean
no_blocking_foreground :
    ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧ ¬ isTerminal t.state
```

This is what makes "parent blocks on foreground" structural.

### Bridge layer (`BridgedState.Transition`)

```lean
inductive BridgedState.Transition : BridgedState → BridgedState → Prop where

  | parent_step {pre post}
      (h               : ComposedState.Transition pre.parent post.parent)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      (h_link_preserved : -- structural guards stay intact
                          link post.parent post.bridgeCallId
                               post.child.request.requestId)
      : Transition pre post

  | child_step {pre post}
      (h               : ComposedState.Transition pre.child post.child)
      (h_parent_eq     : post.parent = pre.parent)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_spawn {pre post} {newTool : ToolCallContext}
      (h_proc           : pre.parent.request.state = .processing)
      (h_depth          : pre.parent.request.subagentDepth + 1 ≤ maxSubagentDepth)
      (h_callId_fresh   : ∀ t ∈ pre.parent.tools, t.callId ≠ post.bridgeCallId)
      (h_newTool_shape  : newTool.callId = post.bridgeCallId ∧
                           newTool.state = .pending ∧
                           newTool.childRequestId = some post.child.requestId)
      (h_tools_append   : post.parent.tools = pre.parent.tools ++ [newTool])
      (h_request_eq     : post.parent.request = pre.parent.request)
      (h_child_pending  : post.child.request.state = .pending ∧
                           post.child.request.interruptRequestedAt = none ∧
                           post.child.request.causedByParentRequestId =
                             some pre.parent.requestId ∧
                           post.child.request.causedByParentToolCallId =
                             some post.bridgeCallId ∧
                           post.child.request.subagentDepth =
                             pre.parent.request.subagentDepth + 1)
      (h_child_no_tools : post.child.tools = [])
      : Transition pre post

  | bridge_complete {pre post} {idx : Nat} {tPre tPost : ToolCallContext}
      (h_child_done     : pre.child.request.state = .completed)
      (h_idx            : pre.parent.tools[idx]? = some tPre)
      (h_tPre_shape     : tPre.callId = pre.bridgeCallId ∧
                           tPre.state = .running ∧
                           tPre.persistence = .committed ∧
                           tPre.childRequestId = some pre.child.requestId)
      (h_tPost_shape    : tPost.callId = pre.bridgeCallId ∧
                           tPost.state = .completed ∧
                           tPost.childRequestId = some pre.child.requestId)
      (h_tools_set      : post.parent.tools = pre.parent.tools.set idx tPost)
      (h_request_eq     : post.parent.request = pre.parent.request)
      (h_child_eq       : post.child = pre.child)
      (h_bridgeId_eq    : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_failure {pre post} {idx : Nat} {tPre tPost : ToolCallContext}
      (h_child_term     : pre.child.request.state = .failed ∨
                           pre.child.request.state = .dead ∨
                           pre.child.request.state = .interrupted ∨
                           pre.child.request.state = .superseded)
      (h_idx            : pre.parent.tools[idx]? = some tPre)
      (h_tPre_shape     : tPre.callId = pre.bridgeCallId ∧
                           tPre.state = .running ∧
                           tPre.childRequestId = some pre.child.requestId)
      (h_tPost_shape    : tPost.callId = pre.bridgeCallId ∧
                           (tPost.state = .failed ∨ tPost.state = .cancelled))
      (h_tools_set      : post.parent.tools = pre.parent.tools.set idx tPost)
      (h_request_eq     : post.parent.request = pre.parent.request)
      (h_child_eq       : post.child = pre.child)
      (h_bridgeId_eq    : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_cancel_cascade {pre post}
      (h_parent_term    : isTerminal pre.parent.request.state ∨
                          ∃ t ∈ pre.parent.tools,
                            t.callId = pre.bridgeCallId ∧ t.state = .cancelled)
      (h_cascade_pol    : ∃ t ∈ pre.parent.tools,
                            t.callId = pre.bridgeCallId ∧
                            t.cancelPolicy = .cascade)
      (h_interrupt_set  : post.child.request.interruptRequestedAt.isSome)
      (h_parent_eq      : post.parent = pre.parent)
      (h_child_tools_eq : post.child.tools = pre.child.tools)
      : Transition pre post
```

Six bridge transitions: `parent_step`, `child_step`, `bridge_spawn`, `bridge_complete`, `bridge_failure`, `bridge_cancel_cascade`. The split between `bridge_complete` (child `.completed`) and `bridge_failure` (child non-`.completed` terminal) keeps the projection explicit on both sides.

**Constructor shapes.** `bridge_spawn` is *append-style*: `post.parent.tools = pre.parent.tools ++ [newTool]` with a freshness precondition (`newTool.callId` not present in pre-state) so the new callId can't collide. `bridge_complete` and `bridge_failure` are *set-style* (mirror of `tool_step`): an explicit index `idx` with `pre.parent.tools[idx]? = some tPre` and `post.parent.tools = pre.parent.tools.set idx tPost`. These tightenings rule out adversarial duplicate-callId tools and make `INV-UNIQUE` (below) preservable across every transition.

Note that `bridge_cancel_cascade` only sets `interruptRequestedAt` on the child; the child's actual transition to `.interrupted` happens through the existing `interrupt_processing` constructor on `RequestContext.Transition` lifted via `child_step`. The cascade is two halves, and B3 (below) bundles them into a trace.

## Properties

Invariants (proven structurally; no `sorry`):

```lean
/-- INV-FG: at most one foreground non-terminal tool per request. -/
theorem invFG_preserved
    {pre post : ComposedState}
    (h_inv  : pre.invFG)
    (h_step : Transition pre post) :
    post.invFG
  -- where invFG s := (s.tools.filter
  --                    (fun t => t.awaitMode = .foreground ∧ ¬ isTerminal t.state)).length ≤ 1

/-- INV-UNIQUE: every tool in `tools` has a distinct callId. Established at
    spawn (bridge_spawn carries h_callId_fresh) and preserved by every
    Composed.Transition; lifts to BridgedState (parent and child sides)
    via `bridgedUniqueCallIds_preserved`. Consumed by B3' to discharge
    the bridge_cancel_cascade case without an external uniqueness hypothesis. -/
theorem uniqueCallIds_preserved
    {pre post : ComposedState}
    (h_inv  : pre.UniqueCallIds)
    (h_step : Transition pre post) :
    post.UniqueCallIds
  -- where UniqueCallIds s :=
  --   ∀ i j, ∀ (h_i : i < s.tools.length) (h_j : j < s.tools.length),
  --     s.tools[i].callId = s.tools[j].callId → i = j

theorem bridgedUniqueCallIds_preserved
    {pre post : BridgedState}
    (h_parent_inv : pre.parent.UniqueCallIds)
    (h_child_inv  : pre.child.UniqueCallIds)
    (h_step : Transition pre post) :
    post.parent.UniqueCallIds ∧ post.child.UniqueCallIds

/-- INV-DEPTH: subagent depth never exceeds the configured cap on any reachable state. -/
theorem inv_depth
    (pre post : BridgedState)
    (h_init  : pre.parent.request.subagentDepth ≤ maxSubagentDepth ∧
               pre.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_trace : Trace pre post) :
    post.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    post.child.request.subagentDepth ≤ maxSubagentDepth

/-- INV-LINK: parent ↔ child references stay symmetric. -/
theorem inv_link (s : BridgedState) :
    (∃ t ∈ s.parent.tools, t.callId = s.bridgeCallId ∧
                            t.childRequestId = some s.child.request.requestId) ∧
    s.child.request.causedByParentRequestId = some s.parent.request.requestId ∧
    s.child.request.causedByParentToolCallId = some s.bridgeCallId
```

Bridge theorems:

```lean
/-- B1: A child Request reaching .completed propagates to parent ToolCall .completed.
    Liveness conditional: requires bounded time advance for parent to observe.
    The running-tool witness bundles persistence and child-link facts so the
    bridge_complete construction has everything it needs without a uniqueness
    side condition. -/
theorem bridged_child_completion_propagates
    (pre : BridgedState)
    (h_running    : ∃ t ∈ pre.parent.tools,
                      t.callId = pre.bridgeCallId ∧
                      t.state = .running ∧
                      t.persistence = .committed ∧
                      t.childRequestId = some pre.child.requestId)
    (h_child_done : pre.child.request.state = .completed) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧ t.state = .completed

/-- B2: A child failure projects to parent ToolCall failure or cancellation. -/
theorem bridged_child_failure_projects
    (pre : BridgedState)
    (h_running    : ∃ t ∈ pre.parent.tools, t.callId = pre.bridgeCallId ∧ t.state = .running)
    (h_child_term : pre.child.request.state = .failed ∨
                    pre.child.request.state = .dead ∨
                    pre.child.request.state = .interrupted ∨
                    pre.child.request.state = .superseded) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧
              (t.state = .failed ∨ t.state = .cancelled)

/-- B3: Cascade cancel correctness. Parent terminal under cascade ⇒ child interrupted. -/
theorem cascade_cancels_child
    (pre : BridgedState)
    (h_parent_term : isTerminal pre.parent.request.state)
    (h_cascade     : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.cancelPolicy = .cascade ∧
                       ¬ isTerminal t.state) :
    ∃ post, Trace pre post ∧ post.child.request.state = .interrupted

/-- B3': Detach correctness (negative form). Detach mode does NOT cascade.
    Consumes the structural INV-UNIQUE invariant on the parent's tools to
    derive same-tool from same-callId in the bridge_cancel_cascade case;
    no external uniqueness hypothesis required. -/
theorem detach_does_not_cancel_child
    (pre post : BridgedState)
    (h_detach    : ∃ t ∈ pre.parent.tools,
                     t.callId = pre.bridgeCallId ∧ t.cancelPolicy = .detach)
    (h_step      : Transition pre post)
    (h_no_other  : ¬ pre.child.request.interruptRequestedAt.isSome)
    (h_uniq      : pre.parent.UniqueCallIds) :
    post.child.request.interruptRequestedAt = pre.child.request.interruptRequestedAt

/-- B4: Subagent depth bound. Restated standalone for prominence; same content as INV-DEPTH. -/
theorem subagent_depth_bounded := inv_depth

/-- B5: Link symmetry restated. Same as INV-LINK. -/
theorem bridge_link_symmetric := inv_link

/-- B6: Foreground blocking. Parent's seq numbers don't advance while a foreground tool is live. -/
theorem foreground_blocks_parent_advance
    (pre post : BridgedState)
    (h_fg     : ∃ t ∈ pre.parent.tools,
                  t.awaitMode = .foreground ∧ ¬ isTerminal t.state)
    (h_step   : Transition pre post) :
    pre.parent.request.progressSeq = post.parent.request.progressSeq ∧
    pre.parent.request.messageSeq  = post.parent.request.messageSeq
```

### What B-theorems deliberately don't claim

- **No automatic propagation across detach.** B1 requires the cascade path or active observation. A detached child's terminal state may go unobserved by its (now-gone) parent — that's the point of detach.
- **No bound on observation latency.** B1 says "eventually, given trace continuation"; bounded latency would be a fairness assumption on the runtime scheduler, which is out of scope.
- **No theorem about steering message ordering.** `send_message_to_subagent` writes a session message; the child's pickup is bounded only by its own progress (S3 on the child).

### Why S3 (`progress_monotonic`) does not need to weaken

`progress_monotonic` is local to one `RequestContext.Transition`: `post.progressSeq ≥ pre.progressSeq`. Existing `Composed.tool_step` requires `post.request = pre.request`, so any tool-level transition (including a backgrounded tool flipping to terminal) leaves the parent's `progressSeq` alone. S3 holds across `tool_step` events trivially. The new `background` / `foreground` / `detach` mode-changes and the `bridge_complete` projection all stay inside the tool layer; none modify `RequestContext`. B6 makes this layer-separation a theorem rather than a structural artifact.

What backgrounding *does* break — but neither is currently a Lean property:
1. *"Tool result appears immediately after its tool-call request in the parent's narrative."* Backgrounded tools intentionally violate this. Not a theorem.
2. *"`messageSeq` order matches the wall-clock causal order of underlying events."* Two backgrounded completions race to write into session message rows in arbitrary order. `messageSeq` is monotonic by *append order*, which is preserved.

## Tool surface

Tools injected into the parent's tool list when subagent capability is enabled in behavior config:

| Tool | Args | Returns | Effect |
|---|---|---|---|
| `spawn_subagent` | `behavior_id`, `prompt`, `await_mode: foreground|background`, `cancel_policy: cascade|detach`, `tool_allowlist?`, `deadline?`, `max_tokens?` | `{ tool_call_id, child_request_id }` | Fires `bridge_spawn`. |
| `wait_task` | `tool_call_id` | terminal state + projected output | Fires `foreground` mode-change. |
| `get_task_result` | `tool_call_id` | `{ state, output?, partial? }` | Read-only snapshot; advances parent's `messageSeq` by 1 (ordinary observation). |
| `read_subagent_transcript` | `tool_call_id`, `since_message_seq?` | list of session messages | Read-only `child_step`. |
| `send_message_to_subagent` | `tool_call_id`, `message` | ack | Writes a SessionMessage on the child's session (modeled as `child_step`). |
| `cancel_task` | `tool_call_id` | ack | Drives parent ToolCall to `.cancelled`; fires cascade if `cancelPolicy = cascade`. |
| `list_tasks` | `filter: live|terminal|all`, `limit` | list of handles | GraphQL filter on `AgentToolCall`. |
| `background_task` | `tool_call_id` | ack | Fires `background` mode-change. |

The first three are required for any subagent capability. `read_subagent_transcript` and `send_message_to_subagent` are gated separately (steering surface). `cancel_task`, `list_tasks`, `background_task` are required for multi-flight + backgrounding.

## Schema deltas

### `AgentRequest`

```graphql
type AgentRequest {
  # ...existing fields...
  caused_by_parent_request_id: String     # immediate parent in subagent tree
  caused_by_parent_tool_call_id: String   # the bridge edge on the parent
  subagent_depth: Int                     # 0 for top-level; child = parent + 1
}
```

`caused_by_trigger_kind` enum gains `"subagent"`.

### `AgentToolCall`

```graphql
type AgentToolCall {
  # ...existing B1 fields including lifecycle_state...
  request_id: String              # direct link (was implicit via session_id)
  await_mode: String              # "foreground" | "background"
  cancel_policy: String           # "cascade" | "detach"
  child_request_id: String        # non-empty iff this is a subagent invocation
}
```

### `ToolSelection`

```graphql
type ToolSelection {
  # ...existing fields (delegate_to, mcp_service_allowlist)...
  subagent_targets: [String]            # behavior_ids this agent may spawn
  subagent_tools: SubagentToolPolicy
}

type SubagentToolPolicy {
  spawn_enabled: Boolean
  steering_enabled: Boolean   # gates send_message_to_subagent + read_subagent_transcript
  background_enabled: Boolean # gates background mode + background_task / list_tasks
}
```

Defaults: all `false`. A behavior with no subagent config exposes no subagent tools — fully backwards-compatible.

### `TriggerSource` expansion: `SubagentSource`

Mirror of `ScheduleSource` / `EventSource` / `ManualSource` in the existing `TriggerEngine`. Listens for `AgentToolCall.create` events with `child_request_id` set, validates depth and behavior allowlist, dispatches a `FireIntent` that materializes the child `AgentRequest`.

Reusing `TriggerEngine`:
- Concurrency mode (`one_at_a_time` / `latest_only` / `parallel`) is configurable per parent behavior — fan-out limits live in trigger config.
- Existing T1–T4 trigger theorems apply: subagent fire is an enabled-gate fire, parented by the bridge edge, with full lineage.
- Apply-time validation extends to validate `behavior_id` is in the parent's `subagent_targets` allowlist.

### Identity

The child Request runs under the same `agent_did` as the parent in this spec. Cross-principal delegation lands with sourcenetwork/defra-agent#9 (AgentPrincipal/AgentBehavior split). Schema is forward-compatible: when #9 lands, `subagent_targets` becomes `(behavior_id, principal_did?)` pairs and `bridge_spawn` gains an identity-delegation precondition. Nothing in B1–B6 needs to change.

## File layout

```
crates/defra-agent/proofs/Proofs/
  Subagent.lean                   # 4-line re-export stub
  Subagent/
    State.lean                    # AwaitMode, CancelPolicy enums, BridgedState type
    Transition.lean               # BridgedState.Transition (6 constructors)
    Properties.lean               # B1–B6, INV-FG / INV-UNIQUE / INV-DEPTH / INV-LINK
    Executable.lean               # step refinement (for Rust conformance)
  ToolExecution/
    State.lean                    # AMEND: + awaitMode, cancelPolicy, childRequestId
    Transition.lean               # AMEND: + background, foreground, detach;
                                  #        complete gains bridge precondition disjunct
  Request/
    State.lean                    # AMEND: + subagentDepth, causedByParentRequestId,
                                  #        causedByParentToolCallId
  Composed.lean                   # AMEND: tools : List;
                                  #        no_blocking_foreground guard on advance / begin_inference
  Conformance/
    Contracts.lean                # AMEND: emit AwaitMode/CancelPolicy + bridge cases
```

## Conformance contract additions

Rust conformance tests in `tests/state_machine_conformance.rs` and `tests/lifecycle_regression.rs` consume Lean-emitted case lists.

| Generator | Cases | What Rust must check |
|---|---|---|
| `AwaitMode.all` exhaustiveness | 2 | `AgentToolCall.await_mode` round-trips. |
| `CancelPolicy.all` exhaustiveness | 2 | `AgentToolCall.cancel_policy` round-trips. |
| Bridge transition matrix | ~8 | Every spawn / mode-change / cascade / complete the runtime executes refines a Lean constructor. |
| B3 cascade trace witness | 1 | End-to-end: parent `.interrupted` ⇒ child reaches `.interrupted` within bounded steps. |
| B3' detach trace witness | 1 | End-to-end: detach-mode parent termination leaves child running. |
| B4 depth bound | 1 | Spawn at `subagent_depth = maxSubagentDepth` is rejected at runtime. |
| B5 link symmetry | per-row | Every `AgentToolCall.child_request_id` matches exactly one `AgentRequest.caused_by_parent_tool_call_id` and vice versa. |
| B6 foreground blocking | 1 | Parent's `progressSeq` and `messageSeq` do not advance while a foreground tool is live. |
| INV-UNIQUE | per-row | `AgentToolCall.tool_call_key` (the runtime callId) is unique within a request's tool list — runtime mints fresh callIds at spawn and never reuses them. |

These slot into the existing JSON-emitter pattern. Rust tests fail compilation if Lean adds a constructor that isn't covered.

## Migration

All new schema fields have benign defaults that exactly match today's behavior:

| Field | Default | Existing-row interpretation |
|---|---|---|
| `AgentToolCall.await_mode` | `"foreground"` | Today's tool calls are all synchronous foreground — no behavior change. |
| `AgentToolCall.cancel_policy` | `"cascade"` | C2 already cascades; default preserves existing behavior. |
| `AgentToolCall.child_request_id` | `null` | All existing tool calls are native, not subagent. |
| `AgentToolCall.request_id` | backfill from `(session_id, tool_call_id)` lookup | Already implicit via session; backfill is mechanical. |
| `AgentRequest.caused_by_parent_request_id` | `null` | Top-level requests have no parent. |
| `AgentRequest.caused_by_parent_tool_call_id` | `null` | Same. |
| `AgentRequest.subagent_depth` | `0` | Top-level. |

`SubagentSource` activates only when at least one behavior has `subagent_tools.spawn_enabled = true`. New tools never appear unless explicitly opted-in. No silent behavior change.

## References

- B1 ToolCall lifecycle spec: `defra-agent-issue-149-native-glob-deadline/docs/superpowers/specs/2026-05-08-toolcall-lifecycle-spec-design.md`
- Existing patterns: `Proofs/Request/{State,Transition,Properties,Executable}.lean`, `Proofs/InferenceCall/{State,Transition,Properties,Executable,SlotAccounting}.lean`, `Proofs/Composed.lean`, `Proofs/Triggers/`
- External systems studied:
  - OpenAI Codex: `codex-rs/tools/src/agent_tool.rs`, `codex-rs/rollout-trace/src/model/session.rs`, `codex-rs/protocol/src/protocol.rs`, `codex-rs/rollout-trace/src/reducer/tool/agents.rs`
  - Tintinweb pi-subagents: `src/types.ts`, `src/agent-manager.ts`
  - Nicobailon pi-subagents: `src/shared/types.ts`, `src/intercom/result-intercom.ts`
- Project conventions: `CLAUDE.md` ("The Lean proofs are the source of truth for all state machine behavior.")
