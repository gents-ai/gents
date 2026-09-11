import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.Card
import Proofs.PromptAssembly.Executable
import Proofs.Configuration
import Proofs.Skills

/-!
# Prompt assembly: literal system prompt, task-only templates

System prompts stay literal at request time: they are not templates and do
not consult a volatility catalog. `Volatility`, `Catalog`, `WellFormedSystem`,
and `validateSystem` are deleted; only task prompt templates remain, rendered
per invocation against an explicit binding. The `contextPreamble` slot for the
removed dynamic request context is gone (see `PromptAssembly.Slot`), and layer
order is preserved: preamble first, skill reminders, compaction/conversation
layers, prompt last.
-/

set_option linter.dupNamespace false

namespace PromptAssembly.Template

abbrev VarRef := String

abbrev Binding := VarRef → String

/-- A task prompt template reads a fixed set of variables, rendered per
invocation. -/
structure Template where
  reads : Finset VarRef
  deriving DecidableEq

def render (t : Template) (b : Binding) : Finset (VarRef × String) :=
  t.reads.image (fun v => (v, b v))

/-- Generic task render theorem: rendering depends only on the read variables. -/
theorem render_determined (t : Template) (b1 b2 : Binding)
    (h : ∀ v ∈ t.reads, b1 v = b2 v) :
    render t b1 = render t b2 := by
  unfold render
  apply Finset.image_congr
  intro v hv
  simp [h v hv]

/-- Use the existing skill selector for reminder eligibility. The context's
whitelist alone is insufficient: unavailable, foreign, and disabled skills do
not contribute reminder slots. Tool authority remains the resolved ceiling. -/
def activeSkills (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) : Finset Skills.Skill :=
  Skills.select available
    { principal := principal, ceiling := context.toolNames.toFinset,
      skillIds := context.skillIds.toFinset }

theorem active_skill_owned_enabled (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill)
    (h : skill ∈ activeSkills context principal available) :
    skill.owner = principal ∧ skill.enabled = true :=
  Skills.select_respect_principal available _ h

theorem active_skill_whitelisted (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill)
    (h : skill ∈ activeSkills context principal available) : skill.id ∈ context.skillIds := by
  have hs := Skills.select_ids_subset_whitelist available
    { principal := principal, ceiling := context.toolNames.toFinset,
      skillIds := context.skillIds.toFinset }
  simpa using hs (Finset.mem_image.mpr ⟨skill, h, rfl⟩)

/-- Bind task variables only at the task-prompt slot of the existing assembler.
Literal preamble content comes from the resolved context; reminder count comes
from actual skill selection. Layer ordering remains owned by `assemble`. -/
def bindTask (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (task : Template) (binding : Binding)
    (summaryCount conversationLen : Nat) : List (Slot × Option (Finset (VarRef × String))) :=
  (assemble (activeSkills context principal available).card summaryCount conversationLen (some context.instructions)).map
    (fun slot => (slot, if slot = .prompt then some (render task binding) else none))

/-- The actual assembled first slot contains the exact resolved instruction bytes
and receives no task-variable substitutions. -/
theorem assembled_preamble_literal (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context principal available task binding summaryCount conversationLen).head? =
      some (.preamble (some context.instructions), none) := by
  simp [bindTask, assemble, perTurnRequest]

/-- Changing invocation bindings cannot change any context/conversation slot or
its literal payload. Only the prompt slot can acquire task substitution values. -/
theorem task_binding_preserves_context (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (task : Template)
    (a b : Binding) (summaryCount conversationLen : Nat) :
    ((bindTask context principal available task a summaryCount conversationLen).filter (fun item => item.1 != .prompt)) =
      ((bindTask context principal available task b summaryCount conversationLen).filter (fun item => item.1 != .prompt)) := by
  unfold bindTask
  rw [List.filter_map, List.filter_map]
  apply List.map_congr_left
  intro slot hs
  simp only [List.mem_filter, Function.comp_def, bne_iff_ne] at hs
  simp [hs.2]

/-- Task substitutions land at the final task-prompt slot, rather than being
silently discarded to satisfy the literal-context guarantee. -/
theorem assembled_task_rendered (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context principal available task binding summaryCount conversationLen).getLast? =
      some (.prompt, some (render task binding)) := by
  simp only [bindTask, assemble, perTurnRequest, List.map_cons, List.map_append,
    List.map_singleton, List.map_nil, reduceCtorEq, ↓reduceIte]
  exact List.getLast?_concat _

/-- The derived structural sequence is exactly the existing assembler, with
skill slots counted from the owned, enabled selection, never raw references. -/
theorem bound_task_uses_existing_assembler (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (task : Template)
    (binding : Binding) (summaryCount conversationLen : Nat) :
    (bindTask context principal available task binding summaryCount conversationLen).map Prod.fst =
      assemble (activeSkills context principal available).card summaryCount conversationLen (some context.instructions) := by
  simp [bindTask, List.map_map, Function.comp_def]

/-- Adding a disabled skill cannot add reminders, even if its ID is selected. -/
theorem disabled_skill_adds_no_reminders (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill) (h : skill.enabled = false)
    (task : Template) (binding : Binding) (summaryCount conversationLen : Nat) :
    bindTask context principal (insert skill available) task binding summaryCount conversationLen =
      bindTask context principal available task binding summaryCount conversationLen := by
  simp [bindTask, activeSkills, Skills.select, Finset.filter_insert, h]

/-- A foreign skill cannot add reminders to this principal's execution. -/
theorem foreign_skill_adds_no_reminders (context : Configuration.Context) (principal : String)
    (available : Finset Skills.Skill) (skill : Skills.Skill) (h : skill.owner ≠ principal)
    (task : Template) (binding : Binding) (summaryCount conversationLen : Nat) :
    bindTask context principal (insert skill available) task binding summaryCount conversationLen =
      bindTask context principal available task binding summaryCount conversationLen := by
  simp [bindTask, activeSkills, Skills.select, Finset.filter_insert, h]

end PromptAssembly.Template
