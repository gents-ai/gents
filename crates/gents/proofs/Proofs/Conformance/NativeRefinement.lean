import Proofs.ToolCalls.Background.Observation
import Proofs.ToolCalls.Background.ProcessControl
import Proofs.ToolCalls.PluginJob.TargetResolution
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.ToolCalls

namespace Conformance.NativeRefinement

open Lean Background.Observation PluginJob.TargetResolution

def observationCases : List (State × Input) := do
  let now ← [0, 4, 5, 9, 10]
  let cancelled ← [false, true]
  let feedOpen ← [false, true]
  let change ← [.quiet, .unrelated, .relevant, .overflow, .closed]
  pure (⟨5, feedOpen⟩, ⟨now, cancelled, 10, change⟩)

def bindings : List Binding :=
  [⟨"review-local", "node-local", "reviewer"⟩,
   ⟨"review-remote", "node-remote", "reviewer"⟩,
   ⟨"review-alias", "node-local", "reviewer"⟩,
   ⟨"reviewer", "node-local", "other-agent"⟩]

def clientProcessClaimCasesJson : String :=
  let cases := do
    let claimant ← ["agent", "other"]
    let owner ← [none, some "client", some "other"]
    let requestedId ← ["call", "other"]
    let requestedDoc ← ["doc", "other"]
    let read ← [false, true]
    let update ← [false, true]
    let invoke ← [false, true]
    let currentRevision ← [0, 1]
    let intent : Background.ProcessControl.ClientIntent :=
      ⟨"client", ⟨requestedId, requestedDoc⟩, 0⟩
    pure (Json.mkObj [
      ("claimant", toJson claimant), ("owner", toJson owner),
      ("requested_id", toJson requestedId), ("requested_doc", toJson requestedDoc),
      ("read", toJson read), ("update", toJson update), ("invoke", toJson invoke),
      ("current_revision", toJson currentRevision),
      ("expected", toJson (Background.ProcessControl.agentMayClaim
        "agent" claimant intent owner ⟨"call", "doc"⟩ read update invoke 0 currentRevision))])
  (toJson cases).compress

def clientProcessReceiptCasesJson : String :=
  let expected : Background.ProcessControl.ControlReceipt :=
    ⟨"intent", "client", "agent", ⟨"call", "doc"⟩⟩
  let cases := do
    let intentDoc ← ["intent", "other"]
    let author ← ["client", "other"]
    let agent ← ["agent", "other"]
    let call ← ["call", "other"]
    let document ← ["doc", "other"]
    let signatureValid ← [false, true]
    let observedResult ← [Background.ProcessControl.ControlResult.applied,
      Background.ProcessControl.ControlResult.unconfirmed]
    let signedResult ← [Background.ProcessControl.ControlResult.applied,
      Background.ProcessControl.ControlResult.unconfirmed]
    let receipt : Background.ProcessControl.ControlReceipt :=
      ⟨intentDoc, author, agent, ⟨call, document⟩⟩
    let resultName := fun result => if result == Background.ProcessControl.ControlResult.applied
      then "applied" else "unconfirmed"
    pure (Json.mkObj [
      ("intent_doc", toJson intentDoc), ("author", toJson author),
      ("agent", toJson agent), ("call", toJson call), ("document", toJson document),
      ("signature_valid", toJson signatureValid),
      ("observed_result", toJson (resultName observedResult)),
      ("signed_result", toJson (resultName signedResult)),
      ("expected", toJson (Background.ProcessControl.receiptAccepted receipt expected signatureValid &&
        Background.ProcessControl.signedResultMatches observedResult signedResult))])
  (toJson cases).compress

def clientControlGrantCasesJson : String :=
  let cases := do
    let authority ← [false, true]
    let fenced ← [false, true]
    pure (Json.mkObj [("authority", toJson authority), ("fenced", toJson fenced),
      ("expected", toJson (Background.ProcessControl.mayPublishOutcome authority fenced))])
  (toJson cases).compress

deriving instance ToJson for Change
deriving instance ToJson for Wake
deriving instance ToJson for Binding

structure ObservationInput where
  rescanAt : Nat
  feedOpen : Bool
  now : Nat
  cancelled : Bool
  deadlineAt : Nat
  change : Change
  deriving ToJson

