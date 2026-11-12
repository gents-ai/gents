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

end InferenceCall.DroppedPermit
