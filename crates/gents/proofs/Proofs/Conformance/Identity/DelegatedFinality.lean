import Proofs.Store.Identity.DelegatedFinality
import Proofs.Conformance.Family

namespace Conformance.DelegatedFinality

open Lean Identity.DelegatedFinality

deriving instance ToJson for Native, Knowledge

inductive ActionKind where
  | begin
  | loseAck
  | pendingRead
  | childCas
  | otherFailureCas
  | exactReceipt
  | abortEvidence
  deriving ToJson

structure ActionWire where
  kind : ActionKind
  valid : Option Bool
  deriving ToJson

def ActionWire.ofModel : Action → ActionWire
  | .begin => ⟨.begin, none⟩
  | .loseAck => ⟨.loseAck, none⟩
  | .pendingRead => ⟨.pendingRead, none⟩
  | .childCas => ⟨.childCas, none⟩
  | .otherFailureCas => ⟨.otherFailureCas, none⟩
  | .exactReceipt receiptMatches => ⟨.exactReceipt, some receiptMatches⟩
  | .abortEvidence valid => ⟨.abortEvidence, some valid⟩

def ActionWire.toModel (action : ActionWire) : Action :=
  match action.kind with
  | .begin => .begin
  | .loseAck => .loseAck
  | .pendingRead => .pendingRead
  | .childCas => .childCas
  | .otherFailureCas => .otherFailureCas
  | .exactReceipt => .exactReceipt (action.valid.getD false)
  | .abortEvidence => .abortEvidence (action.valid.getD false)

structure TraceInput where
  name : String
  actions : List ActionWire
  deriving ToJson

structure Finality where
  native : Native
  knowledge : Knowledge
  deriving ToJson

private def traceInput (c : String × List Action) : TraceInput :=
  { name := c.1, actions := c.2.map ActionWire.ofModel }

private def finality (i : TraceInput) : Finality :=
  let s := run (i.actions.map ActionWire.toModel)
  { native := s.native, knowledge := s.knowledge }

@[conformance_family]
def served : Family TraceInput Finality where
  inputs := cases.map traceInput
  model := finality

end Conformance.DelegatedFinality
