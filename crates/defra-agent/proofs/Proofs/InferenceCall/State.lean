import Proofs.Scheduling

/-!
# Inference Call State

State vocabulary and request linkage for persisted `InferenceCall` rows.
-/

/-- Persisted call states used by the Rust admission controller. -/
inductive InferenceCallState where
  | queued
  | running
  | cancelled
  | completed
  | failed
  deriving DecidableEq, Repr

namespace InferenceCallState

instance : HasTerminal InferenceCallState where
  isTerminal s := s = .cancelled ∨ s = .completed ∨ s = .failed
  isTerminal_dec s :=
    match s with
    | .cancelled => isTrue (Or.inl rfl)
    | .completed => isTrue (Or.inr (Or.inl rfl))
    | .failed => isTrue (Or.inr (Or.inr rfl))
    | .queued => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h =>
            cases h with
            | inl h => cases h
            | inr h => cases h)
    | .running => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h =>
            cases h with
            | inl h => cases h
            | inr h => cases h)

/-- String vocabulary persisted in `InferenceCall.call_state`. -/
def toDefraDB : InferenceCallState → String
  | .queued => "queued"
  | .running => "running"
  | .cancelled => "cancelled"
  | .completed => "completed"
  | .failed => "failed"

/-- Parse the persisted `InferenceCall.call_state` vocabulary. -/
def fromDefraDB? : String → Option InferenceCallState
  | "queued" => some .queued
  | "running" => some .running
  | "cancelled" => some .cancelled
  | "completed" => some .completed
  | "failed" => some .failed
  | _ => none

theorem fromDefraDB_toDefraDB (s : InferenceCallState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

theorem terminal_iff (s : InferenceCallState) :
    isTerminal s ↔ s = .cancelled ∨ s = .completed ∨ s = .failed := by
  rfl

end InferenceCallState

/--
Closed set of system-generated `InferenceCall.failure_reason` values emitted
by backend admission and interrupt/drop paths.

Provider errors remain open strings and are intentionally outside this
vocabulary.
-/
inductive InferenceCallTerminalReason where
  | cancelled
  | backendGone
  | queueFull
  | streamDroppedBeforeTerminalResponse
  deriving DecidableEq, Repr

namespace InferenceCallTerminalReason

/-- String vocabulary persisted in `InferenceCall.failure_reason` for system reasons. -/
def toDefraDB : InferenceCallTerminalReason → String
  | .cancelled => "Cancelled"
  | .backendGone => "BackendGone"
  | .queueFull => "QueueFull"
  | .streamDroppedBeforeTerminalResponse => "StreamDroppedBeforeTerminalResponse"

/-- Parse system-generated `InferenceCall.failure_reason` values. -/
def fromDefraDB? : String → Option InferenceCallTerminalReason
  | "Cancelled" => some .cancelled
  | "BackendGone" => some .backendGone
  | "QueueFull" => some .queueFull
  | "StreamDroppedBeforeTerminalResponse" => some .streamDroppedBeforeTerminalResponse
  | _ => none

theorem fromDefraDB_toDefraDB (reason : InferenceCallTerminalReason) :
    fromDefraDB? reason.toDefraDB = some reason := by
  cases reason <;> rfl

end InferenceCallTerminalReason

/-- A single persisted inference call, linked to its request by request id. -/
structure InferenceCall where
  callId : Nat
  requestId : RequestId
  backend : BackendId
  state : InferenceCallState
  deriving Repr

namespace InferenceCall

/-- A call is live while it can still enter provider work or hold backend work. -/
def isLive (call : InferenceCall) : Prop :=
  call.state = .queued ∨ call.state = .running

instance (call : InferenceCall) : Decidable call.isLive := by
  unfold isLive
  infer_instance

/-- The same predicate, named for cancellation theorems. -/
def cancellable (call : InferenceCall) : Prop :=
  call.isLive

/-- A call is linked to a request when both carry the same `request_id`. -/
def linkedTo (call : InferenceCall) (requestId : RequestId) : Prop :=
  call.requestId = requestId

/-- Model update for a persisted cancellation. -/
def cancel (call : InferenceCall) : InferenceCall :=
  { call with state := .cancelled }

theorem cancel_state (call : InferenceCall) :
    call.cancel.state = .cancelled := by
  rfl

theorem cancel_preserves_requestId (call : InferenceCall) :
    call.cancel.requestId = call.requestId := by
  rfl

theorem cancel_preserves_backend (call : InferenceCall) :
    call.cancel.backend = call.backend := by
  rfl

end InferenceCall
