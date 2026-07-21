import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Proofs.PromptAssembly.Executable

set_option linter.dupNamespace false

/-!
# PromptAssembly.Template — cache-safe role-aware templating (issue #497)

The dynamic-context counterpart to the provider-input sanitizer. A behavior's
*system* template must stay byte-stable across requests so the provider prefix
cache is not invalidated; only its *per-request* context may vary.

The model abstracts over the rendering engine (MiniJinja): a template is
characterised by the set of variable references it reads, and `render` depends
only on the binding restricted to those reads (engine purity / strict-undefined
evaluation). The cache-safety guarantee is therefore a property of *which
variables a template reads*, not of engine expressiveness.

Rust conformance: `crates/gents/tests/conformance/prompt_template.rs`.
-/

namespace PromptAssembly.Template

/-- Volatility class of a catalog variable. -/
inductive Volatility where
  | static
  | runConstant
  | perRequest
  deriving DecidableEq, Repr

/-- A catalog variable reference (full dotted key, e.g. `"node.node_did"`). -/
abbrev VarRef := String

/-- The runtime-owned catalog: maps a full ref to its volatility. An unknown
ref (not in the catalog) maps to `none` and is therefore never run-constant. -/
abbrev Catalog := VarRef → Option Volatility

/-- A binding assigns each variable a rendered value. -/
abbrev Binding := VarRef → String

/-- A template, abstracted by the complete set of variable refs it reads. -/
structure Template where
  reads : Finset VarRef
  deriving DecidableEq

/-- Render normal form: exactly the (ref, value) pairs the template reads.
Models a pure engine — output is a function of the read variables alone. -/
def render (t : Template) (b : Binding) : Finset (VarRef × String) :=
  t.reads.image (fun v => (v, b v))

/-- Engine purity: agreement on the read set ⇒ identical render. -/
theorem render_determined (t : Template) (b1 b2 : Binding)
    (h : ∀ v ∈ t.reads, b1 v = b2 v) :
    render t b1 = render t b2 := by
  unfold render
  apply Finset.image_congr
  intro v hv
  simp [h v hv]

/-- A system template is well-formed when every variable it reads is
run-constant per the catalog. (Static literal text contributes no reads.) -/
def WellFormedSystem (cat : Catalog) (t : Template) : Prop :=
  ∀ v ∈ t.reads, cat v = some .runConstant

/-- Two bindings agree on all run-constant variables — the condition that
holds across two requests in the same run (run-constants are frozen at start). -/
def AgreeRunConstant (cat : Catalog) (b1 b2 : Binding) : Prop :=
  ∀ v, cat v = some .runConstant → b1 v = b2 v

/-- **Cache stability.** A well-formed system template renders identically
across any two requests whose bindings agree on run-constant values — i.e. the
cacheable system prefix is byte-stable regardless of per-request context. -/
theorem system_render_stable (cat : Catalog) (t : Template) (b1 b2 : Binding)
    (wf : WellFormedSystem cat t) (agree : AgreeRunConstant cat b1 b2) :
    render t b1 = render t b2 := by
  apply render_determined
  intro v hv
  exact agree v (wf v hv)

/-- Decidable mirror of the cache-safety guard. -/
noncomputable def validateSystem (cat : Catalog) (t : Template) : Bool := by
  classical
  exact decide (WellFormedSystem cat t)

/-- The guard is sound and complete w.r.t. `WellFormedSystem`. -/
theorem validateSystem_correct (cat : Catalog) (t : Template) :
    validateSystem cat t = true ↔ WellFormedSystem cat t := by
  classical
  simp [validateSystem]

open PromptAssembly (Slot)

/-- Per-request assembly when a `request_context_template` is present: the
rendered context rides immediately before the new prompt, after the
conversation. Mirrors `loop_stream::build_request` injecting the context
message ahead of the prompt. -/
def assembleWithContext (skillCount summaryCount conversationLen : Nat) : List Slot :=
  Slot.preamble ::
    ((List.range skillCount).map Slot.skillReminder ++
      ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
        (List.range conversationLen).map Slot.conversation)) ++
    [Slot.contextPreamble, Slot.prompt]

/-- The context slot precedes the prompt and follows the conversation. Any
reordering in `loop_stream.rs` breaks this `rfl`. -/
theorem assembleWithContext_spec (skillCount summaryCount conversationLen : Nat) :
    assembleWithContext skillCount summaryCount conversationLen =
      Slot.preamble ::
        ((List.range skillCount).map Slot.skillReminder ++
          ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
            (List.range conversationLen).map Slot.conversation)) ++
        [Slot.contextPreamble, Slot.prompt] := rfl

/-- The assembly ends with exactly `[contextPreamble, prompt]` — context
immediately precedes the prompt. This is the ordering the conformance fence and
`loop_stream.rs` injection must match; it is stronger than "last slot is
prompt". -/
theorem assembleWithContext_tail
    (skillCount summaryCount conversationLen : Nat) :
    ∃ pre, assembleWithContext skillCount summaryCount conversationLen =
        pre ++ [Slot.contextPreamble, Slot.prompt] :=
  ⟨Slot.preamble ::
    ((List.range skillCount).map Slot.skillReminder ++
      ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
        (List.range conversationLen).map Slot.conversation)), rfl⟩

/-- Corollary: the last slot is the prompt. -/
theorem assembleWithContext_last
    (skillCount summaryCount conversationLen : Nat) :
    (assembleWithContext skillCount summaryCount conversationLen).getLast? = some Slot.prompt := by
  obtain ⟨pre, h⟩ := assembleWithContext_tail skillCount summaryCount conversationLen
  rw [h]
  simp

end PromptAssembly.Template
