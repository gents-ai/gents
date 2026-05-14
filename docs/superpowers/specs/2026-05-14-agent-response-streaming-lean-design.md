# AgentResponse streaming → terminal lifecycle — Lean design

Date: 2026-05-14
Issue: #190 (Refs #183 parent tracker, #191 / #184 / #179 / #64 / #172 cross-links)
Status: design — pending implementation plan

## Why

Gap #2 from the 2026-05-13 formal coverage audit (`docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`, row line 39, "AgentResponse lifecycle (streaming → terminal)"). Server-side `AgentResponse` transitions `streaming → completed | error` are operational only today. S6 `persistence_before_completion` and L3 `recovery_convergence` imply terminal responses are committed, but the *streaming path* that produces those terminals carries no contract. The deadline audit's ⚠️ verdict on stream-liveness lives in the same gap.

Closing this gap pins the streamed-token persistence path under formal contract, gives #184 (compaction) the terminal observation it needs to anchor against, ties operator-visibility counters (#179, `streaming` state) to a typed vocabulary, and formalizes the #64 live-tail clear semantics that today are documented only in a comment in `streaming.rs:557`.

## What

A new Lean module `Proofs/StreamingResponse/` (own module, not an extension of `Proofs/Request/`) containing:

- A state machine with three statuses (`streaming | completed | error`), eight transitions covering the normal-path streaming lifecycle plus a recovery-path transition, and a composed bridge state that pairs response transitions with request-side terminal commits.
- Twelve theorems closing: terminal irreversibility, terminal-after-finalize, S6-bridge (response.completed ⇒ request.completed + persistence.committed), stream-liveness timeout (idle deadline exceeded ⇒ legal transition to error), #64 live-tail clear on normal-path finalize, recovery-path content-preservation asymmetry, completed-liveTail-not-canonical sentinel, idempotent finalize, begin-uniqueness per request_id, recovery sweep parity.
- Twelve conformance vectors emitted as `ResponseTransitionCase` rows, registered in the coverage ledger as a `consumerWithFollowUpCoverage` entry (Rust consumer wiring is a downstream task, not part of this PR).
- A small refactor of `Proofs/Recovery/Sweeps.lean` that replaces its local `ResponseRecoveryStatus` enum with an import from the new module, retiring a silent-duplication risk.

Zero `sorry`. No Rust production code; the existing `streaming.rs` implementation is out of scope. The brief's hard constraints are respected: `Proofs/Transcript/`, `Proofs/Properties/Safety.lean`, `Proofs/Properties/Liveness.lean`, and `Proofs/Request/*` are read-only.

## Where it lives

```
crates/defra-agent/proofs/Proofs/StreamingResponse/
  State.lean         -- Status, ErrorReason, LiveTail, ResponseContext, ResponseRequestBridge
  Transition.lean    -- the inductive Transition + BridgeTransition + Trace
  Properties.lean    -- the twelve theorems
  Executable.lean    -- ResponseTransitionCase rows
Proofs/StreamingResponse.lean    -- barrel
```

Touches outside the module:

- `Proofs.lean` — adds one line: `import Proofs.StreamingResponse`. Placed alphabetically between `Proofs.MCPHealth` and `Proofs.Subagent`. **For the #184 agent**: this is the single shared editing surface; the new import is at this canonical location, so #184's downstream import-line addition won't conflict.
- `Proofs/Recovery/Sweeps.lean` — replaces the local `ResponseRecoveryStatus` inductive (lines 73–98 of the current file) with an import of `StreamingResponse.Status`. The `ResponseRecoveryRow` wrapper stays (a thin `{status : Status}` record local to Sweeps), the three existing theorems (`responseRecovery_stale_positive`, `responseRecover_terminal`, `responseRecover_zero`) re-prove against the canonical type, and `responseRecoverySweep` re-points. Same goals, just unified vocabulary. Net diff: ~20 lines removed (the enum + its namespace + the local `HasTerminal` instance), ~3 lines added (the import + a thin re-export).

