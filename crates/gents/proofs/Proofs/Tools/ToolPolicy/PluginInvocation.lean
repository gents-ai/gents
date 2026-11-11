namespace ToolPolicy.PluginInvocation

/-- Digests and serialized grant/declaration snapshots are compared by the native
executor. `binding` represents the remaining installed-record fields compared
when authorizing a per-call directory; they do not affect cached compilation.
Artifact parsing, hashing and filesystem observations remain native
obligations; cache membership never establishes current installation authority. -/
structure Record where
  digest : String
  grant : String
  declaration : String
  binding : String
  deriving BEq, DecidableEq, Repr

def admitted (current : Option Record) (pinned : String)
    (boundUnder : Option Record) : Bool :=
  match current with
  | none => false
  | some record => record.digest == pinned &&
      match boundUnder with
      | none => true
      | some bound => decide (record = bound)

def reusable (cached current : Record) : Bool :=
  cached.digest == current.digest && cached.grant == current.grant &&
    cached.declaration == current.declaration

theorem removed_denied (pinned : String) (bound : Option Record) :
    admitted none pinned bound = false := rfl

theorem admission_requires_pin (record : Record) (pinned : String)
    (bound : Option Record) (h : admitted (some record) pinned bound = true) :
    record.digest = pinned := by
  simp only [admitted, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1

theorem bound_admission_requires_current (record bound : Record) (pinned : String)
    (h : admitted (some record) pinned (some bound) = true) : record = bound := by
  simp only [admitted, Bool.and_eq_true, beq_iff_eq] at h
  exact of_decide_eq_true h.2

theorem cache_requires_current_grant (cached current : Record)
    (h : reusable cached current = true) : cached.grant = current.grant := by
  simp only [reusable, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.2

end ToolPolicy.PluginInvocation
