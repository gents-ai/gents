import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.Card
import Proofs.PromptAssembly.Executable
import Proofs.Configuration
import Proofs.Skills

set_option linter.dupNamespace false

namespace PromptAssembly.Template

def discoveryToolNames : Finset String :=
  {"discover_tools", "describe_tool", "call_tool"}

/-- MCP policy enablement alone does not establish the discovery interface:
flat selections expose direct tools and empty exact-name selections grant none.
The prompt may describe the discovery workflow only when all its operations
are actually presented. This predicate grants no service or tool permission. -/
def discoveryGuidance (enabled : Bool) (presented : Finset String) : Bool :=
  enabled && decide (discoveryToolNames ⊆ presented)

theorem discovery_guidance_requires_presented_tools (enabled : Bool)
    (presented : Finset String) (h : discoveryGuidance enabled presented = true) :
    discoveryToolNames ⊆ presented := by
  exact of_decide_eq_true (Bool.and_eq_true_iff.mp h).2

theorem disabled_discovery_has_no_guidance (presented : Finset String) :
    discoveryGuidance false presented = false := rfl

theorem complete_discovery_has_guidance :
    discoveryGuidance true discoveryToolNames = true := by
  simp [discoveryGuidance]

abbrev VarRef := String

abbrev Binding := VarRef → String

structure Template where
  reads : Finset VarRef
  deriving DecidableEq

def render (t : Template) (b : Binding) : Finset (VarRef × String) :=
  t.reads.image (fun v => (v, b v))

theorem render_determined (t : Template) (b1 b2 : Binding)
    (h : ∀ v ∈ t.reads, b1 v = b2 v) :
    render t b1 = render t b2 := by
  unfold render
  apply Finset.image_congr
  intro v hv
  simp [h v hv]

def activeSkills (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) : Finset Skills.Skill :=
  Skills.select available
    { node := node, ceiling := context.toolNames.toFinset,
      skillIds := context.skillIds.toFinset }

theorem active_skill_owned_enabled (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill)
    (h : skill ∈ activeSkills context node available) :
    skill.owner = node ∧ skill.enabled = true :=
  Skills.select_respect_node available _ h

theorem active_skill_whitelisted (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill)
    (h : skill ∈ activeSkills context node available) : skill.id ∈ context.skillIds := by
  have hs := Skills.select_ids_subset_whitelist available
    { node := node, ceiling := context.toolNames.toFinset,
      skillIds := context.skillIds.toFinset }
  simpa using hs (Finset.mem_image.mpr ⟨skill, h, rfl⟩)

def bindTask (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (task : Template) (binding : Binding)
    (summaryCount conversationLen : Nat) : List (Slot × Option (Finset (VarRef × String))) :=
  (assemble (activeSkills context node available).card summaryCount conversationLen (some context.instructions)).map
    (fun slot => (slot, if slot = .prompt then some (render task binding) else none))

theorem assembled_preamble_literal (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context node available task binding summaryCount conversationLen).head? =
      some (.preamble (some context.instructions), none) := by
  simp [bindTask, assemble, perTurnRequest]

theorem task_binding_preserves_context (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (task : Template)
    (a b : Binding) (summaryCount conversationLen : Nat) :
    ((bindTask context node available task a summaryCount conversationLen).filter (fun item => item.1 != .prompt)) =
      ((bindTask context node available task b summaryCount conversationLen).filter (fun item => item.1 != .prompt)) := by
  unfold bindTask
  rw [List.filter_map, List.filter_map]
  apply List.map_congr_left
  intro slot hs
  simp only [List.mem_filter, Function.comp_def, bne_iff_ne] at hs
  simp [hs.2]

theorem assembled_task_rendered (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context node available task binding summaryCount conversationLen).getLast? =
      some (.prompt, some (render task binding)) := by
  simp only [bindTask, assemble, perTurnRequest, List.map_cons, List.map_append,
    List.map_singleton, List.map_nil, reduceCtorEq, ↓reduceIte]
  exact List.getLast?_concat _

theorem bound_task_uses_existing_assembler (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context node available task binding summaryCount conversationLen).map Prod.fst =
      assemble (activeSkills context node available).card summaryCount conversationLen (some context.instructions) := by
  simp [bindTask, List.map_map, Function.comp_def]

theorem disabled_skill_adds_no_reminders (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill) (h : skill.enabled = false)
    (task : Template) (binding : Binding) (summaryCount conversationLen : Nat) :
    bindTask context node (insert skill available) task binding summaryCount conversationLen =
      bindTask context node available task binding summaryCount conversationLen := by
  simp [bindTask, activeSkills, Skills.select, Finset.filter_insert, h]

theorem foreign_skill_adds_no_reminders (context : Configuration.Context) (node : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill) (h : skill.owner ≠ node)
    (task : Template) (binding : Binding) (summaryCount conversationLen : Nat) :
    bindTask context node (insert skill available) task binding summaryCount conversationLen =
      bindTask context node available task binding summaryCount conversationLen := by
  simp [bindTask, activeSkills, Skills.select, Finset.filter_insert, h]

end PromptAssembly.Template
