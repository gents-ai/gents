import Proofs.Basic

/-! # CompletionRetry: per-completion retry inside the owned loop (#631)

One request runs one owned loop; each turn issues completions. This model
governs how a single request's completion failures are retried. It is the
formal fence for: no tool re-execution on retry, retraction only before
effects, bounded budgets with at-most-once repair, backoff that fits the
claimed deadline, and at most one retained rendered turn per turn index. -/

namespace CompletionRetry

/-- Retry-relevant failure classification. Mirrors the Rust
`InferenceError` retry classes produced by `classify_completion_error`:
`transport` covers transient/timeout/rate-limit/5xx; `parseBadRequest` is
the vLLM tool-call json-parse 400 signature; `permanent` is everything
else. -/
inductive FailureClass
  | transport
  | parseBadRequest
  | permanent
  deriving DecidableEq, Repr

/-- Per-request retry budget, resolved from InferenceProfile + execution
origin before the loop starts. `transportRetries` is the ladder length. -/
structure Budget where
  transportRetries : Nat
  resampleRetries : Nat
  allowRepair : Bool
  deriving DecidableEq, Repr

/-- Where the current completion attempt stands. `backingOff until` holds
the wake time so deadline-fit is checkable at transition time. -/
inductive Phase
  | issuing
  | streaming
  | backingOff (wake : Time)
  | repairing
  | turnClosed      -- mid-stream failure after effects: partial turn + results durably threaded
  | turnDone        -- completion consumed to its end
  | exhausted       -- retry budget or deadline exhausted → terminal failed
  | failedPermanent -- non-retryable classification → terminal failed
  deriving DecidableEq, Repr

/-- Per-turn effect/render tracking. `turnIndex` identifies the current
turn; `effects` counts tool executions in the CURRENT turn; `rendered`
counts retained rendered instances of the current turn index in the
materialized response. Close-and-continue increments `turnIndex` (a new
turn with fresh counters); retraction keeps it (same turn, re-sampled). -/
structure TurnCtx where
  turnIndex : Nat
  effects : Nat
  rendered : Nat
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  budget : Budget
  transportUsed : Nat
  resampleUsed : Nat
  repairUsed : Bool
  /-- Last parse-400 error text, for deterministic-400 detection. Opaque. -/
  lastParseError : Option String
  now : Time
  deadline : Option Time
  turn : TurnCtx
  deriving DecidableEq, Repr

/-- A wake time fits the deadline iff it does not pass it. No deadline
means the ladder itself is the ceiling. -/
def fitsDeadline (wake : Time) (deadline : Option Time) : Prop :=
  match deadline with
  | none => True
  | some d => wake ≤ d

def State.terminal (s : State) : Prop :=
  s.phase = Phase.turnDone ∨ s.phase = Phase.exhausted ∨ s.phase = Phase.failedPermanent

end CompletionRetry