## State vocabulary (Proofs/StreamingResponse/State.lean)

```lean
namespace StreamingResponse

abbrev DocId := Nat
abbrev RequestId := Nat
abbrev Time := Nat

inductive Status where
  | streaming
  | completed
  | error
  deriving DecidableEq, Repr

namespace Status
  def toDefraDB : Status → String
    | .streaming => "streaming"
    | .completed => "complete"     -- matches Rust StreamStatus::Complete::as_str
    | .error => "error"

  def fromDefraDB? : String → Option Status
  instance : HasTerminal Status    -- terminal := completed ∨ error
end Status

inductive ErrorReason where
  | streamIdleTimeout
  | daemonRestartRecovery
  | inferenceFailed
  | finalizeRequestedError
  | interrupted
  deriving DecidableEq, Repr

inductive LiveTail where
  | empty
  | nonEmpty
  deriving DecidableEq, Repr

structure ResponseContext where
  docId                       : DocId
  requestId                   : RequestId
  status                      : Status
  liveTail                    : LiveTail
  tokenCount                  : Nat
  lastProgressAt              : Time
  streamIdleDeadline          : Time
  now                         : Time
  errorReason                 : Option ErrorReason
  materializedMessageSequence : Option Transcript.Sequence
  interruptedAt               : Option Time
  deriving DecidableEq, Repr

structure ResponseRequestBridge where
  response           : ResponseContext
  requestState       : RequestState        -- from Proofs.Request.State
  requestPersistence : PersistenceState    -- from Proofs.Persistence
  deriving DecidableEq

end StreamingResponse
```

Notes on shape:

- `LiveTail` is `empty | nonEmpty` rather than a string. The proofs in scope only need to discriminate the clear/not-clear cases; richer modeling would buy nothing and would couple the spec to encoding details.
- `tokenCount` is preserved through `resetTail` (matches `streaming.rs:182–189` comment: "token_count cumulative metering field").
- `streamIdleDeadline` is a parameter on the context — engine-supplied. Today the runtime value is 300_000 ms; the model proves the timeout transition is legal whenever `now > streamIdleDeadline`, leaving the runtime free to change the constant without invalidating the proof.
- `materializedMessageSequence` reuses `Transcript.Sequence` directly. This is the only vocabulary import from `Proofs/Transcript/` (and is read-only).

## Transitions (Proofs/StreamingResponse/Transition.lean)

