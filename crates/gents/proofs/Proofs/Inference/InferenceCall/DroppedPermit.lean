import Proofs.Inference.InferenceCall.SlotAccounting

/-! The terminal an admission permit records when it is dropped before it was
finished (#1366). A cancelled call token wins; otherwise the reason observed
for the call (a stream stall, or the reason an observer was given) is the
failure; otherwise the drop is a stream drop before any terminal response.
The observed reason is recorded as given; redaction and bounding of its text
are a separate owner. -/
namespace InferenceCall.DroppedPermit

inductive DropOutcome where
  | cancelled
  | observed
  | streamDropped
  deriving DecidableEq, Repr

def outcome (cancelled : Bool) (observed : Option String) : DropOutcome :=
  if cancelled then .cancelled
  else match observed with
    | some _ => .observed
    | none => .streamDropped

theorem cancelled_token_wins (observed : Option String) :
    outcome true observed = .cancelled := by
  simp [outcome]

theorem observed_reason_is_the_failure (reason : String) :
    outcome false (some reason) = .observed := by
  simp [outcome]

theorem silent_drop_is_a_stream_drop :
    outcome false none = .streamDropped := by
  simp [outcome]

theorem cancellation_never_becomes_a_failure (observed : Option String) :
    outcome true observed ≠ .observed ∧ outcome true observed ≠ .streamDropped := by
  simp [outcome]

/-- The state the dropped call's row records. -/
def recordedState : DropOutcome → InferenceCallState
  | .cancelled => .cancelled
  | .observed => .failed
  | .streamDropped => .failed

/-- Which failure reason the dropped call's row records: the cancellation
reason, the observed reason (as the failure-text owner records it), or the
stream-drop reason. -/
inductive RecordedReason where
  | cancelled
  | observedReason
  | streamDropped
  deriving DecidableEq, Repr

def recordedReason : DropOutcome → RecordedReason
  | .cancelled => .cancelled
  | .observed => .observedReason
  | .streamDropped => .streamDropped

/-- Backend slots a single call holds, reconstructed from its row's state. -/
def slotsHeld (state : InferenceCallState) : Nat :=
  if InferenceCallState.holdsBackendSlot state then 1 else 0

theorem a_dropped_permit_releases_its_slot (cancelled : Bool) (observed : Option String) :
    slotsHeld (recordedState (outcome cancelled observed)) = 0 := by
  cases cancelled <;> cases observed <;>
    simp [slotsHeld, recordedState, outcome, InferenceCallState.cancelled_no_backend_slot,
      InferenceCallState.failed_no_backend_slot]

theorem a_held_permit_holds_one_slot : slotsHeld .running = 1 := by
  simp [slotsHeld, InferenceCallState.running_holds_backend_slot]

theorem cancellation_is_recorded_as_cancelled (observed : Option String) :
    recordedState (outcome true observed) = .cancelled ∧
      recordedReason (outcome true observed) = .cancelled := by
  simp [recordedState, recordedReason, outcome]

end InferenceCall.DroppedPermit
