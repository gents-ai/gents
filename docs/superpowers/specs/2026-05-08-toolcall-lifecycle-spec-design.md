# ToolCall Lifecycle — Lean Spec Design

**Status:** Design (B1 of issue 149 follow-up program)
**Date:** 2026-05-08
**Tracks issue:** [sourcenetwork/defra-agent#149](https://github.com/sourcenetwork/defra-agent/issues/149) — *Native glob tool can hang request past deadline and require process restart*
**Scope:** Lean spec only. Implementation tracked in separate specs (B2–B6).

## Background

Issue 149 reports a request-liveness failure: native filesystem `glob` tool calls can remain persisted as `AgentToolCall.status=called` indefinitely. While this happens, the request stays `processing/processing` past its recorded deadline, the single-worker queue stops advancing, and `/healthz` reports OK. Recovery only happens after a process restart and startup-recovery sweep.

A code audit confirms the gap is in the spec model, not just the runtime:

- `Proofs/Request/State.lean:154-158` defines a `deadlineExceeded` predicate, but no transition in `Proofs/Request/Transition.lean` consumes it. Property S4 (deadline bounding) names the concept without operationalizing it.
- `Proofs/ToolExecution.lean` is purely a *policy* model (`Health × SchemaStatus → PreflightDecision`, plus retry disposition). It has no lifecycle states.
- The runtime enforces deadline only at the inference-daemon loop entry and around retry backoff (`crates/defra-agent/src/agent/daemon/inference.rs:56-72`). Tool futures are not wrapped, and `glob` is a synchronous recursive `std::fs::read_dir` walk (`crates/defra-agent/src/toolset/shared/filesystem.rs:226-281`) with no cancellation point.

Per `CLAUDE.md`'s "Lean proofs are the source of truth for all state machine behavior" rule, the fix begins in the spec. This document is that spec.

The design draws on a study of OpenAI Codex's `unified_exec` process supervisor and `tools/orchestrator` for execution-model patterns to adopt (typed payload dispatch, two-layer cancellation, partial-output preservation, process-group kill) and patterns to deliberately reject (implicit unnamed lifecycle, no formal state machine for tool execution).

## Goals

- Add a daemon-visible lifecycle state machine for an individual tool dispatch.
- Compose it with the existing `RequestContext` lifecycle so a request whose deadline expires drives every live linked tool call to a terminal state.
- Prove the property whose absence caused 149: an in-flight tool call cannot hold a request past its deadline.
- Keep the existing `ToolExecution` policy model (preflight + retry disposition) intact and consistent with the new lifecycle.

## Non-goals (out of scope for this spec)

1. Subprocess kill mechanics — process-group kill, SIGTERM/SIGKILL escalation, output-drain timeouts. Daemon-visible only; OS behavior is an external assumption.
2. Output streaming — no `OutputDelta` / chunk-replay model. Partial output is metadata on the terminal row, not a state.
3. Persistent processes spanning turns or requests (codex `unified_exec` analog). See "Future work" below.
4. Sandbox/permission tier model. `CommandPolicy.lean` already covers argv/network/sandbox policy; composition with the new lifecycle is separate.
5. Concurrency between tool calls within a single request. Single-flight `Option` model.
6. DefraDB schema rename. Spec uses canonical names (`pending`, `running`, `timedOut`, `cancelled`); persisted vocabulary aliasing handled at runtime/migration time.

## File layout

The existing `Proofs/ToolExecution.lean` becomes a folder, mirroring `Proofs/Request/` and `Proofs/InferenceCall/`:

```
crates/defra-agent/proofs/Proofs/
  ToolExecution.lean              -- 4-line re-export stub
  ToolExecution/
    Policy.lean                   -- existing contents, content unchanged
    State.lean                    -- new: ToolCallState, ToolCallContext, predicates
    Transition.lean               -- new: relational transitions
    Properties.lean               -- new: T1..T5
    Executable.lean               -- new: step function for conformance traces
  Composed.lean                   -- amended: tool_step variant + C1, C1', C2, C3
  Conformance/
    Contracts.lean                -- amended: emit ToolCall vocabulary and witness rows
```

The top-level `Proofs/ToolExecution.lean` re-exports `ToolExecution.Policy` and `ToolExecution.State` so existing import paths in Rust conformance tests stay stable.

## State vocabulary

```lean
inductive ToolCallState where
  | pending       -- row created, dispatch decided, work not yet started
  | running       -- spawned/awaiting; the in-flight state where 149's bug lives
  | completed     -- success terminal
  | failed        -- non-deadline non-interrupt error terminal
  | timedOut      -- request deadline exceeded → tool killed terminal (closes 149)
  | cancelled     -- request was interrupted → tool killed terminal
  deriving DecidableEq, Repr
```

Persisted vocabulary via `toDefraDB`: `"pending" | "running" | "completed" | "failed" | "timedOut" | "cancelled"`.

`HasTerminal` instance: `completed | failed | timedOut | cancelled` are terminal.

The current schema's `status="called"` (in-flight) and `status="completed" + tool_failure_class` (failed) are legacy persistence shapes. Schema migration is a runtime concern (B6), not a spec concern.

## Context

```lean
structure ToolCallContext where
  callId       : ToolCallId
  requestId    : RequestId
  state        : ToolCallState
  operation    : ToolOperation        -- existing enum from Policy.lean
  deadline     : Time                 -- inherited from parent RequestContext.deadline
  startedAt    : Option Time          -- set on pending → running
  currentTime  : Time
  failureClass : Option FailureClass  -- existing enum from Policy.lean
  persistence  : PersistenceState     -- mirrors RequestContext.persistence
  deriving Repr

def deadlineExceeded (c : ToolCallContext) : Prop := c.currentTime > c.deadline
def cancellable (c : ToolCallContext) : Prop := c.state = .pending ∨ c.state = .running
def linkedTo (c : ToolCallContext) (rid : RequestId) : Prop := c.requestId = rid
```

The composition guard in `Composed.lean` requires `pre.tool.requestId = pre.requestId` and `pre.tool.deadline = pre.request.deadline` and `pre.tool.currentTime = pre.request.currentTime`. These structural invariants make "tool deadline equals request deadline" hold by construction; we do not prove it as a separate theorem.

## Transitions

Modeled the same way as `Proofs/Request/Transition.lean` and `Proofs/InferenceCall/Transition.lean`: a relational `inductive Transition : ToolCallContext → ToolCallContext → Prop`, plus an `Executable.lean` step function proven to refine the relation.

Seven state-changing transitions, two non-state transitions:

```lean
inductive Transition : ToolCallContext → ToolCallContext → Prop where

  | dispatch {pre post : ToolCallContext}                        -- Pending → Running
      (h_state    : pre.state = .pending)
      (h_post     : post = { pre with state := .running
                                    , startedAt := some pre.currentTime })
      : Transition pre post

  | spawnFailed {pre post : ToolCallContext} (failure : FailureClass)   -- Pending → Failed
      (h_state    : pre.state = .pending)
      (h_post     : post = { pre with state := .failed
                                    , failureClass := some failure })
      : Transition pre post

  | complete {pre post : ToolCallContext}                        -- Running → Completed
      (h_state    : pre.state = .running)
      (h_persist  : pre.persistence = .committed)
      (h_post     : post = { pre with state := .completed })
      : Transition pre post

  | fail {pre post : ToolCallContext} (failure : FailureClass)   -- Running → Failed
      (h_state    : pre.state = .running)
      (h_post     : post = { pre with state := .failed
                                    , failureClass := some failure })
      : Transition pre post

  | timeout {pre post : ToolCallContext}                         -- Running → TimedOut
      (h_state    : pre.state = .running)
      (h_deadline : pre.deadlineExceeded)
      (h_post     : post = { pre with state := .timedOut })
      : Transition pre post

  | cancelBeforeDispatch {pre post : ToolCallContext}            -- Pending → Cancelled
      (h_state    : pre.state = .pending)
      (h_post     : post = { pre with state := .cancelled })
      : Transition pre post

  | cancelDuringRun {pre post : ToolCallContext}                 -- Running → Cancelled
      (h_state    : pre.state = .running)
      (h_post     : post = { pre with state := .cancelled })
      : Transition pre post

  | timeAdvance {pre post : ToolCallContext} (t : Time)
      (h_le   : pre.currentTime ≤ t)
      (h_post : post = { pre with currentTime := t })
      : Transition pre post

  | persistenceStep {pre post : ToolCallContext}
      (policy : PersistenceState.FailurePolicy)
      (next : PersistenceState)
      (h_p_step : PersistenceState.Transition policy pre.persistence next)
      (h_post   : post = { pre with persistence := next })
      : Transition pre post
```

### Shape choices

- **Spawn failure is its own transition.** Codex models a synchronous spawn error as `ExecCommandEnd { exit_code: -1 }`. Modeling it as `Pending → Failed` keeps "Running" honest: a call is `Running` only if it actually got a chance to execute.
- **`failureClass` is set only on the `.failed` terminal.** `TimedOut` and `Cancelled` are their own terminal states — the *state* is the cause; they do not carry a `failureClass`. T2 (deadline ⇒ TimedOut) stays provable because the `timeout` constructor is the only transition that produces `.timedOut`.
- **`complete` requires `persistence = .committed`** (mirror of Request S6). The `AgentToolCall` row's `completed_at`, `latency_ms`, and `tool_failure_class` are visible at the moment we declare success.
- **`timeout` requires `deadlineExceeded`** as a precondition. The daemon cannot fabricate a timeout. This is the property issue 149 reports as missing in practice.
- **No `Cancelling` intermediate state.** Codex doesn't model one. From the daemon's view, kill is atomic — the next observable state after issuing kill is the terminal one.
- **Two cancel constructors** (`cancelBeforeDispatch` for Pending, `cancelDuringRun` for Running) so the composition theorem C2 case-splits cleanly. Mirrors `InferenceCall.cancel_before_stream_transition` / `cancel_during_stream_transition`.

### Trace

`ToolExecution/Transition.lean` defines `Trace : ToolCallContext → ToolCallContext → Prop` as the reflexive-transitive closure of `Transition`, mirroring the existing pattern in `Composed.lean` and `InferenceCall/Transition.lean`. The composed-layer trace (used by C1, C1', C2, C3) is the existing `ComposedState.Trace` already defined in `Composed.lean`.

### Executable refinement

`ToolExecution/Executable.lean` defines a `step : ToolCallContext → ToolCallEvent → Option ToolCallContext` function (with `ToolCallEvent` an enum mirroring the state-changing constructors) and proves `step_refines_transition`. Same pattern as `Request/Executable.lean`. Required for Rust conformance trace generation.

## Single-machine properties (T1..T5)

Live in `ToolExecution/Properties.lean`.

```lean
/-- T1: Terminal irreversibility. Mirror of Request S1. -/
theorem terminal_irreversible
    {pre post : ToolCallContext}
    (h_terminal : isTerminal pre.state)
    (h_step     : Transition pre post) :
    pre.state = post.state ∧ pre.failureClass = post.failureClass

/-- T2: TimedOut is reachable only when deadline is exceeded.
    The property whose absence caused issue 149. -/
theorem timedOut_requires_deadline_exceeded
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_post : post.state = .timedOut) :
    pre.deadlineExceeded

/-- T3: Persistence before completion. Mirror of Request S6. -/
theorem completed_implies_committed
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_post : post.state = .completed) :
    post.persistence = .committed

/-- T4: Cancellable iff non-terminal. -/
theorem cancellable_iff_non_terminal (c : ToolCallContext) :
    c.cancellable ↔ ¬ isTerminal c.state

/-- T5: Bounded reachability to terminal (liveness).
    Any non-terminal call has a trace to a terminal state, given sufficient
    time advance. Quantifies over a witness time `∃ t > deadline`.
    Daemon-side liveness underlying 149's fix. -/
theorem live_call_reaches_terminal
    (c : ToolCallContext)
    (h_live : ¬ isTerminal c.state) :
    ∃ post, Trace c post ∧ isTerminal post.state
```

### Why this set, not more

- No S3 (monotonic progress). Tool calls don't carry a `progressSeq`. Skipped.
- No S5 (recovery exclusivity). Tool-call recovery is a request-level concern handled transitively via C1 (a request whose deadline expires drives its tool to terminal).
- `startedAt` monotonicity is structurally true (only `dispatch` writes it, and only on a Pending pre-state); stated as a corollary in the file header rather than a theorem.

### Boundary noted in the file header

The lifecycle picks up after `Policy.preflight = .dispatch`. A `.block` decision skips the lifecycle entirely and persists `failed` directly via the existing `tool_failure_class` field at the request level. That gating is enforced in Rust at the dispatch site; modeling it in Lean would require carrying `Health × SchemaStatus` in `ToolCallContext`, which doesn't earn its keep.

## Composition with `RequestContext`

Lives in `Composed.lean`, alongside `interrupted_request_cancels_live_linked_call`.

### Step 1: Extend `ComposedState`

```lean
structure ComposedState where
  requestId : RequestId
  process   : ProcessState
  request   : RequestContext
  call      : InferenceCall
  tool      : Option ToolCallContext   -- new
  deriving Repr
```

`Option` because not every request has a tool call in flight at every moment. Single-flight matches `max_concurrent=1` and the daemon's serial single-active-tool model. Multi-flight is a future extension (see Future work).

### Step 2: New `tool_step` variant

```lean
inductive Transition : ComposedState → ComposedState → Prop where
  -- existing process_step, request_step, persistence_step, call_step
  | tool_step {pre post : ComposedState} {toolPre toolPost : ToolCallContext} :
      pre.tool = some toolPre →
      ToolCallContext.Transition toolPre toolPost →
      post.tool = some toolPost →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      -- structural composition guards:
      toolPre.requestId = pre.requestId →
      toolPre.deadline = pre.request.deadline →
      toolPre.currentTime = pre.request.currentTime →
      Transition pre post
```

The last three guards encode the structural invariants from above; they hold by construction in any valid composed trace.

### Step 3: 149-closing theorems (C1 split)

C1 splits into two theorems by pre-state, since a `Pending`-at-deadline tool routes to `.cancelled` (it never ran), while a `Running`-at-deadline tool routes to `.timedOut` (it was killed). This mirrors how the dispatcher actually behaves.

```lean
/-- C1: A request whose deadline is exceeded times out a Running linked tool. -/
theorem deadline_exceeded_request_timesOut_running_tool
    {pre : ComposedState} {toolPre : ToolCallContext}
    (h_tool       : pre.tool = some toolPre)
    (h_running    : toolPre.state = .running)
    (h_linked     : toolPre.linkedTo pre.requestId)
    (h_deadline   : pre.request.deadlineExceeded)
    (h_synced     : toolPre.deadline = pre.request.deadline ∧
                    toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.linkedTo pre.requestId

/-- C1': A request whose deadline is exceeded cancels a Pending linked tool. -/
theorem deadline_exceeded_request_cancels_pending_tool
    {pre : ComposedState} {toolPre : ToolCallContext}
    (h_tool       : pre.tool = some toolPre)
    (h_pending    : toolPre.state = .pending)
    (h_linked     : toolPre.linkedTo pre.requestId)
    (h_deadline   : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId
```

### Step 4: Interrupt theorem (C2)

Direct mirror of the existing `interrupted_request_cancels_live_linked_call`.

```lean
/-- C2: An interrupted request cancels every live linked tool call. -/
theorem interrupted_request_cancels_live_linked_tool
    {pre : ComposedState} {toolPre : ToolCallContext}
    (h_tool        : pre.tool = some toolPre)
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked      : toolPre.linkedTo pre.requestId)
    (h_live        : toolPre.cancellable) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId
```

### Step 5: Liveness theorem (C3)

```lean
/-- C3: A request whose linked tool is terminal can resume making progress.
    The semantic complement of 149: terminal tool ⇒ no daemon-side blockage. -/
theorem terminal_tool_unblocks_request_progress
    {pre : ComposedState} {toolPre : ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_terminal : isTerminal toolPre.state)
    (h_proc     : pre.request.state = .processing) :
    ∃ post,
      Transition pre post ∧
      RequestContext.Transition pre.request post.request
```

## Conformance contract additions

Rust conformance tests in `tests/state_machine_conformance.rs` and `tests/lifecycle_regression.rs` consume Lean-emitted case lists via `Proofs/Conformance/Contracts.lean`.

| Generator | Cases | What Rust must check |
|---|---|---|
| `ToolCallState.all` exhaustiveness | 6 | Every persisted `AgentToolCall.lifecycle_state` value has a Lean constructor (and vice versa). |
| Transition matrix (state × event → next-state) | ~14 | Every transition the runtime executes refines a Lean constructor. |
| `terminal_irreversible` (T1) | 4 | Runtime never moves a terminal tool call. |
| `timedOut_requires_deadline_exceeded` (T2) | 1 witness | Any persisted `lifecycle_state="timedOut"` row has `currentTime > deadline` at the witness moment. |
| `completed_implies_committed` (T3) | 1 | Runtime never persists `completed` without persistence-committed flush. |
| `live_call_reaches_terminal` (T5) | 2 | Conformance trace executor terminates within bounded steps. |
| Composed C1, C1', C2, C3 | 4 traces | End-to-end runtime traces match the Composed.lean theorem statements. |

These slot into the existing JSON-emitter pattern. Rust tests fail compilation if Lean adds a constructor that isn't covered.

## Policy ↔ Lifecycle boundary

`ToolExecution/Policy.lean` (existing, content unchanged) returns a `PreflightDecision`. The lifecycle in `State.lean` picks up after that decision:

- `PreflightDecision.dispatch` → caller creates a `ToolCallContext` with `state = .pending` and the lifecycle proceeds.
- `PreflightDecision.block failure` → no lifecycle row created; the failure persists at the request level via the existing `tool_failure_class` field. From the `ComposedState` view, `pre.tool = none` for that call.

This gating stays in Rust at the dispatch site (no Lean theorem). The Properties.lean header documents it as a structural assumption, consistent with how `PersistenceState` preconditions are stated at the boundary rather than forced into the state space.

## Future work

These extensions are deliberately deferred to keep this spec focused.

- **Codebase-wide deadline audit.** Once this lifecycle pattern lands, audit every deadline-bearing entity (Request, ToolCall, InferenceCall, MCP transport, ScheduleSource fire windows, etc.) and confirm every nested deadline ≤ its parent's deadline, either as a structural invariant or as a composition theorem. Each gap closes either by adding a Lean theorem or by deleting unused/dead deadline configuration.
- **B4 — persistent processes spanning turns/requests** (codex `unified_exec` analog). Will require:
  - `ComposedState.tool : Option → tools : Array`.
  - Weakening the `tool_step` guards `toolPre.deadline = pre.request.deadline` and `toolPre.requestId = pre.requestId`, since a persistent process is not bound to any single request's deadline.
  - Restating `RequestContext` properties that depend on linked-call termination. Specifically: monotonic-progress and bounded-termination liveness statements may need to be conditioned on "in-scope tool calls are terminal" rather than "all linked tool calls are terminal."
- **B5 — sandbox/permission tier model.** `CommandPolicy.lean` already covers argv/network/sandbox policy; composition with the new lifecycle is straightforward — likely a guard on the `dispatch` transition that reads the policy. Deferred to its own spec.
- **B6 — schema migration.** `AgentToolCall.status` vocabulary expansion; data migration plan for existing `called` and `completed+failure_class` rows.
- **B2 + B3 — the actual runtime work** (subprocess supervisor, native-tool migration). Tracked as separate specs that consume this one.

## References

- Issue: [sourcenetwork/defra-agent#149](https://github.com/sourcenetwork/defra-agent/issues/149)
- Existing patterns: `Proofs/Request/{State,Transition,Properties,Executable}.lean`, `Proofs/InferenceCall/{State,Transition,Properties,Executable,SlotAccounting}.lean`, `Proofs/Composed.lean`
- Existing policy model: `Proofs/ToolExecution.lean`
- Codex architecture studied: `codex-rs/core/src/unified_exec/`, `codex-rs/core/src/exec.rs`, `codex-rs/core/src/tools/orchestrator.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/protocol/src/protocol.rs`
- Project conventions: `CLAUDE.md` ("The Lean proofs are the source of truth for all state machine behavior.")