```lean
inductive Transition : ResponseContext → ResponseContext → Prop where
  | begin (pre post : ResponseContext) :
      pre.status = .streaming → pre.liveTail = .empty →
      pre.tokenCount = 0 → pre.materializedMessageSequence = none →
      post = pre → Transition pre post

  | writeTokens (pre post : ResponseContext) (delta : Nat) :
      pre.status = .streaming → delta > 0 →
      post = { pre with
        liveTail := .nonEmpty
      , tokenCount := pre.tokenCount + delta
      , lastProgressAt := pre.now } → Transition pre post

  | writeReasoning (pre post : ResponseContext) :
      pre.status = .streaming →
      post = { pre with liveTail := .nonEmpty, lastProgressAt := pre.now } →
      Transition pre post

  | flushPending (pre post : ResponseContext) :
      pre.status = .streaming → post = pre → Transition pre post

  | resetTail (pre post : ResponseContext) :
      pre.status = .streaming →
      post = { pre with liveTail := .empty } → Transition pre post

  | setInterruptedAt (pre post : ResponseContext) (at : Time) :
      pre.interruptedAt = none →
      post = { pre with interruptedAt := some at } → Transition pre post

  | finalizeComplete (pre post : ResponseContext) (seq : Transcript.Sequence) :
      pre.status = .streaming →
      post = { pre with
        status := .completed
      , liveTail := .empty
      , materializedMessageSequence := some seq } → Transition pre post

  | finalizeError (pre post : ResponseContext) (reason : ErrorReason) :
      pre.status = .streaming →
      reason ∈ ({.inferenceFailed, .finalizeRequestedError,
                 .streamIdleTimeout, .interrupted} : List ErrorReason) →
      (reason = .streamIdleTimeout → pre.now > pre.streamIdleDeadline) →
      post = { pre with
        status := .error
      , liveTail := .empty
      , errorReason := some reason } → Transition pre post

  | recoverInterrupted (pre post : ResponseContext) :
      pre.status = .streaming →
      post = { pre with
        status := .error
      , -- liveTail UNCHANGED — recovery path stamps content (recovery.rs:142)
        errorReason := some .daemonRestartRecovery } → Transition pre post

  | observeIdempotentFinalize (pre post : ResponseContext) :
      pre.status = .completed ∨ pre.status = .error →
      post = pre → Transition pre post

inductive Trace : ResponseContext → ResponseContext → Prop where
  | refl {s} : Trace s s
  | step {s₁ s₂ s₃} : Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

inductive BridgeTransition : ResponseRequestBridge → ResponseRequestBridge → Prop where
  | finalizeComplete {pre post} :
      Transition pre.response post.response →
      post.response.status = .completed →
      pre.requestState = .processing →
      post.requestState = .completed →
      post.requestPersistence = .committed →
      BridgeTransition pre post

  | finalizeError {pre post} :
      Transition pre.response post.response →
      post.response.status = .error →
      post.response.errorReason ≠ some .daemonRestartRecovery →
      pre.requestState = .processing →
      post.requestState = .failed →
      post.requestPersistence = .committed →
      BridgeTransition pre post

  | recoverPaired {pre post} :
      Transition pre.response post.response →
      post.response.errorReason = some .daemonRestartRecovery →
      pre.requestState = .processing →
      post.requestState = .failed →
      post.requestPersistence = .committed →
      BridgeTransition pre post
```

Shape decisions:

- **`begin` is a seed-validity assertion, not a row-creation step.** The single-row state machine can't witness "no row exists"; instead, `begin` is the well-formedness predicate on a freshly-introduced response (status = streaming, liveTail = empty, tokenCount = 0, materializedMessageSequence = none), expressed as a `Transition pre pre` whose hypotheses are exactly the seed shape. The world-level uniqueness check (`load_response_state_by_key` in `streaming.rs:313`) is captured separately by `BeginUniquePerRequestId` on a `List ResponseContext`.
- **`flushPending` is an abstract no-op.** Carried as an explicit transition so conformance vectors can pin it; at the abstract level there's nothing to observe.
- **`recoverInterrupted` and `finalizeError(streamIdleTimeout)` are deliberately distinct transitions** with different live-tail semantics. This makes the asymmetry between `streaming.rs::finalize` (clears content) and `recovery.rs::recover_stuck_responses` (stamps "Response interrupted — daemon restarted") first-class. Any future Rust change that brings recovery into line with normal finalize will break the `recovery_path_preserves_liveTail` theorem and force re-modeling.
- **`setInterruptedAt` is status-independent.** Matches `streaming.rs::write_interrupted_at` which writes the timestamp without changing status.
- **`BridgeTransition.recoverPaired` is conceptually separate from `finalizeError`** because in the runtime, recovery-path request promotion happens in `recover_stuck_requests` (`recovery.rs:65`), not in the same atomic mutation as the response update. Modeling them as one bridge step would misrepresent the Rust contract; modeling as two steps with the same conclusion captures the truth.

## Properties (Proofs/StreamingResponse/Properties.lean)

### State-machine basics

```lean
theorem terminal_irreversibility
    {pre post : ResponseContext}
    (h_term : isTerminal pre.status) (h : Transition pre post) :
    isTerminal post.status

theorem identity_preserved
    {pre post : ResponseContext} (h : Transition pre post) :
    pre.docId = post.docId ∧ pre.requestId = post.requestId

theorem status_flow_bounded
    {pre post : ResponseContext} (h : Transition pre post) :
    (pre.status = .streaming → post.status = .streaming ∨ isTerminal post.status) ∧
    (isTerminal pre.status → post.status = pre.status)
```

### Terminal-after-finalize (composes with S6)

