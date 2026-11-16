import Proofs.Conformance.CanonicalOutputWire
import Proofs.Runtime.CanonicalOutput.Execution.AuxiliaryCases

namespace Conformance.AuxiliaryOutput

open Lean CanonicalOutput CanonicalOutput.Execution CanonicalOutput.Execution.Examples
open CanonicalOutput.Execution.AuxiliaryCases

structure CaseInput where
  name : String
  raw : Json
  outcome : String
  nativePublicationClosing : Json
  nativePublicationMessage : Json
  observation : Json
  deriving ToJson

def outcomeName : Outcome → String
  | .complete => "complete"
  | .«partial» => "partial"

def caseInput (witness : AuxiliaryCases.Case) : CaseInput :=
  { name := witness.name, raw := CanonicalOutputWire.segment (observed witness.kind)
    outcome := outcomeName witness.outcome
    nativePublicationClosing := CanonicalOutputWire.segment (nativePublicationClose witness.kind)
    nativePublicationMessage := CanonicalOutputWire.message nativePublicationMessage
    observation := CanonicalOutputWire.observation
      (AuxiliaryCases.observation witness.kind [observed witness.kind]) }

def caseNamed (name : String) : Option AuxiliaryCases.Case :=
  AuxiliaryCases.cases.find? (·.name == name)

theorem case_names_are_unique :
    (AuxiliaryCases.cases.map (·.name)).eraseDups.length = AuxiliaryCases.cases.length := by
  native_decide

def recovery (kind : AuxiliaryKind) :=
  recoverExpiredBatch (world 20 [observed kind]) 7 8 5 30 [⟨recoveryClose kind, none⟩]

structure ProjectionOutput where
  audit : Json
  publicView : Json
  recoveryClose : Option Json
  deriving ToJson

/-- The audit prefix, public projection and expired-lease recovery plan of the
raw auxiliary record alone. -/
@[conformance_family]
def projection : Family CaseInput (Option ProjectionOutput) where
  inputs := AuxiliaryCases.cases.map caseInput
  model := fun i => (caseNamed i.name).map fun witness =>
    let observation := AuxiliaryCases.observation witness.kind [observed witness.kind]
    { audit := CanonicalOutputWire.auditResult (StreamingResponse.reconstructAuditPrefix observation none)
      publicView := CanonicalOutputWire.view (StreamingResponse.project observation)
      recoveryClose := if succeeds (recovery witness.kind) then
        some (CanonicalOutputWire.close (recoveryClose witness.kind)) else none }

structure OwnedOutput where
  claimedAppendAccepted : Bool
  beginAccepted : Bool
  begunAppendAccepted : Bool
  nativePublicationAccepted : Bool
  closeAccepted : Bool
  staleCloseReplayAccepted : Bool
  deriving ToJson

/-- The owned guards on a served request: appends before and after the
existing begin, publishing a native reasoning header, closing, and replaying
the close under a stale writer. -/
@[conformance_family]
def owned : Family CaseInput (Option OwnedOutput) where
  inputs := AuxiliaryCases.cases.map caseInput
  model := fun i => (caseNamed i.name).map fun witness =>
    { claimedAppendAccepted := claimedAppendAccepted witness.kind
      beginAccepted := begunWorld?.isSome
      begunAppendAccepted := begunAppendAccepted witness.kind
      nativePublicationAccepted := nativePublicationAccepted witness.kind
      closeAccepted := succeeds (closeAuxiliary (world 5 [observed witness.kind]) 7
        (AuxiliaryCases.close witness.kind witness.outcome))
      staleCloseReplayAccepted := staleWriterCloseReplayAccepted witness.kind witness.outcome }

structure SinkOutput where
  close : Json
  segments : Option Nat
  audit : Json
  publicView : Json
  deriving ToJson

/-- The non-Claude auxiliary sink writes the raw reasoning and its closure;
the durable records audit exactly and never project as public output. -/
@[conformance_family]
def sink : Family CaseInput (Option SinkOutput) where
  inputs := AuxiliaryCases.cases.map caseInput
  model := fun i => (caseNamed i.name).map fun witness =>
    let closing := AuxiliaryCases.close witness.kind witness.outcome
    let observation := AuxiliaryCases.observation witness.kind [observed witness.kind]
    { close := CanonicalOutputWire.close closing
      segments := match closeAuxiliary (world 5 [observed witness.kind]) 7 closing with
        | .ok post => some post.segments.length
        | .error _ => none
      audit := CanonicalOutputWire.auditResult (StreamingResponse.reconstructAuditPrefix observation none)
      publicView := CanonicalOutputWire.view (StreamingResponse.project observation) }

example : claimedWorld.lease.request.toDefraDB = "claimed" := by native_decide

example : AuxiliaryCases.cases.all fun witness =>
    let raw := observed witness.kind
    raw.writer == .request 7 && claimedWorld.currentGeneration? == some 7 &&
      (AuxiliaryCases.close witness.kind witness.outcome).coordinate == raw.coordinate &&
      (AuxiliaryCases.observation witness.kind [raw]).target.coordinate == raw.coordinate &&
      (nativePublicationClose witness.kind).coordinate == raw.coordinate &&
      (nativePublicationClose witness.kind).writer == raw.writer &&
      (staleWriterClose witness.kind witness.outcome).writer != raw.writer := by
  native_decide

example : (emptyAssistant 200 5).blocks.isEmpty && (emptyAssistant 200 5).header.refs.isEmpty &&
    (emptyAssistant 200 5).header.role == .assistant &&
    (emptyAssistant 200 5).header.publication == .requestExecution 7 &&
    nativePublicationMessage.header.role == .assistant &&
    nativePublicationMessage.header.publication == .requestExecution 7 := by
  native_decide

example : AuxiliaryCases.cases.all fun witness =>
    (match (nativePublicationClose witness.kind).close with
      | some (.closed .complete _ _) => true
      | _ => false) &&
    succeeds (appendRaw (world 5) 7 (observed witness.kind)) &&
    succeeds (closeAuxiliary (world 5 [observed witness.kind]) 7
      (AuxiliaryCases.close witness.kind witness.outcome)) &&
    !succeeds (acceptAndPublish (world 5 [observed witness.kind]) 7
      (AuxiliaryCases.close witness.kind witness.outcome) (emptyAssistant 200 5) []) := by
  native_decide

end Conformance.AuxiliaryOutput
