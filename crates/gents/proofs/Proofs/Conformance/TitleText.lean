import Proofs.Session.TitleText
import Proofs.Session.PromptAssembly.CapturedContext
import Proofs.Conformance.CapturedContextWire
import Proofs.Conformance.Family
import Lean

namespace Conformance.TitleText
open Lean

structure Case where
  name : String
  raw : String
  fallback : String := ""
  generatedSource : Bool := true
  recent : List String := []
  captures : List Enrollment.CapturedContext := []
  deriving ToJson

private def capture (text : String) : Enrollment.CapturedContext :=
  { kind := .file, source := .editor, capturedAt := "2026-10-20T00:00:00Z",
    unsaved := true, content := text }

def cases : List Case := [
  { name := "sentence-case", raw := "fix the CI failures" },
  { name := "technical-names", raw := "Fix vera.rs CI and OAuth login" },
  { name := "generated-slug", raw := "address-highest-priority-issue" },
  { name := "user-slug-kept-exact", raw := "this-is-my-name", generatedSource := false },
  { name := "user-spacing-kept-exact", raw := "  My custom title  ", generatedSource := false },
  { name := "quotes-and-extra-lines", raw := "\n `Review the runtime configuration`\nExplanation omitted" },
  { name := "unicode", raw := "Réparer les tests de démarrage" },
  { name := "unicode-whitespace", raw := "Fix\u00a0the\tmodel\nUnrelated text" },
  { name := "fallback", raw := "", fallback := "Can you fix the model selection in vera.rs?", recent := ["Model selection", "Runtime startup"] },
  { name := "empty", raw := "", fallback := "" },
  { name := "word-bound", raw := "one two three four five six seven eight nine ten" },
  { name := "character-bound", raw := "Investigate extraordinarily long configuration names while preserving meaningful words" },
  { name := "long-unicode-token", raw := String.mk (List.replicate 100 '界') },
  { name := "capture-only", raw := "", captures := [capture "Inspect captured workspace settings"] },
  { name := "capture-prose-precedence", raw := "", fallback := "Fix the selected function",
    captures := [capture "unrelated captured text"] },
  { name := "capture-order", raw := "", captures := [capture "First snapshot", capture "Second snapshot"] },
  { name := "empty-capture", raw := "", captures := [capture ""] },
  { name := "whitespace-capture", raw := "", fallback := "\t\n", captures := [capture " \n\t"] },
  { name := "long-capture-bounded-prompt", raw := "", captures := [capture (String.mk (List.replicate 1000 '界'))] }
]

structure GenerationOutput where
  preamble : String
  input : String
  generated : String
  display : String
  prompt : String
  deriving ToJson

@[conformance_family]
def generation : Family Case GenerationOutput where
  inputs := cases
  model := fun c =>
    let input := _root_.TitleText.inputText c.fallback c.captures
    { preamble := _root_.TitleText.preamble
      input
      generated := _root_.TitleText.generated c.raw input
      display := _root_.TitleText.display c.generatedSource c.raw
      prompt := _root_.TitleText.prompt input c.recent }

structure SummaryInput where
  name : String
  raw : String
  generatedSource : Bool
  deriving ToJson

@[conformance_family]
def summary : Family SummaryInput (Option String) where
  inputs := cases.map fun c => { name := c.name, raw := c.raw, generatedSource := c.generatedSource }
  model := fun i => some (_root_.TitleText.display i.generatedSource i.raw)

end Conformance.TitleText
