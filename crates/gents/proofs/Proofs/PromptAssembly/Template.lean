import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Proofs.PromptAssembly.Executable

set_option linter.dupNamespace false

namespace PromptAssembly.Template

inductive Volatility where
  | static
  | runConstant
  | perRequest
  deriving DecidableEq, Repr

abbrev VarRef := String

abbrev Catalog := VarRef → Option Volatility

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

def WellFormedSystem (cat : Catalog) (t : Template) : Prop :=
  ∀ v ∈ t.reads, cat v = some .runConstant

def AgreeRunConstant (cat : Catalog) (b1 b2 : Binding) : Prop :=
  ∀ v, cat v = some .runConstant → b1 v = b2 v

theorem system_render_stable (cat : Catalog) (t : Template) (b1 b2 : Binding)
    (wf : WellFormedSystem cat t) (agree : AgreeRunConstant cat b1 b2) :
    render t b1 = render t b2 := by
  apply render_determined
  intro v hv
  exact agree v (wf v hv)

noncomputable def validateSystem (cat : Catalog) (t : Template) : Bool := by
  classical
  exact decide (WellFormedSystem cat t)

theorem validateSystem_correct (cat : Catalog) (t : Template) :
    validateSystem cat t = true ↔ WellFormedSystem cat t := by
  classical
  simp [validateSystem]

open PromptAssembly (Slot)

def assembleWithContext (skillCount summaryCount conversationLen : Nat) : List Slot :=
  Slot.preamble ::
    ((List.range skillCount).map Slot.skillReminder ++
      ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
        (List.range conversationLen).map Slot.conversation)) ++
    [Slot.contextPreamble, Slot.prompt]

theorem assembleWithContext_spec (skillCount summaryCount conversationLen : Nat) :
    assembleWithContext skillCount summaryCount conversationLen =
      Slot.preamble ::
        ((List.range skillCount).map Slot.skillReminder ++
          ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
            (List.range conversationLen).map Slot.conversation)) ++
        [Slot.contextPreamble, Slot.prompt] := rfl

theorem assembleWithContext_tail
    (skillCount summaryCount conversationLen : Nat) :
    ∃ pre, assembleWithContext skillCount summaryCount conversationLen =
        pre ++ [Slot.contextPreamble, Slot.prompt] :=
  ⟨Slot.preamble ::
    ((List.range skillCount).map Slot.skillReminder ++
      ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
        (List.range conversationLen).map Slot.conversation)), rfl⟩

theorem assembleWithContext_last
    (skillCount summaryCount conversationLen : Nat) :
    (assembleWithContext skillCount summaryCount conversationLen).getLast? = some Slot.prompt := by
  obtain ⟨pre, h⟩ := assembleWithContext_tail skillCount summaryCount conversationLen
  rw [h]
  simp

end PromptAssembly.Template
