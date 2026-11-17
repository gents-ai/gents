/-! Read leases observe only the originally pinned home. Native directory
handles implement identity equality and anchor file opens; their filesystem
semantics are an explicit premise, not a claim proved by this pure model. -/
namespace PluginStoreHome

inductive Mode where
  | read | bootstrap | writer
  deriving DecidableEq, Repr

inductive Outcome where
  | acquired | absent | busy
  deriving DecidableEq, Repr

structure Observation where
  /-- Equality of pinned and current native directory identity. -/
  sameHome : Bool
  storePresent : Bool
  lockPresent : Bool
  contended : Bool
  deriving DecidableEq, Repr

structure Decision where
  outcome : Outcome
  creates : Bool
  deriving DecidableEq, Repr

/-- Read never creates. Bootstrap and writers alone may create, and a
runtime-pinned caller of any mode cannot authorize a replaced home. -/
def acquire (mode : Mode) (o : Observation) : Decision :=
  if !o.sameHome then ⟨.absent, false⟩
  else if mode == .read && (!o.storePresent || !o.lockPresent) then ⟨.absent, false⟩
  else ⟨if o.contended then .busy else .acquired,
        mode != .read && (!o.storePresent || !o.lockPresent)⟩

theorem read_has_no_filesystem_creation (o : Observation) :
    (acquire .read o).creates = false := by
  rcases o with ⟨same, store, lock, busy⟩
  cases same <;> cases store <;> cases lock <;> cases busy <;> rfl

theorem only_explicit_creators (mode : Mode) (o : Observation)
    (h : (acquire mode o).creates = true) : mode = .bootstrap ∨ mode = .writer := by
  cases mode <;> simp_all [read_has_no_filesystem_creation]

theorem replaced_home_not_created (mode : Mode) (o : Observation)
    (h : o.sameHome = false) :
    acquire mode o = ⟨.absent, false⟩ := by
  simp [acquire, h]

theorem absent_read_does_not_masquerade_as_contention (o : Observation)
    (h : o.storePresent = false) : (acquire .read o).outcome = .absent := by
  simp [acquire, h]

end PluginStoreHome
