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

/-- A served session-message commit can hold its native commit, race a
competing failure against it, or lose its post-commit acknowledgement; these
traces are the ones those three situations perform. -/
def servedNames : List String :=
  ["child-wins-after-pending-read", "failure-wins-before-child", "child-committed-ack-lost"]

@[conformance_family]
def served : Family TraceInput Finality where
  inputs := (cases.filter fun c => c.1 ∈ servedNames).map traceInput
  model := finality

structure UnreachedFinality where
  native : Native
  knowledge : Knowledge
  attempts : Nat
  mayReportFailure : Bool
  mayRetry : Bool
  deriving ToJson

/-- model-production-gap: the session-message commit has no caller-side
knowledge state that replays a pending read, a foreign receipt or an invalid
abort proof, and no attempt counter or retry/report permission; production
neither retries nor reports after a lost acknowledgement, so these traces have
no production form yet. -/
@[conformance_family]
def servedUnreached : Family TraceInput UnreachedFinality where
  inputs := (cases.filter fun c => c.1 ∉ servedNames).map traceInput
  model := fun i =>
    let s := run (i.actions.map ActionWire.toModel)
    { native := s.native, knowledge := s.knowledge, attempts := s.started
      mayReportFailure := mayReportFailure s, mayRetry := mayRetry s }

end Conformance.DelegatedFinality