```lean
theorem completed_carries_materialized_handle
    {pre post : ResponseContext}
    (h : Transition pre post) (h_completed : post.status = .completed) :
    post.materializedMessageSequence.isSome

theorem response_completed_implies_request_committed
    {pre post : ResponseRequestBridge}
    (h : BridgeTransition pre post)
    (h_completed : post.response.status = .completed) :
    post.requestState = .completed ∧ post.requestPersistence = .committed
```

The second theorem is the response-level instance of `Proofs.Properties.Safety.persistence_before_completion` (Safety.lean:202). Its conclusion is exactly the request-side post-state that S6 requires. The composition is: `BridgeTransition` enforces both halves of the atomic Rust mutation in `streaming.rs::build_finalize_mutation` simultaneously.

### Stream-liveness (sibling of L3)

```lean
theorem streamIdle_eventually_terminal
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming)
    (h_expired : pre.now > pre.streamIdleDeadline) :
    ∃ post, Transition pre post ∧ post.status = .error ∧
            post.errorReason = some .streamIdleTimeout

theorem streaming_eventually_terminal
    (pre : ResponseContext) (h : pre.status = .streaming) :
    ∃ post, Transition pre post ∧ isTerminal post.status
```

The first proves the deadline-bounded liveness obligation (closing the deadline-audit ⚠️ stream-liveness verdict). The second is the unconditional sibling of L3 — from any streaming state, *some* terminal-reaching transition exists (witnessed by `recoverInterrupted`).

### #64 live-tail clear + recovery asymmetry

```lean
theorem normal_finalize_clears_liveTail
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_finalize : post.status = .completed ∨
                  (post.status = .error ∧
                   post.errorReason ≠ some .daemonRestartRecovery)) :
    post.liveTail = .empty

theorem recovery_path_preserves_liveTail
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_reason : post.errorReason = some .daemonRestartRecovery) :
    post.liveTail = pre.liveTail

theorem completed_liveTail_is_not_canonical
    {s : ResponseContext}
    (h_completed : s.status = .completed)
    (h_reachable : ∃ s₀, Trace s₀ s) :
    s.liveTail = .empty ∧ s.materializedMessageSequence.isSome
```

The third theorem is the **#64 sentinel**: a `.completed` response's `.liveTail` is empty (not canonical text), and the canonical handle is `materializedMessageSequence`. Documents in Lean what `streaming.rs:557` documents in a comment.

### Uniqueness + idempotence

```lean
def BeginUniquePerRequestId (rows : List ResponseContext) : Prop :=
  ∀ r₁ r₂, r₁ ∈ rows → r₂ ∈ rows →
    r₁.requestId = r₂.requestId → r₁.docId = r₂.docId

theorem begin_preserves_unique_per_request_id
    (rows : List ResponseContext) (new : ResponseContext)
    (h_unique : BeginUniquePerRequestId rows)
    (h_no_existing : ∀ r ∈ rows, r.requestId ≠ new.requestId) :
    BeginUniquePerRequestId (new :: rows)

theorem idempotent_finalize_is_noop
    {pre post : ResponseContext} (h : Transition pre post)
    (h_pre_term : isTerminal pre.status) :
    post = pre
```

`BeginUniquePerRequestId` is lifted to the world layer (a `List ResponseContext`) rather than baked into `ResponseContext` itself, matching `streaming.rs::begin`'s `load_response_state_by_key` check.

### Recovery sweep parity

```lean
theorem recoverySweep_implements_recoverInterrupted
    (row : ResponseRecoveryRow)
    (h_stale : responseRecoveryStale row) :
    ∃ pre post,
      Transition pre post ∧
      pre.status = .streaming ∧
      post.status = .error ∧
      post.errorReason = some .daemonRestartRecovery
```

After the Sweeps.lean refactor, this proves the registered `responseRecoverySweep` is a degenerate instance of `Transition.recoverInterrupted`. Retires the silent-duplication risk between `ResponseRecoveryStatus` and `StreamingResponse.Status`.

