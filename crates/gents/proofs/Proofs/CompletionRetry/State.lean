import Proofs.CanonicalOutput.State

namespace CompletionRetry

inductive FailureClass
  | transport
  | parseBadRequest
  | permanent
  deriving DecidableEq, Repr

/-- Typed origin at the provider-input boundary. Local request construction is
deterministic; a retry cannot change the same malformed native body. This does
not classify arbitrary provider-reported failures, whose status/payload need
their existing separate interpretation. -/
inductive FailureOrigin
  | localRequestBuild
  | retryableTransport
  /-- A malformed or truncated provider-delivered stream is not a malformed
  local request. This origin applies whether or not a preview item arrived. -/
  | providerStreamMalformed
  deriving DecidableEq, Repr

def FailureOrigin.class : FailureOrigin → FailureClass
  | .localRequestBuild => .permanent
  | .retryableTransport => .transport
  | .providerStreamMalformed => .transport

structure Budget where
  transportRetries : Nat
  resampleRetries : Nat
  allowRepair : Bool
  deriving DecidableEq, Repr

/-- Retry policy begins before publication. Accepted phases are absorbing
publication-side observations, not provider-attempt phases. -/
inductive Phase
  | issuing
  /-- A provider attempt is in flight; this does not assert that the first
  streamed item has arrived. Native pre-first and mid-stream routes both
  refine this phase, with retraction after a failed attempt. -/
  | streaming
  | retractRequired (failure : FailureClass) (error : String) (wake : Time)
  | retracted (failure : FailureClass) (error : String) (wake : Time)
  | backingOff (wake : Time)
  | repairing
  | accepted (header : Nat)
  | acceptedToolFailed (header : Nat)
  | exhausted
  | failedPermanent
  deriving DecidableEq, Repr

structure State where
  request : CanonicalOutput.DocId
  scope : Nat
  turn : Nat
  phase : Phase
  budget : Budget
  transportUsed : Nat
  resampleUsed : Nat
  repairUsed : Bool
  lastParseError : Option String
  now : Time
  deadline : Option Time
  attempt : Nat
  /-- Monotone projection of usage already accounted by the InferenceCall owner.
  This retry policy preserves it across retraction and acceptance; it is not a
  second usage ledger and does not prove charge completeness or idempotency. -/
  usageCharged : Nat
  deriving DecidableEq, Repr

def fitsDeadline (wake : Time) (deadline : Option Time) : Prop :=
  match deadline with
  | none => True
  | some d => wake ≤ d

instance (wake : Time) (deadline : Option Time) : Decidable (fitsDeadline wake deadline) := by
  unfold fitsDeadline
  cases deadline <;> infer_instance

def State.accepted (s : State) : Prop :=
  (∃ header, s.phase = .accepted header) ∨
    (∃ header, s.phase = .acceptedToolFailed header)

def State.terminal (s : State) : Prop :=
  s.accepted ∨ s.phase = .exhausted ∨ s.phase = .failedPermanent

end CompletionRetry
