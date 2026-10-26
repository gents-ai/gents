import Proofs.PluginJob.Admission

namespace PluginJob.TargetResolution

/-- An AgentTarget document's wire ID and its resolved endpoint are distinct
identities. The native configuration owner supplies the ordered bindings;
resolving a wire spelling never falls back to matching an Agent ID. -/
structure Binding where
  wire : String
  node : String
  agent : String
  deriving DecidableEq, Repr

def endpoint (binding : Binding) : String × String := (binding.node, binding.agent)

def resolve (bindings : List Binding) (wire : String) : Option (String × String) :=
  (bindings.find? fun binding => binding.wire == wire).map endpoint

def permits (bindings : List Binding) (ids : List String) (target : String × String) : Bool :=
  bindings.any fun binding => endpoint binding == target && ids.contains binding.wire

theorem unknown_wire_cannot_resolve (bindings : List Binding) (wire : String)
    (absent : ∀ binding ∈ bindings, binding.wire ≠ wire) : resolve bindings wire = none := by
  have missing : (bindings.find? fun binding => binding.wire == wire) = none := by
    apply List.find?_eq_none.mpr
    intro binding member
    simp [absent binding member]
  simp [resolve, missing]

theorem resolution_comes_from_named_binding (bindings : List Binding) (wire : String)
    (target : String × String) (resolved : resolve bindings wire = some target) :
    ∃ binding ∈ bindings, binding.wire = wire ∧ endpoint binding = target := by
  cases found : bindings.find? (fun binding => binding.wire == wire) with
  | none => simp [resolve, found] at resolved
  | some binding =>
    have value : endpoint binding = target := by simpa [resolve, found] using resolved
    exact ⟨binding, List.mem_of_find?_eq_some found,
      by simpa using List.find?_some found, value⟩

theorem agent_spelling_does_not_grant_access (bindings : List Binding) (agent : String)
    (absent : ∀ binding ∈ bindings, binding.wire ≠ agent) : resolve bindings agent = none :=
  unknown_wire_cannot_resolve bindings agent absent

theorem permitted_endpoint_has_named_witness (bindings : List Binding) (ids : List String)
    (target : String × String) (allowed : permits bindings ids target = true) :
    ∃ binding ∈ bindings, endpoint binding = target ∧ binding.wire ∈ ids := by
  simpa [permits, List.any_eq_true, Bool.and_eq_true, beq_iff_eq,
    List.contains_iff_mem] using allowed

end PluginJob.TargetResolution