structure ObservationOutput where
  wake : Wake
  feedOpen : Bool
  rescanAt : Nat
  deriving ToJson

@[conformance_family]
def observation : Family ObservationInput ObservationOutput where
  inputs := observationCases.map fun (state, input) =>
    { rescanAt := state.rescanAt, feedOpen := state.feedOpen, now := input.now
      cancelled := input.cancelled, deadlineAt := input.deadlineAt, change := input.change }
  model := fun i =>
    let state : State := ⟨i.rescanAt, i.feedOpen⟩
    let observed := observeChange state i.change
    { wake := wake state ⟨i.now, i.cancelled, i.deadlineAt, i.change⟩
      feedOpen := observed.feedOpen
      rescanAt := observed.rescanAt }

structure Endpoint where
  node : String
  agent : String
  deriving ToJson

structure TargetInput where
  bindings : List Binding
  wire : String
  ids : List String
  target : Endpoint
  deriving ToJson

structure TargetOutput where
  guestTargets : List String
  resolved : Option Endpoint
  permitted : Bool
  deriving ToJson

@[conformance_family]
def targetResolution : Family TargetInput TargetOutput where
  inputs := do
    let table ← [[], bindings.take 3, bindings]
    let wire ← ["review-local", "review-remote", "review-alias", "reviewer", "missing"]
    let ids ← [[], ["review-local"], ["review-remote"], ["reviewer"], ["review-alias"],
      ["missing"], ["review-remote", "missing", "review-local"]]
    let target ← [("node-local", "reviewer"), ("node-remote", "reviewer"),
      ("node-local", "other-agent")]
    pure { bindings := table, wire, ids, target := ⟨target.1, target.2⟩ }
  model := fun i =>
    { guestTargets := guestTargets i.ids i.bindings
      resolved := (resolve i.bindings i.wire).map fun pair => ⟨pair.1, pair.2⟩
      permitted := permits i.bindings i.ids (i.target.node, i.target.agent) }

structure IdentityInput where
  ownedId : String
  ownedDoc : String
  requestedId : String
  requestedDoc : String
  deriving ToJson

@[conformance_family]
def processIdentity : Family IdentityInput Bool where
  inputs := do
    let ownedId ← ["call", "spawned:parent-doc"]
    let ownedDoc ← ["tool-doc-a", "tool-doc-b"]
    let requestedId ← ["call", "spawned:parent-doc", "parent-doc"]
    let requestedDoc ← ["tool-doc-a", "tool-doc-b", "missing"]
    pure { ownedId, ownedDoc, requestedId, requestedDoc }
  model := fun i => Background.ProcessControl.sameInvocation
    ⟨i.requestedId, i.requestedDoc⟩ ⟨i.ownedId, i.ownedDoc⟩

structure SignedControlInput where
  author : String
  node : String
  agent : String
  session : String
  request : String
  call : String
  document : String
  action : String
  signatureValid : Bool
  deriving ToJson

@[conformance_family]
def signedControl : Family SignedControlInput Bool where
  inputs := do
    let author ← ["client", "other"]
    let node ← ["node", "other"]
    let agent ← ["agent", "other"]
    let session ← ["session-doc", "other"]
    let request ← ["request-doc", "other"]
    let call ← ["call", "other"]
    let document ← ["tool-doc", "other"]
    let action ← ["process_stop", "job_stop"]
    let signatureValid ← [false, true]
    pure { author, node, agent, session, request, call, document, action, signatureValid }
  model := fun i =>
    Background.ProcessControl.signedScopeAccepted
      ⟨i.author, i.node, i.agent, i.session, i.request, ⟨i.call, i.document⟩, i.action⟩
      ⟨"client", "node", "agent", "session-doc", "request-doc", ⟨"call", "tool-doc"⟩, "process_stop"⟩
      i.signatureValid

structure StopInput where
  state : ToolExecution.ToolCallState
  deriving ToJson

@[conformance_family]
def stopAction : Family StopInput Background.ProcessControl.StopAction where
  inputs := ToolExecution.ToolCallState.all.map fun state => { state }
  model := fun i => Background.ProcessControl.stopAction i.state

end Conformance.NativeRefinement
