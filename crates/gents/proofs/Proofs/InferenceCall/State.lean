import Proofs.Scheduling

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

def toDefraDB : InferenceCallState → String
  | .queued => "queued"
  | .running => "running"
  | .cancelled => "cancelled"
  | .completed => "completed"
  | .failed => "failed"

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

inductive InferenceCallTerminalReason where
  | cancelled
  | backendGone
  | queueFull
  | streamDroppedBeforeTerminalResponse
  deriving DecidableEq, Repr

namespace InferenceCallTerminalReason

def toDefraDB : InferenceCallTerminalReason → String
  | .cancelled => "Cancelled"
  | .backendGone => "BackendGone"
  | .queueFull => "QueueFull"
  | .streamDroppedBeforeTerminalResponse => "StreamDroppedBeforeTerminalResponse"

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

structure InferenceCall where
  callId : Nat
  requestId : RequestId
  backend : BackendId
  state : InferenceCallState
  deriving Repr

namespace InferenceCall

def isLive (call : InferenceCall) : Prop :=
  call.state = .queued ∨ call.state = .running

instance (call : InferenceCall) : Decidable call.isLive := by
  unfold isLive
  infer_instance

def cancellable (call : InferenceCall) : Prop :=
  call.isLive

def linkedTo (call : InferenceCall) (requestId : RequestId) : Prop :=
  call.requestId = requestId

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