## Conformance vectors (Proofs/StreamingResponse/Executable.lean)

```lean
structure ResponseTransitionCase where
  name                       : String
  group                      : String
  action                     : String
  legal                      : Bool
  preStatus                  : String
  postStatus                 : String
  preLiveTail                : String
  postLiveTail               : String
  preTokenCount              : Nat
  postTokenCount             : Nat
  errorReason                : Option String
  preMaterializedSeq         : Option Transcript.Sequence
  postMaterializedSeq        : Option Transcript.Sequence
  expectedRequestState       : Option String
  expectedRequestPersistence : Option String
  deriving Repr

def responseTransitionCases : List ResponseTransitionCase := [
  /-  1 -/ begin_emits_streaming_empty
  /-  2 -/ , write_tokens_advances_progress
  /-  3 -/ , write_reasoning_no_token_bump
  /-  4 -/ , flush_pending_is_abstract_noop
  /-  5 -/ , reset_tail_clears_but_preserves_tokens
  /-  6 -/ , finalize_complete_clears_and_materializes
  /-  7 -/ , finalize_error_inference_failed_clears
  /-  8 -/ , finalize_error_idle_timeout_requires_deadline
  /-  9 -/ , recover_interrupted_keeps_content
  /- 10 -/ , observe_idempotent_finalize_is_noop
  /- 11 -/ , set_interrupted_at_does_not_change_status
  /- 12 -/ , bridge_completed_pairs_request_committed
]
```

| # | Name | Group | Action | Headline assertion |
|---|---|---|---|---|
| 1 | `begin_emits_streaming_empty` | normal | begin | seed shape is legal |
| 2 | `write_tokens_advances_progress` | normal | write_tokens | liveTail → nonEmpty, tokenCount bumps, lastProgressAt = now |
| 3 | `write_reasoning_no_token_bump` | normal | write_reasoning | liveTail → nonEmpty, tokenCount unchanged |
| 4 | `flush_pending_is_abstract_noop` | normal | flush | post = pre |
| 5 | `reset_tail_clears_but_preserves_tokens` | normal | reset_tail | liveTail → empty, tokenCount preserved |
| 6 | `finalize_complete_clears_and_materializes` | normal | finalize_complete | status → completed, liveTail → empty, materializedSeq = some |
| 7 | `finalize_error_inference_failed_clears` | normal | finalize_error | status → error, liveTail → empty, reason = inferenceFailed |
| 8 | `finalize_error_idle_timeout_requires_deadline` | normal | finalize_error | reason = streamIdleTimeout legal only when now > deadline |
| 9 | `recover_interrupted_keeps_content` | recovery | recover_interrupted | status → error, liveTail unchanged, reason = daemonRestartRecovery |
| 10 | `observe_idempotent_finalize_is_noop` | idempotent | observe_idempotent_finalize | pre terminal, post = pre |
| 11 | `set_interrupted_at_does_not_change_status` | boundary | set_interrupted_at | interruptedAt → some, status unchanged |
| 12 | `bridge_completed_pairs_request_committed` | bridge | finalize_complete | response.completed ⇒ request.completed + persistence.committed |

Coverage ledger entry (in `Proofs/Conformance/CoverageLedger.lean`):

```lean
, consumerWithFollowUpCoverage
    "streaming_response_cases"
    "ResponseTransitionCases"
    "state_machine_conformance::generated_streaming_response_cases_pin_lifecycle_contract"
    "Rust consumer wires up in a follow-up; vectors are stable and ready."
```

Modeled after the existing `queue_deadline_cases` and `recovery_sweep_cases` entries (lines 277–286 of CoverageLedger.lean).

## Cross-module wiring summary

