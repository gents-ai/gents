import Proofs.TitleText
import Lean

namespace Conformance.TitleText
open Lean

private structure Case where
  name : String
  raw : String
  fallback : String := ""
  generatedSource : Bool := true
  recent : List String := []

private def cases : List Case := [
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
  { name := "long-unicode-token", raw := String.mk (List.replicate 100 '界') }
]

private def caseJson (c : Case) : Json := Json.mkObj [
  ("name", toJson c.name), ("raw", toJson c.raw), ("fallback", toJson c.fallback),
  ("generated_source", toJson c.generatedSource), ("recent", toJson c.recent),
  ("expected_generated", toJson (_root_.TitleText.generated c.raw c.fallback)),
  ("expected_display", toJson (_root_.TitleText.display c.generatedSource c.raw)),
  ("expected_prompt", toJson (_root_.TitleText.prompt c.fallback c.recent))
]

def casesJson : String := (Json.mkObj [
  ("preamble", toJson _root_.TitleText.preamble),
  ("cases", toJson (cases.map caseJson))]).compress
end Conformance.TitleText
