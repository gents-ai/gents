import Lean
import Proofs.Session.PromptAssembly.Provider

namespace PromptAssembly.ProviderSanitation
open Lean

structure Part where
  kind : String
  id : Nat
  deriving ToJson

structure Row where
  role : String
  result : Option Nat
  content : List Part
  deriving ToJson

structure Input where
  rows : List Row
  deriving ToJson

def toProvider (index : Nat) (row : Row) : Provider.ProviderRow :=
  let content := row.content.map fun part =>
    if part.kind == "call" then Content.Item.call part.id else Content.Item.text part.id
  let kind := match row.result with
    | some id => Transcript.MessageKind.toolResult id ⟨1, id, id⟩
    | none =>
      let calls := Content.callsOf content
      if calls = ∅ then Transcript.MessageKind.ordinary else .assistantToolCalls calls
  ⟨⟨index, 1, index, if row.role == "assistant" then .assistant else .user, kind⟩, content⟩

def fromProvider (row : Provider.ProviderRow) : Row :=
  ⟨row.row.role.toDefraDB, match row.row.kind with | .toolResult id _ => some id | _ => none,
    row.content.map fun item => match item with
      | .call id => ⟨"call", id⟩
      | .text id | .other id => ⟨"text", id⟩⟩

/-- Content-bearing sanitation uses the existing provider owner. The fixture
vocabulary has unique call IDs, and numeric text tokens preserve full content
identity through its representation adapter. -/
def sanitize (input : Input) : List Row :=
  (Provider.sanitizeForProviderGlobal (input.rows.zipIdx.map fun (row, index) => toProvider index row)).map fromProvider

end PromptAssembly.ProviderSanitation