| File | Change | Net |
|---|---|---|
| `Proofs.lean` | + `import Proofs.StreamingResponse` (alphabetic placement between `MCPHealth` and `Subagent`) | +1 line |
| `Proofs/Recovery/Sweeps.lean` | replace local `ResponseRecoveryStatus` + `ResponseRecoveryRow` (lines 73–146) with imports from `Proofs.StreamingResponse.State`; re-point three existing theorems | ~25 lines removed, ~5 added |
| `Proofs/Conformance/CoverageLedger.lean` | + one `consumerWithFollowUpCoverage` entry | +5 lines |
| New: `Proofs/StreamingResponse/State.lean` | full | ~120 lines |
| New: `Proofs/StreamingResponse/Transition.lean` | full | ~80 lines |
| New: `Proofs/StreamingResponse/Properties.lean` | full | ~180 lines (12 theorems) |
| New: `Proofs/StreamingResponse/Executable.lean` | full | ~140 lines (12 vectors) |
| New: `Proofs/StreamingResponse.lean` | barrel | ~10 lines |

Read-only (do not modify):
- `Proofs/Transcript/*` — vocabulary import (`Sequence`) only.
- `Proofs/Properties/Safety.lean` — S6 cited as the parent theorem of `response_completed_implies_request_committed`.
- `Proofs/Properties/Liveness.lean` — L3 cited as the parent theorem of `streaming_eventually_terminal`.
- `Proofs/Request/*` — `RequestState` and `PersistenceState` imported by `ResponseRequestBridge`.

## Verdicts moved by this PR

| Audit row | Before | After |
|---|---|---|
| AgentResponse lifecycle (streaming → terminal), line 39 | ❌ | ✓ Modeled |
| Stream liveness / finalize / live-tail (#64), line 32 | ❌ | ✓ Modeled |
| Deadline audit ⚠️ stream-liveness verdict (#172) | ⚠️ | ✓ closed via `streamIdle_eventually_terminal` |

## Out of scope

- Rust production code (the `streaming.rs` implementation stays as-is; this PR is Lean + ledger only).
- Rust consumer test in `tests/state_machine_conformance.rs` (registered as `consumerWithFollowUpCoverage`).
- Modeling `progress_seq` as a per-response counter — it's owned by `RequestLifecycle::advance` and lives in the Request model; this spec uses it only at the bridge layer.
- Modeling `materializedMessageSequence` write semantics in detail; we treat it as an `Option Sequence` field that becomes `some` on `finalizeComplete`. The actual materialization runs out-of-band in the runtime; #191 owns that vocabulary.

## Risks and notes

- **Type-class plumbing for `HasTerminal Status`**: the `Recovery/Sweeps.lean` refactor depends on `HasTerminal` working transparently after the move. Verify with `lake build` after the State.lean lift.
- **`Transcript.Sequence` import direction**: the new module imports `Proofs.Transcript.State`. Check for circular-import risk; `Transcript` does not import `StreamingResponse`, so we're acyclic.
- **`Recovery/Sweeps.lean` downstream consumers**: `Proofs/Recovery/ContractCases.lean` may reference `ResponseRecoveryStatus` constructors directly. If so, update those references in the impl phase. Net effect: cosmetic name change only.
- **`finalizeError(streamIdleTimeout)` hypothesis discharge**: the runtime today fires the timeout via tracing only, not a typed signal; the model proves the transition is *legal* when `now > deadline`, not that the runtime *takes* it. This is a runtime obligation marker (the existing audit pattern), not a runtime guarantee — flagged in the PR body.

## File-by-file build order (for the implementation plan)

1. `Proofs/StreamingResponse/State.lean` — vocabulary first; `lake build` must pass.
2. `Proofs/StreamingResponse/Transition.lean` — depends on State.
3. `Proofs/StreamingResponse/Properties.lean` — depends on Transition. Build incrementally, one theorem at a time, to localize tactic-failure regressions.
4. `Proofs/StreamingResponse/Executable.lean` — depends on Transition.
5. `Proofs/StreamingResponse.lean` — barrel.
6. `Proofs.lean` — add the import; `lake build` must still pass.
7. `Proofs/Recovery/Sweeps.lean` — point at canonical types; re-prove the three sweep theorems.
8. `Proofs/Conformance/CoverageLedger.lean` — register the ledger entry.

Final `lake build` must complete without `sorry` and without errors.
