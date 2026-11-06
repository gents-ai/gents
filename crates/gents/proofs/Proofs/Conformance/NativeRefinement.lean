import Proofs.Background.Observation
import Proofs.Background.ProcessControl
import Proofs.PluginJob.TargetResolution
import Proofs.Conformance.ContractTypes

namespace Conformance.NativeRefinement

open Lean Background.Observation PluginJob.TargetResolution

def changeName : Change → String
  | .quiet => "quiet"
  | .unrelated => "unrelated"
  | .relevant => "relevant"
  | .overflow => "overflow"
  | .closed => "closed"

def wakeName : Wake → String
  | .cancel => "cancel"
  | .deadline => "deadline"
  | .rescan => "rescan"
  | .changed => "changed"
  | .wait => "wait"

def observationCases : List (State × Input) := do
  let now ← [0, 4, 5, 9, 10]
  let cancelled ← [false, true]
  let feedOpen ← [false, true]
  let change ← [.quiet, .unrelated, .relevant, .overflow, .closed]
  pure (⟨5, feedOpen⟩, ⟨now, cancelled, 10, change⟩)

def observationJson (test : State × Input) : Json :=
  let (state, input) := test
  Json.mkObj [
    ("rescan_at", toJson state.rescanAt), ("feed_open", toJson state.feedOpen),
    ("now", toJson input.now), ("cancelled", toJson input.cancelled),
    ("deadline_at", toJson input.deadlineAt), ("change", toJson (changeName input.change)),
    ("expected_wake", toJson (wakeName (wake state input))),
    ("expected_feed_open", toJson (observeChange state input.change).feedOpen),
    ("expected_rescan_at", toJson (observeChange state input.change).rescanAt)]

def observationCasesJson : String := (toJson (observationCases.map observationJson)).compress

def bindings : List Binding :=
  [⟨"review-local", "node-local", "reviewer"⟩,
   ⟨"review-remote", "node-remote", "reviewer"⟩,
   ⟨"review-alias", "node-local", "reviewer"⟩,
   ⟨"reviewer", "node-local", "other-agent"⟩]

def bindingJson (binding : Binding) : Json := Json.mkObj [
  ("wire", toJson binding.wire), ("node", toJson binding.node),
  ("agent", toJson binding.agent)]

def targetCasesJson : String :=
  let cases := do
    let table ← [[], bindings.take 3, bindings]
    let wire ← ["review-local", "review-remote", "review-alias", "reviewer", "missing"]
    let ids ← [[], ["review-local"], ["review-remote"], ["reviewer"], ["review-alias"],
      ["missing"], ["review-remote", "missing", "review-local"]]
    let target ← [("node-local", "reviewer"), ("node-remote", "reviewer"),
      ("node-local", "other-agent")]
    pure (Json.mkObj [
      ("bindings", toJson (table.map bindingJson)), ("wire", toJson wire),
      ("ids", toJson ids), ("target", toJson [target.1, target.2]),
      ("expected_guest_targets", toJson (guestTargets ids table)),
      ("expected_resolved", match resolve table wire with
        | none => Json.null
        | some pair => toJson [pair.1, pair.2]),
      ("expected_permitted", toJson (permits table ids target))])
  (toJson cases).compress

def processIdentityCasesJson : String :=
  let cases := do
    let ownedId ← ["call", "spawned:parent-doc"]
    let ownedDoc ← ["tool-doc-a", "tool-doc-b"]
    let requestedId ← ["call", "spawned:parent-doc", "parent-doc"]
    let requestedDoc ← ["tool-doc-a", "tool-doc-b", "missing"]
    pure (Json.mkObj [
      ("owned_id", toJson ownedId), ("owned_doc", toJson ownedDoc),
      ("requested_id", toJson requestedId), ("requested_doc", toJson requestedDoc),
      ("expected_cancel", toJson (Background.ProcessControl.sameInvocation
        ⟨requestedId, requestedDoc⟩ ⟨ownedId, ownedDoc⟩))])
  (toJson cases).compress

def operatorControlCasesJson : String :=
  let cases := do
    let executing ← ["did:node:a", "did:node:b"]
    let requested ← ["did:node:a", "did:node:b"]
    let owner ← ["did:node:a", "did:node:b"]
    let actor ← [none, some "did:node:a", some "did:node:b", some "did:operator:other"]
    pure (Json.mkObj [("executing", toJson executing), ("requested", toJson requested),
      ("owner", toJson owner), ("actor", toJson actor),
      ("expected", toJson (Background.ProcessControl.operatorAtNode executing requested owner actor))])
  (toJson cases).compress

def operatorProcessCasesJson : String :=
  let cases := do
    let actor ← [none, some "node", some "foreign"]
    let requested ← ["node", "foreign"]
    let owner ← ["node", "foreign"]
    let requestedId ← ["call", "other"]
    let requestedDoc ← ["doc", "other"]
    let background ← [false, true]
    let native ← [false, true]
    pure (Json.mkObj [
      ("executing", toJson "node"),
      ("owned_id", toJson "call"), ("owned_doc", toJson "doc"),
      ("actor", toJson actor), ("requested", toJson requested), ("owner", toJson owner),
      ("requested_id", toJson requestedId), ("requested_doc", toJson requestedDoc),
      ("background", toJson background), ("native", toJson native),
      ("expected", toJson (Background.ProcessControl.operatorProcessAuthorized
        "node" requested owner actor ⟨requestedId, requestedDoc⟩ ⟨"call", "doc"⟩
        background native))])
  (toJson cases).compress

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

def clientSignedControlCasesJson : String :=
  let expected : Background.ProcessControl.SignedControlScope :=
    ⟨"client", "node", "agent", "session-doc", "request-doc", ⟨"call", "tool-doc"⟩, "process_stop"⟩
  let cases := do
    let author ← ["client", "other"]
    let node ← ["node", "other"]
    let agent ← ["agent", "other"]
    let session ← ["session-doc", "other"]
    let request ← ["request-doc", "other"]
    let call ← ["call", "other"]
    let document ← ["tool-doc", "other"]
    let action ← ["process_stop", "job_stop"]
    let valid ← [false, true]
    let scope : Background.ProcessControl.SignedControlScope :=
      ⟨author, node, agent, session, request, ⟨call, document⟩, action⟩
    pure (Json.mkObj [("author", toJson author), ("node", toJson node),
      ("agent", toJson agent), ("session", toJson session), ("request", toJson request),
      ("call", toJson call), ("document", toJson document), ("action", toJson action),
      ("signature_valid", toJson valid),
      ("expected", toJson (Background.ProcessControl.signedScopeAccepted scope expected valid))])
  (toJson cases).compress

def stopActionName : Background.ProcessControl.StopAction → String
  | .cancelBeforeDispatch => "cancel_before_dispatch"
  | .stopProcess => "stop_process"
  | .reportTerminal => "report_terminal"

def processStopActionCasesJson : String :=
  (toJson (ToolExecution.ToolCallState.all.map fun state =>
    Json.mkObj [("state", toJson state.toDefraDB),
      ("expected", toJson (stopActionName (Background.ProcessControl.stopAction state)))])).compress

end Conformance.NativeRefinement
