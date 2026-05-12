import Proofs.Basic
import Proofs.Persistence
import Proofs.Scheduling
import Proofs.ToolExecution.State

/-!
# Request State

State vocabulary and per-request context shared by the relational and executable
request semantics. The persisted vocabulary includes `inputRequired` for
protocol compatibility, but the current core transition machine treats it as a
reserved non-terminal state rather than a state Rust emits today.
-/

/-- The 9 persisted states of the request lifecycle. -/
inductive RequestState where
  | pending
  | claimed
  | processing
  | inputRequired
  | completed
  | failed
  | superseded
  | dead
  | interrupted
  deriving DecidableEq, Repr

namespace RequestState

/-- String vocabulary persisted in `AgentRequest.lifecycle_state`. -/
def toDefraDB : RequestState → String
  | .pending => "pending"
  | .claimed => "claimed"
  | .processing => "processing"
  | .inputRequired => "inputRequired"
  | .completed => "completed"
  | .failed => "failed"
  | .superseded => "superseded"
  | .dead => "dead"
  | .interrupted => "interrupted"

/-- Parse the persisted `AgentRequest.lifecycle_state` vocabulary. -/
def fromDefraDB? : String → Option RequestState
  | "pending" => some .pending
  | "claimed" => some .claimed
  | "processing" => some .processing
  | "inputRequired" => some .inputRequired
  | "completed" => some .completed
  | "failed" => some .failed
  | "superseded" => some .superseded
  | "dead" => some .dead
  | "interrupted" => some .interrupted
  | _ => none

theorem fromDefraDB_toDefraDB (s : RequestState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

instance : HasTerminal RequestState where
  isTerminal s :=
    s = .completed ∨ s = .failed ∨ s = .superseded ∨ s = .dead ∨ s = .interrupted
  isTerminal_dec s :=
    match s with
    | .completed => isTrue (Or.inl rfl)
    | .failed => isTrue (Or.inr (Or.inl rfl))
    | .superseded => isTrue (Or.inr (Or.inr (Or.inl rfl)))
    | .dead => isTrue (Or.inr (Or.inr (Or.inr (Or.inl rfl))))
    | .interrupted => isTrue (Or.inr (Or.inr (Or.inr (Or.inr rfl))))
    | .pending => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .claimed => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .processing => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))
    | .inputRequired => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => cases h with
          | inl h => exact absurd h (by decide)
          | inr h => cases h with
            | inl h => exact absurd h (by decide)
            | inr h => cases h with
              | inl h => exact absurd h (by decide)
              | inr h => exact absurd h (by decide))

end RequestState

/-- Mutable per-request context that transitions carry along. -/
structure RequestContext where
  state        : RequestState
  origin       : ExecutionOrigin
  backend      : BackendId
  admission    : AdmissionState
  deadline     : Time
  claimTime    : Time
  currentTime  : Time
  retryCount   : Nat
  maxRetries   : Nat
  progressSeq  : Nat
  messageSeq   : Nat
  isLatest     : Bool
  persistence  : PersistenceState
  /-- Submitter-set interrupt timestamp; runtime-read-only. `none` means no interrupt requested. -/
  interruptRequestedAt : Option Time := none
  /-- Submitter-set TTL deadline; runtime-read-only. `none` means no TTL set. -/
  validUntil           : Option Time := none
  -- Subagent lineage / depth bound:
  subagentDepth                : Nat := 0
  causedByParentRequestId      : Option RequestId := none
  causedByParentToolCallId     : Option ToolExecution.ToolCallId := none
  deriving Repr

namespace RequestContext

/-- Coherence relation between lifecycle state and admission state. -/
def coherentStateAdmission : RequestState → AdmissionState → Prop
  | .pending, a => a = .released
  | .claimed, a => a = .waiting ∨ a = .acquired
  | .processing, a => a = .executing
  | .inputRequired, a => a = .executing
  | .completed, a => a = .released
  | .failed, a => a = .released
  | .superseded, a => a = .released
  | .dead, a => a = .released
  | .interrupted, a => a = .released

instance (s : RequestState) (a : AdmissionState) : Decidable (coherentStateAdmission s a) := by
  cases s <;> unfold coherentStateAdmission <;> infer_instance

/-- State/admission coherence for a concrete request context. -/
def coherent (r : RequestContext) : Prop :=
  coherentStateAdmission r.state r.admission

instance (r : RequestContext) : Decidable r.coherent := by
  unfold coherent
  infer_instance

/-- Whether the deadline has been exceeded. -/
def deadlineExceeded (r : RequestContext) : Prop :=
  r.currentTime > r.deadline

instance (r : RequestContext) : Decidable r.deadlineExceeded :=
  Nat.decLt r.deadline r.currentTime

/-- Submitter TTL is open exactly when no TTL exists or current time is at/before it. -/
def ttlOpen (r : RequestContext) : Prop :=
  match r.validUntil with
  | none => True
  | some t => r.currentTime ≤ t

instance (r : RequestContext) : Decidable r.ttlOpen := by
  unfold ttlOpen
  cases r.validUntil <;> infer_instance

/-- Whether retries are exhausted. -/
def retriesExhausted (r : RequestContext) : Prop :=
  r.retryCount ≥ r.maxRetries

instance (r : RequestContext) : Decidable r.retriesExhausted :=
  Nat.decLe r.maxRetries r.retryCount

/-- Project any terminal transition into a released admission state. -/
def releaseToTerminal (r : RequestContext) (terminal : RequestState) : RequestContext :=
  match terminal with
  | .completed => { r with state := .completed, admission := .released, persistence := .committed }
  | .failed => { r with state := .failed, admission := .released }
  | .superseded => { r with state := .superseded, admission := .released }
  | .dead => { r with state := .dead, admission := .released }
  | .interrupted => { r with state := .interrupted, admission := .released }
  | .pending => { r with admission := .released }
  | .claimed => { r with admission := .released }
  | .processing => { r with admission := .released }
  | .inputRequired => { r with admission := .released }

theorem releaseToTerminal_state
    {r : RequestContext} {terminal : RequestState}
    (h_terminal : isTerminal terminal) :
    (releaseToTerminal r terminal).state = terminal := by
  cases h_terminal with
  | inl h => simp [releaseToTerminal, h]
  | inr h =>
    cases h with
    | inl h => simp [releaseToTerminal, h]
    | inr h =>
      cases h with
      | inl h => simp [releaseToTerminal, h]
      | inr h =>
        cases h with
        | inl h => simp [releaseToTerminal, h]
        | inr h => simp [releaseToTerminal, h]

theorem releaseToTerminal_released
    (r : RequestContext) (terminal : RequestState) :
    (releaseToTerminal r terminal).admission = .released := by
  cases terminal <;> simp [releaseToTerminal]

theorem releaseToTerminal_backend
    (r : RequestContext) (terminal : RequestState) :
    (releaseToTerminal r terminal).backend = r.backend := by
  cases terminal <;> simp [releaseToTerminal]


end RequestContext
