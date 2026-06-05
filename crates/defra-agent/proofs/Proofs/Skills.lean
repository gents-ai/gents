import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Union

/-!
# Skills — privilege algebra

Formal source-of-truth for defra-agent skills (spec
`docs/superpowers/specs/2026-06-02-skills-integration-design.md`).

A `Skill` declares the tools it *depends on* (`toolRefs`) — it never *grants*
them (D3, Codex-faithful). A behavior resolves a tool `ceiling` from its
`tool_selection`. Activation contributes, per skill, only `toolRefs ∩ ceiling`
(intersect + degrade), so the resolved surface with any skills active stays
`⊆ ceiling`. This module proves that privilege-monotonicity (S-Skill-1), the
composition closure under unions of active skills (S-Skill-2), and that the
D5 effective candidate set respects the owning principal (S-Skill-3).
-/

namespace Skills

abbrev ToolId := String
abbrev SkillId := String
abbrev Did := String

inductive Scope
  | principal
  | behavior
  deriving DecidableEq, Repr

/-- A skill: owned by a principal, scoped (D5), declaring tool dependencies. -/
structure Skill where
  id       : SkillId
  owner    : Did
  scope    : Scope
  toolRefs : Finset ToolId
  enabled  : Bool
  deriving DecidableEq

/-- A behavior: its resolved tool ceiling (D3) plus the D5 refinement lists. -/
structure Behavior where
  id            : String
  principal     : Did
  ceiling       : Finset ToolId
  skillRefs     : Finset SkillId
  skillExcludes : Finset SkillId

/-- D5 effective candidate set: principal-scoped skills inherit to every
    behavior of the owner; behavior-scoped skills are candidates only where
    opted in via `skillRefs`; `skillExcludes` opts out. -/
def candidates (skills : Finset Skill) (b : Behavior) : Finset Skill :=
  skills.filter (fun s =>
    s.owner = b.principal ∧
    s.enabled = true ∧
    (s.scope = Scope.principal ∨ s.id ∈ b.skillRefs) ∧
    s.id ∉ b.skillExcludes)

/-- Tools an active skill may use against a behavior ceiling: intersect +
    degrade (D3). Never adds a tool the behavior does not already allow. -/
def skillTools (b : Behavior) (s : Skill) : Finset ToolId :=
  s.toolRefs ∩ b.ceiling

/-- The tool surface available for a request with a set of `active` skills:
    the behavior ceiling plus each active skill's degraded contribution. -/
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

/-- S-Skill-2: activating any subset of the candidate set still stays within
    the ceiling — no union of skills escalates privilege. -/
theorem composition_closed (skills : Finset Skill) (b : Behavior)
    (active : Finset Skill) (_hsub : active ⊆ candidates skills b) :
    resolvedSurface b active ⊆ b.ceiling :=
  activation_subset_ceiling b active

end Skills
