import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Union

/-!
# Skills: explicit context whitelist selection

A context selects skills through an explicit `skill_ids` whitelist. `Skill.scope`, inheritance, and excludes are removed: empty
whitelist selects none, and no selection can reference a skill outside the
whitelist. Owner and enabled filtering and the tool ceiling are preserved.
-/

namespace Skills

abbrev ToolId := String
abbrev SkillId := String
abbrev Did := String

structure Skill where
  id       : SkillId
  owner    : Did
  toolRefs : Finset ToolId
  enabled  : Bool
  deriving DecidableEq

/-- Resolved context selection plus the existing effective tool ceiling. -/
structure Context where
  principal : Did
  ceiling  : Finset ToolId
  skillIds : Finset SkillId

/-- Skills selectable by `b`: explicitly whitelisted by the context, owned by
the behavior's principal, and enabled. -/
def select (skills : Finset Skill) (b : Context) : Finset Skill :=
  skills.filter (fun s =>
    s.id ∈ b.skillIds ∧
    s.owner = b.principal ∧
    s.enabled = true)

/-- Selection never exceeds the context whitelist. -/
theorem select_ids_subset_whitelist (skills : Finset Skill) (b : Context) :
    (select skills b).image Skill.id ⊆ b.skillIds := by
  intro id hid
  obtain ⟨s, hs, rfl⟩ := Finset.mem_image.mp hid
  unfold select at hs
  rw [Finset.mem_filter] at hs
  exact hs.2.1

/-- Consequently any active set within the selection cannot exceed the
whitelist. -/
theorem selected_subset_whitelist (skills : Finset Skill) (b : Context)
    (active : Finset Skill) (h : active ⊆ select skills b) :
    active.image Skill.id ⊆ b.skillIds :=
  Finset.Subset.trans (Finset.image_subset_image h)
    (select_ids_subset_whitelist skills b)

/-- Empty whitelist selects none. -/
theorem empty_whitelist_selects_none (skills : Finset Skill) (b : Context)
    (hb : b.skillIds = ∅) : select skills b = ∅ := by
  unfold select
  ext s
  rw [Finset.mem_filter, hb]
  simp

theorem select_subset_skills (skills : Finset Skill) (b : Context) :
    select skills b ⊆ skills := Finset.filter_subset _ _

theorem select_respect_principal (skills : Finset Skill) (b : Context)
    {s : Skill} (hs : s ∈ select skills b) :
    s.owner = b.principal ∧ s.enabled = true := by
  unfold select at hs
  rw [Finset.mem_filter] at hs
  exact ⟨hs.2.2.1, hs.2.2.2⟩

/-- A skill may recommend only tools already permitted by the resolved ceiling.
Recommendations never grant authority. -/
def skillTools (b : Context) (s : Skill) : Finset ToolId :=
  s.toolRefs ∩ b.ceiling

theorem recommended_tools_permitted (b : Context) (s : Skill) :
    skillTools b s ⊆ b.ceiling := Finset.inter_subset_right

/-- Activation selects skill content and carries existing tool authority unchanged.
There is no second tool-surface composition through skill references. -/
def activate (skills : Finset Skill) (b : Context) : Finset Skill × Finset ToolId :=
  (select skills b, b.ceiling)

theorem activation_preserves_tool_authority (skills : Finset Skill) (b : Context) :
    (activate skills b).2 = b.ceiling := rfl

theorem activation_skills_whitelisted (skills : Finset Skill) (b : Context) :
    (activate skills b).1.image Skill.id ⊆ b.skillIds :=
  select_ids_subset_whitelist skills b

end Skills
