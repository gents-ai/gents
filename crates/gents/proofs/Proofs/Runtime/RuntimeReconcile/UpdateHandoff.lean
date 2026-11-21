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
Idle is never inferred from durable request rows. The adapter keeps persisted
drain/health deadlines; one persisted post-resume health grace permits forward
startup after offline intervals, without unbounded crash-driven extension. -/
def step (phase : Phase) : Action → Option Phase
  | .register verified => if phase = .staged ∧ verified then some .registered else none
  | .acknowledge => if phase = .registered then some .accepted else none
  | .retire idle => if phase = .accepted ∧ idle then some .retired else none
  | .release exclusive => if phase = .retired ∧ exclusive then some .released else none
  | .backup complete => if phase = .released ∧ complete then some .backedUp else none
  | .install unchanged => if phase = .backedUp ∧ unchanged then some .installed else none
  | .verify ready => if phase = .installed ∧ ready then some .healthy else none
  | .fail => if phase ≠ .healthy ∧ phase ≠ .failed then some .failed else none

inductive Registrant where
  | submitter | status
  deriving DecidableEq, Repr

inductive HelperRegistration where
  | register | helperOwns | finished | submitterOwns
  deriving DecidableEq, Repr

/-- Whether a caller registers an operation's update helper job, decided under
the operation's `helper.lock` on the receipt read under it. The lock's holder is
either the helper, from its start through the terminal cleanup that removes its
job definition, or a registrant deciding this same question; before
acknowledgement that registrant can only be the submitter (`statusTakesLock`).
So a caller that cannot take the lock leaves registration to its holder.
External premise: the lock is an OS advisory lock, which the kernel drops when
its holder exits. A finished operation's helper only retires again. Before
acknowledgement the submitter owns registration and waits for the helper
itself. -/
def helperRegistration (caller : Registrant) (lockTaken : Bool) (phase : Phase) :
    HelperRegistration :=
  if !lockTaken then .helperOwns
  else match phase, caller with
    | .healthy, _ | .failed, _ => .finished
    | .staged, .status | .registered, .status => .submitterOwns
    | _, _ => .register

/-- Whether a status reader takes the operation's `helper.lock` for the phase it
read before locking. Before acknowledgement only the submitter and the helper
may contend for the lock, so a submitter that cannot take it knows the helper
holds it; submitters of one home are serialized by their submission lock. A
stale pre-lock read there can only skip a registration that a later status read
or the submitter performs. -/
def statusTakesLock (phase : Phase) : Bool :=
  match phase with
  | .accepted | .retired | .released | .backedUp | .installed => true
  | _ => false

/-- Whether a candidate's preflight may create a staged operation. `verified`
is the digest, platform, handoff and schema checks the candidate reports.
Keychain custody additionally needs the candidate to have read this home's
store key with Keychain interaction disabled: a build the Keychain item does
not trust would otherwise block on an approval dialog after the old runtime
retired. Other custody keeps its existing preflight.
External premise: the Keychain item's access list matches the candidate's code
requirement, not its path, so the private staged copy decides as the file the
service will run. -/
def preflight (verified keychainCustody keyReadableWithoutInteraction : Bool) :
    Option Phase :=
  if verified && (!keychainCustody || keyReadableWithoutInteraction) then some .staged
  else none

theorem unreadable_keychain_never_staged (verified : Bool) :
    preflight verified true false = none := by
  simp [preflight]

theorem other_custody_unchanged (verified readable : Bool) :
    preflight verified false readable = (if verified then some .staged else none) := by
  cases verified <;> simp [preflight]

/-- Registration starts only from the phase preflight admitted: a refused
candidate has no receipt and so no registration. -/
def registerAfterPreflight (verified keychainCustody keyReadable : Bool) : Option Phase :=
  (preflight verified keychainCustody keyReadable).bind fun phase => step phase (.register true)

theorem rejected_preflight_cannot_register (verified keychain readable : Bool)
    (h : preflight verified keychain readable = none) :
    registerAfterPreflight verified keychain readable = none := by
  simp [registerAfterPreflight, h]

theorem unreadable_keychain_cannot_register (verified : Bool) :
    registerAfterPreflight verified true false = none := by
  simp [registerAfterPreflight, preflight]

/-- A different executable may migrate before readiness; it is never rolled
back after a startup attempt or publication of its service definition: a
reboot/external start can open that definition after helper death. Reconfiguration of the identical digest may be
restored after idle retirement/store exclusion. The adapter validates identity,
definitions and network and never force-stops active work. -/
def canRestore (phase : Phase) (candidateMayHaveOpened sameBuild : Bool) : Bool :=
  (!candidateMayHaveOpened || sameBuild) && (phase == .retired || phase == .released ||
    phase == .backedUp || phase == .installed)

theorem changed_build_restore_requires_unopened_candidate (phase : Phase) (opened : Bool)
    (h : canRestore phase opened false = true) : opened = false := by
  cases opened <;> simp_all [canRestore]

theorem retirement_requires_acknowledgement (phase : Phase) (idle : Bool) (next : Phase)
    (h : step phase (.retire idle) = some next) : phase = .accepted ∧ idle = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem installation_requires_cold_backup (phase : Phase) (unchanged : Bool) (next : Phase)
    (h : step phase (.install unchanged) = some next) :
    phase = .backedUp ∧ unchanged = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem register_requires_lock_and_live_phase (caller : Registrant) (lockTaken : Bool)
    (phase : Phase) (h : helperRegistration caller lockTaken phase = .register) :
    lockTaken = true ∧ phase ≠ .healthy ∧ phase ≠ .failed := by
  cases caller <;> cases lockTaken <;> cases phase <;> simp_all [helperRegistration]

theorem status_registers_only_acknowledged_operations (lockTaken : Bool) (phase : Phase)
    (h : helperRegistration .status lockTaken phase = .register) :
    phase ≠ .staged ∧ phase ≠ .registered := by
  cases lockTaken <;> cases phase <;> simp_all [helperRegistration]

theorem status_takes_lock_only_where_it_registers (phase : Phase) :
    statusTakesLock phase = true ↔ helperRegistration .status true phase = .register := by
  cases phase <;> simp [statusTakesLock, helperRegistration]

theorem finished_phases_have_no_successor (action : Action) :
    step .healthy action = none ∧ step .failed action = none := by
  cases action <;> simp [step]
end RuntimeReconcile.UpdateHandoff
