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

structure ClaimInput where
  claimant : String
  owner : Option String
  requestedId : String
  requestedDoc : String
  read : Bool
  update : Bool
  invoke : Bool
  currentRevision : Nat
  deriving ToJson, BEq

def claimInputs : List ClaimInput := do
  let claimant ← ["agent", "other"]
  let owner ← [none, some "client", some "other"]
  let requestedId ← ["call", "other"]
  let requestedDoc ← ["doc", "other"]
  let read ← [false, true]
  let update ← [false, true]
  let invoke ← [false, true]
  let currentRevision ← [0, 1]
  pure { claimant, owner, requestedId, requestedDoc, read, update, invoke, currentRevision }

def claimAllowed (i : ClaimInput) : Bool :=
  Background.ProcessControl.agentMayClaim "agent" i.claimant
    ⟨"client", ⟨i.requestedId, i.requestedDoc⟩, 0⟩ i.owner ⟨"call", "doc"⟩
    i.read i.update i.invoke 0 i.currentRevision

private def servedClaim (read update invoke : Bool) : ClaimInput :=
  { claimant := "agent", owner := some "client", requestedId := "call", requestedDoc := "doc"
    read, update, invoke, currentRevision := 0 }

/-- The served claim scenario holds one client-owned intent for the agent's own
call at the revision it observed, and changes only the agent's native session
read, session update and agent invoke permissions between claims, in this
order. -/
def servedClaims : List ClaimInput :=
  [ servedClaim true false true, servedClaim true true false, servedClaim false true true
  , servedClaim true true true, servedClaim true false false ]

@[conformance_family]
def claimScenario : Family ClaimInput Bool where
  inputs := servedClaims
  model := claimAllowed

/-- model-production-gap: the served claim scenario builds only the agent's own
client-owned intent at its observed revision; no production test claims as
another agent, for another owner or invocation, or after a revision change. -/
@[conformance_family]
def claimUnreached : Family ClaimInput Bool where
  inputs := claimInputs.filter fun i => !servedClaims.contains i
  model := claimAllowed

example : claimAllowed { servedClaim true true true with currentRevision := 1 } = false := by
  decide

structure ClientProcessReceiptInput where
  intentDoc : String
  author : String
  agent : String
  call : String
  document : String
  signatureValid : Bool
  observedResult : String
  signedResult : String
  deriving ToJson

def controlResultOf (name : String) : Background.ProcessControl.ControlResult :=
  if name == "applied" then .applied else .unconfirmed

@[conformance_family]
def clientProcessReceipt : Family ClientProcessReceiptInput Bool where
  inputs := do
    let intentDoc ← ["intent", "other"]
    let author ← ["client", "other"]
    let agent ← ["agent", "other"]
    let call ← ["call", "other"]
    let document ← ["doc", "other"]
    let signatureValid ← [false, true]
    let observedResult ← ["applied", "unconfirmed"]
    let signedResult ← ["applied", "unconfirmed"]
    pure { intentDoc, author, agent, call, document, signatureValid, observedResult, signedResult }
  model := fun i =>
    Background.ProcessControl.receiptAccepted ⟨i.intentDoc, i.author, i.agent, ⟨i.call, i.document⟩⟩
        ⟨"intent", "client", "agent", ⟨"call", "doc"⟩⟩ i.signatureValid &&
      Background.ProcessControl.signedResultMatches (controlResultOf i.observedResult)
        (controlResultOf i.signedResult)

structure GrantInput where
  authority : Bool
  fenced : Bool
  deriving ToJson, BEq

def grantAllowed (i : GrantInput) : Bool :=
  Background.ProcessControl.mayPublishOutcome i.authority i.fenced

/-- The served claim scenario publishes inside the claim's fenced transaction,
first under current authority after a failed control mutation, then after the
agent's document update permission is revoked. -/
def servedPublications : List GrantInput := [⟨true, true⟩, ⟨false, true⟩]

@[conformance_family]
def grantScenario : Family GrantInput Bool where
  inputs := servedPublications
  model := grantAllowed

/-- model-production-gap: production publishes a control outcome only inside the
claim's fenced transaction, so no production path reaches an unfenced
publication. -/
@[conformance_family]
def grantUnreached : Family GrantInput Bool where
  inputs := ([false, true].flatMap fun authority => [false, true].map fun fenced =>
    ({ authority, fenced } : GrantInput)).filter fun i => !servedPublications.contains i
  model := grantAllowed

example : grantAllowed ⟨true, false⟩ = false := by decide

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
