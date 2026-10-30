namespace RuntimeReconcile.UpdateHandoff
inductive Phase where
  | staged | registered | accepted | retired | released | backedUp | installed | healthy | failed
  deriving DecidableEq, Repr
inductive Action where
  | register (verified : Bool)
  | acknowledge
  | retire (authenticatedIdle : Bool)
  | release (storeExclusive : Bool)
  | backup (complete : Bool)
  | install (unchanged : Bool)
  | verify (buildReady : Bool)
  | fail
  deriving DecidableEq, Repr
/-- Native-service handoff, not an agent job lifecycle. Inputs come from the
existing authenticated retirement, store lock, and native service owners.
Registration verification includes a readiness acknowledgement from the independently
supervised helper, before the initiating tool acknowledges acceptance.
Idle is never inferred from durable request rows. -/
def step (phase : Phase) : Action → Option Phase
  | .register verified => if phase = .staged ∧ verified then some .registered else none
  | .acknowledge => if phase = .registered then some .accepted else none
  | .retire idle => if phase = .accepted ∧ idle then some .retired else none
  | .release exclusive => if phase = .retired ∧ exclusive then some .released else none
  | .backup complete => if phase = .released ∧ complete then some .backedUp else none
  | .install unchanged => if phase = .backedUp ∧ unchanged then some .installed else none
  | .verify ready => if phase = .installed ∧ ready then some .healthy else none
  | .fail => if phase ≠ .healthy ∧ phase ≠ .failed then some .failed else none

theorem retirement_requires_acknowledgement (phase : Phase) (idle : Bool) (next : Phase)
    (h : step phase (.retire idle) = some next) : phase = .accepted ∧ idle = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem installation_requires_cold_backup (phase : Phase) (unchanged : Bool) (next : Phase)
    (h : step phase (.install unchanged) = some next) :
    phase = .backedUp ∧ unchanged = true := by
  simp only [step] at h
  split at h <;> simp_all
end RuntimeReconcile.UpdateHandoff
