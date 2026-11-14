namespace ClientInstallation

/-- `released`: the listener has drained every attachment. `closed`: the
installation's store lock is free as well (the client core has shut down and
the last reference to its store claim has gone). -/
inductive Phase where
  | accepting
  | draining
  | released
  | closed
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  attachments : Nat
  history : Nat
  deriving DecidableEq, Repr

inductive Action where
  | attach
  | detach
  | retire
  | release
  | close
  | acknowledge
  | replace (startupReserved : Bool)
  deriving DecidableEq, Repr

/-- Reuse is based on the wire contract the caller needs. Executable identity
is retained for fencing a later retirement request, but it cannot establish
which of two builds is newer. Same-protocol command changes therefore remain
additive; incompatible wire changes require a protocol version change. -/
def canReuse (protocolMatches frameLimitMatches : Bool)
    (requiredCommands availableCommands : List String) : Bool :=
  protocolMatches && frameLimitMatches && requiredCommands.all availableCommands.contains

theorem compatible_host_reuses_regardless_of_build_identity
    (requiredCommands availableCommands : List String)
    (commandsPresent : requiredCommands.all availableCommands.contains = true) :
    canReuse true true requiredCommands availableCommands = true := by
  simp [canReuse, commandsPresent]

/-- The listener owns admission and its store lock. Retirement stops new
attachments; existing attachments remain counted until their non-cancellable
operations settle through the connection owner. Only then may shutdown release
that store (`close`), and only a closed installation acknowledges the
retirement: the process that asked for it starts the successor as soon as it
reads the answer, and a successor that opened the store while this one still
held it would be refused (`tla/StoreHandoff.tla`, `AckFollowsRelease` and
`AckedHandoff`; the TLC counterexample to the earlier order is BeginStop, Ack,
Start, TryLock). Replacement therefore also starts from a closed store, and
additionally requires the existing startup lock. These locks protect process
lifetime, not runtime DID or document authorization. -/
def step (state : State) : Action → Option State
  | .attach =>
      if state.phase = .accepting then
        some { state with attachments := state.attachments + 1 }
      else none
  | .detach =>
      if (state.phase = .accepting ∨ state.phase = .draining) ∧ 0 < state.attachments then
        some { state with attachments := state.attachments - 1 }
      else none
  | .retire =>
      if state.phase = .accepting then some { state with phase := .draining } else none
  | .release =>
      if state.phase = .draining ∧ state.attachments = 0 then
        some { state with phase := .released }
      else none
  | .close =>
      if state.phase = .released then some { state with phase := .closed } else none
  | .acknowledge =>
      if state.phase = .closed then some state else none
  | .replace reserved =>
      if state.phase = .closed ∧ state.attachments = 0 ∧ reserved = true then
        some { state with phase := .accepting }
      else none

theorem history_is_preserved (before after : State) (action : Action)
    (h : step before action = some after) : after.history = before.history := by
  cases action <;> simp only [step] at h
  all_goals split at h <;> simp_all
  all_goals cases h; rfl

theorem retirement_preserves_pending_attachments (before after : State)
    (h : step before .retire = some after) : after.attachments = before.attachments := by
  simp only [step] at h
  split at h <;> simp_all
  cases h
  rfl

theorem release_requires_drained_attachments (before after : State)
    (h : step before .release = some after) : before.attachments = 0 := by
  simp only [step] at h
  split at h <;> simp_all

theorem draining_refuses_new_attachments (state : State) (h : state.phase = .draining) :
    step state .attach = none := by
  simp [step, h]

theorem replacement_requires_closed_owner (before after : State) (reserved : Bool)
    (h : step before (.replace reserved) = some after) :
    before.phase = .closed ∧ before.attachments = 0 ∧ reserved = true := by
  simp only [step] at h
  split at h <;> simp_all

/-- A retirement is acknowledged only once the installation's store is free. -/
theorem acknowledgement_requires_closed_store (before after : State)
    (h : step before .acknowledge = some after) : before.phase = .closed := by
  simp only [step] at h
  split at h <;> simp_all

/-- The store closes only after the listener drained, so no attachment can be
working on it when it is released. -/
theorem close_requires_drained_listener (before after : State)
    (h : step before .close = some after) : before.phase = .released := by
  simp only [step] at h
  split at h <;> simp_all

end ClientInstallation
