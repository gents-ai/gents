namespace ClientTelemetry

structure State where
  configured : Bool
  consent : Bool
  persisted : Bool
  opened : Bool
  inFlight : Bool
  deriving DecidableEq, Repr

inductive Action where
  | open | enable (writeOk diskAfter : Bool) | disable (writeOk diskAfter : Bool) | restart
  deriving DecidableEq, Repr

/-- Consent is durable only after a successful settings write. Revocation
cancels in-flight work; it cannot retract bytes already sent to the service. -/
def step (state : State) : Action → State × Bool
  | .open =>
      let send := !state.opened && state.configured && state.consent
      ({ state with opened := true, inFlight := state.inFlight || send }, send)
  | .enable writeOk diskAfter =>
      (if state.configured then
        { state with consent := if writeOk then true else state.consent, persisted := diskAfter }
       else state, false)
  | .disable _writeOk diskAfter =>
      ({ state with consent := false, persisted := diskAfter, inFlight := false }, false)
  | .restart => ({ state with consent := state.persisted, opened := false, inFlight := false }, false)

theorem no_open_without_consent (state : State) (h : state.consent = false) :
    (step state .open).2 = false := by simp [step, h]

theorem enable_preserves_in_flight (state : State) (writeOk diskAfter : Bool) :
    (step state (.enable writeOk diskAfter)).1.inFlight = state.inFlight := by
  simp [step]; split <;> rfl

theorem no_duplicate_open (state : State) (h : state.opened = true) :
    (step state .open).2 = false := by simp [step, h]

theorem disable_cancels_and_persists_off (state : State) :
    (step state (.disable true false)).1.consent = false ∧
    (step state (.disable true false)).1.persisted = false ∧
    (step state (.disable true false)).1.inFlight = false := by simp [step]

theorem failed_disable_still_revokes_current_process (state : State) (diskAfter : Bool) :
    (step state (.disable false diskAfter)).1.consent = false ∧
    (step state (.disable false diskAfter)).1.inFlight = false := by simp [step]

end ClientTelemetry
