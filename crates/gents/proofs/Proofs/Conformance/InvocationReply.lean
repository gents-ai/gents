import Proofs.Session.ToolExecution.InvocationReply
import Proofs.Conformance.Family

namespace ToolExecution.InvocationReply
deriving instance Lean.ToJson for Read
deriving instance Lean.ToJson for Source
end ToolExecution.InvocationReply

namespace Conformance.InvocationReply

open Lean ToolExecution ToolExecution.InvocationReply

/-- A spawn parent's transcript: whether its call completed with a reply,
whether a second copy of that reply was published, and whether a delivery
that is not a native reply names the call. -/
structure Transcript where
  completed : Bool
  duplicateReply : Bool
  foreignDelivery : Bool
  deriving ToJson

structure ReplyObserved where
  read : Read
  lifecycleState : Option String
  boundToCall : Bool
  result : Option Source
  rawResult : Option Source
  deriving ToJson

private def transcripts : List Transcript :=
  [false, true].flatMap fun completed =>
    (if completed then [false, true] else [false]).flatMap fun duplicateReply =>
      [false, true].map fun foreignDelivery => { completed, duplicateReply, foreignDelivery }

@[conformance_family]
def selection : Conformance.Family Transcript ReplyObserved where
  inputs := transcripts
  model := fun t =>
    let replies := (if t.completed then 1 else 0) + (if t.duplicateReply then 1 else 0)
    let deliveries := replies + (if t.foreignDelivery then 1 else 0)
    let observed := ToolExecution.InvocationReply.read deliveries replies false
    let state : ToolCallState := if t.completed then .completed else .running
    { read := observed
    , lifecycleState := if observed.admitted then some state.toDefraDB else none
    , boundToCall := observed.admitted
    , result := resultSource observed
    , rawResult := rawSource observed }

end Conformance.InvocationReply
