import Proofs.ToolPolicy.Meet

namespace PluginJob

/-- An AgentTarget wire ID resolves to `(target_node_did, agent_id)`. -/
abbrev Target := String × String

namespace TargetResolution

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

/-- Wire ceilings intersect before endpoint resolution. Two AgentTarget aliases
for one endpoint do not make disjoint live and pinned ID ceilings overlap. -/
def meetIds (live pinned : List String) : List String :=
  live.filter pinned.contains

def scope (bindings : List Binding) (ids : List String) : ToolPolicy.EndpointScope Target Unit :=
  .only ((bindings.filter fun binding => ids.contains binding.wire).map endpoint).toFinset
    (fun _ => ())

theorem meet_requires_same_wire (live pinned : List String) (wire : String) :
    wire ∈ meetIds live pinned ↔ wire ∈ live ∧ wire ∈ pinned := by
  simp [meetIds, List.contains_iff_mem]

theorem scope_permits_iff (bindings : List Binding) (ids : List String) (target : Target) :
    (scope bindings ids).permits target ↔ permits bindings ids target = true := by
  simp only [scope, ToolPolicy.EndpointScope.permits, List.mem_toFinset,
    List.mem_map, List.mem_filter, List.contains_iff_mem, permits,
    List.any_eq_true, Bool.and_eq_true, beq_iff_eq]
  constructor
  · rintro ⟨binding, ⟨member, named⟩, bound⟩
    exact ⟨binding, member, bound, named⟩
  · rintro ⟨binding, member, bound, named⟩
    exact ⟨binding, ⟨member, named⟩, bound⟩

theorem scope_mono (bindings : List Binding) (left right : List String)
    (subset : left ⊆ right) (target : Target)
    (allowed : (scope bindings left).permits target) :
    (scope bindings right).permits target := by
  rw [scope_permits_iff] at allowed ⊢
  have witness : ∃ binding ∈ bindings, endpoint binding = target ∧ binding.wire ∈ left := by
    simpa [permits, List.any_eq_true, Bool.and_eq_true, beq_iff_eq,
      List.contains_iff_mem] using allowed
  obtain ⟨binding, member, bound, named⟩ := witness
  simp only [permits, List.any_eq_true, Bool.and_eq_true, beq_iff_eq,
    List.contains_iff_mem]
  exact ⟨binding, member, bound, subset named⟩

theorem meet_scope_permits_iff (bindings : List Binding) (live pinned : List String)
    (target : Target) :
    (scope bindings (meetIds live pinned)).permits target ↔
      ∃ binding ∈ bindings, endpoint binding = target ∧
        binding.wire ∈ live ∧ binding.wire ∈ pinned := by
  simp [scope_permits_iff, permits, List.any_eq_true, Bool.and_eq_true,
    beq_iff_eq, List.contains_iff_mem, meet_requires_same_wire]

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

end TargetResolution
end PluginJob
