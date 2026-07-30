import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Union

namespace Skills

abbrev ToolId := String
abbrev SkillId := String
abbrev Did := String

inductive Scope
  | principal
  | behavior
  deriving DecidableEq, Repr

structure Skill where
  id       : SkillId
  owner    : Did
  scope    : Scope
  toolRefs : Finset ToolId
  enabled  : Bool
  deriving DecidableEq

structure Behavior where
  id            : String
  principal     : Did
  ceiling       : Finset ToolId
  skillRefs     : Finset SkillId
  skillExcludes : Finset SkillId

def candidates (skills : Finset Skill) (b : Behavior) : Finset Skill :=
  skills.filter (fun s =>
    s.owner = b.principal ∧
    s.enabled = true ∧
    (s.scope = Scope.principal ∨ s.id ∈ b.skillRefs) ∧
    s.id ∉ b.skillExcludes)

def skillTools (b : Behavior) (s : Skill) : Finset ToolId :=
  s.toolRefs ∩ b.ceiling

def resolvedSurface (b : Behavior) (active : Finset Skill) : Finset ToolId :=
  b.ceiling ∪ active.biUnion (skillTools b)

theorem activation_subset_ceiling (b : Behavior) (active : Finset Skill) :
    resolvedSurface b active ⊆ b.ceiling := by
  unfold resolvedSurface
  apply Finset.union_subset (Finset.Subset.refl _)
  rw [Finset.biUnion_subset]
  intro s _
  unfold skillTools
  exact Finset.inter_subset_right

theorem candidates_respect_principal (skills : Finset Skill) (b : Behavior)
    {s : Skill} (hs : s ∈ candidates skills b) :
    s.owner = b.principal ∧ s.enabled = true := by
  unfold candidates at hs
  rw [Finset.mem_filter] at hs
  exact ⟨hs.2.1, hs.2.2.1⟩

theorem composition_closed (skills : Finset Skill) (b : Behavior)
    (active : Finset Skill) (_hsub : active ⊆ candidates skills b) :
    resolvedSurface b active ⊆ b.ceiling :=
  activation_subset_ceiling b active

end Skills
