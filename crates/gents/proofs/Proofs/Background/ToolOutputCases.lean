import Proofs.Background.ToolOutput
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.ToolDelivery

namespace Subagent.ToolOutput.Cases

open CanonicalOutput CanonicalOutput.Execution
open CanonicalOutput.Execution.Examples

def output : Segment :=
  { id := 700, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
  , flush := some ⟨0, [⟨0, 4, some { block := 0, part := 0, kind := .toolOutput }⟩],
      [76, 73, 86, 69]⟩
  , close := none, createdAt := 5 }

def closing : Segment :=
  { id := 701, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
  , flush := none, close := some (.closed .complete 1 [4]), createdAt := 5 }

def running : Option World := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [foregroundAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  ToolDelivery.appendToolOutput dispatched 600 output |>.toOption

def closed : Option World := do
  let current ← running
  ToolDelivery.closeToolOutput current 600 (.native .complete) closing |>.toOption

def emptyOpen : Option World := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [foregroundAdmission]).toOption
  (dispatch accepted 7 permit).toOption

def emptyClosing : Segment :=
  { closing with id := 704, close := some (.closed .complete 0 []) }

def emptyClosed : Option World := do
  let current ← emptyOpen
  ToolDelivery.closeToolOutput current 600 (.native .complete) emptyClosing |>.toOption

def canonicalProjectionCases : Bool :=
  match running, closed with
  | some openWorld, some closedWorld =>
      project openWorld 600 == some ⟨.open, [76, 73, 86, 69]⟩ &&
      project closedWorld 600 == some ⟨.closed, [76, 73, 86, 69]⟩ &&
      (project openWorld 601).isNone &&
      (match ownedToolByDocument? openWorld 600 with
       | none => false
       | some original =>
           (project { openWorld with toolContexts := openWorld.toolContexts ++
             [{ original with document := 601 }] } 601).isNone) &&
      (project { openWorld with segments := openWorld.segments ++
        [{ output with id := 702, flush := some ⟨0,
          [⟨0, 4, some { block := 0, part := 0, kind := .toolOutput }⟩],
          [66, 65, 68, 33]⟩ }] } 600).isNone &&
      project { closedWorld with segments := closedWorld.segments ++
        [{ output with id := 703, flush := some ⟨1, [⟨0, 1, none⟩], [33]⟩ }] } 600 ==
          some ⟨.closed, [76, 73, 86, 69]⟩
  | _, _ => false

theorem canonical_open_closed_missing_conflict_and_late_suffix :
    canonicalProjectionCases = true := by native_decide

def canonicalEmptyProjectionCases : Bool :=
  match emptyOpen, emptyClosed with
  | some openWorld, some closedWorld =>
      project openWorld 600 == some ⟨.open, []⟩ &&
      project closedWorld 600 == some ⟨.closed, []⟩ &&
      (project openWorld 601).isNone &&
      (project { closedWorld with segments :=
        (closedWorld.segments.map fun segment =>
          if segment.id == emptyClosing.id then
            { segment with close := some (.closed .complete 1 []) }
          else segment) } 600).isNone
  | _, _ => false

theorem canonical_known_empty_open_and_closed_but_missing_extent_rejected :
    canonicalEmptyProjectionCases = true := by native_decide

end Subagent.ToolOutput.Cases
