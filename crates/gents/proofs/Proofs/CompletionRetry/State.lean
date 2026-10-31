import Proofs.CanonicalOutput.State

namespace CompletionRetry

inductive FailureClass
  | transport
  | parseBadRequest
  /-- The provider rejected replayed reasoning: a Claude thinking signature
  that no longer binds its prefix or cannot be verified, or Responses
  encrypted content it cannot decrypt. Resending the identical body fails
  identically, so this class never resamples. -/
  | reasoningRejected
  /-- The provider rejected the request because its input exceeds the
  model's context window. Resending the identical body fails identically, so
  this class never backs off or resamples; one forced reduction of the
  provider input, budgeted from the provider's own limit, may make it fit. -/
  | contextOverflow
  /-- The provider refused this account's quota while the account rotation
  (`AccountRotation.attempt`) holds another eligible account. The next attempt
  goes to that account at once: no backoff, no transport budget. -/
  | accountLimited
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
  | providerReasoningRejected
  | providerAccountLimited
  deriving DecidableEq, Repr

def FailureOrigin.class : FailureOrigin → FailureClass
  | .localRequestBuild => .permanent
  | .retryableTransport => .transport
  | .providerStreamMalformed => .transport
  | .providerReasoningRejected => .reasoningRejected
  | .providerAccountLimited => .accountLimited

structure Budget where
  transportRetries : Nat
  resampleRetries : Nat
  allowRepair : Bool
  /-- Other eligible accounts when the attempt was issued; one rotation each. -/
  accountHops : Nat := 0
  deriving DecidableEq, Repr

/-- Retry policy begins before publication. Accepted phases are absorbing
publication-side observations, not provider-attempt phases. -/
inductive Phase
  | issuing
  | streaming
  | retractRequired (failure : FailureClass) (error : String) (wake : Time)
  | retracted (failure : FailureClass) (error : String) (wake : Time)
  | backingOff (wake : Time)
  | repairing
  /-- The one forced provider-input reduction after a context overflow. -/
  | compacting
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
  /-- The forced overflow reduction is a one-shot capability per completion. -/
  compactUsed : Bool := false
  accountUsed : Nat := 0
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
