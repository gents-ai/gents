/-! Read-only publication acquisition. Native directory handles and advisory
locking are explicit premises. Runtime/offline writers are outside this model. -/
namespace PluginStoreHome
inductive Outcome where
  | acquired | absent | busy
  deriving DecidableEq, Repr
structure Observation where
  sameHome : Bool
  storePresent : Bool
  lockPresent : Bool
  contended : Bool
  deriving DecidableEq, Repr
structure Decision where
  outcome : Outcome
  creates : Bool
  deriving DecidableEq, Repr

def acquire (o : Observation) : Decision :=
  if !o.sameHome || !o.storePresent || !o.lockPresent then ⟨.absent, false⟩
  else ⟨if o.contended then .busy else .acquired, false⟩

theorem read_has_no_filesystem_creation (o : Observation) :
    (acquire o).creates = false := by
  rcases o with ⟨a,b,c,d⟩
  cases a <;> cases b <;> cases c <;> cases d <;> rfl

theorem replaced_home_not_created (o : Observation) (h : o.sameHome = false) :
    acquire o = ⟨.absent, false⟩ := by simp [acquire, h]

theorem absent_read_does_not_masquerade_as_contention (o : Observation)
    (h : o.storePresent = false) : (acquire o).outcome = .absent := by
  simp [acquire, h]
end PluginStoreHome
