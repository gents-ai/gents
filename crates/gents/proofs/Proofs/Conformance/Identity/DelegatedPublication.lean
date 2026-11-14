import Proofs.Session.Identity.DelegatedPublicationCases
import Proofs.Conformance.Family

namespace Conformance.DelegatedPublication

open Lean
open Identity.DelegatedPublication.Cases

structure ActionWire where
  kind : String
  starter : Option Bool := none
  creationReceipt : Option Bool := none
  nativeCommit : Option Bool := none
  exact : Option Bool := none
  owner : Option Bool := none
  actor : Option String := none
  sessionParent : Option Bool := none
  sessionWorker : Option Bool := none
  requestParent : Option Bool := none
  requestSession : Option Bool := none
  requestWorker : Option Bool := none
  markSession : Option Bool := none
  markRequest : Option Bool := none
  deriving ToJson

def actionWire : Action → ActionWire
  | .commit starter receipt native =>
      { kind := "commit", starter := some starter, creationReceipt := some receipt,
        nativeCommit := some native }
  | .observe exact owner => { kind := "observe", exact := some exact, owner := some owner }
  | .publish actor exact g =>
      { kind := "publish", actor := some actor, exact := some exact,
        sessionParent := some g.sessionParent, sessionWorker := some g.sessionWorker,
        requestParent := some g.requestParent, requestSession := some g.requestSession,
        requestWorker := some g.requestWorker, markSession := some g.markSession,
        markRequest := some g.markRequest }
  | .revokeSessionParent => { kind := "revoke_session_parent" }

structure CaseInput where
  name : String
  invokeGrant : Bool
  nodeOwner : String
  parentAgent : String
  targetAgent : String
  sessionDoc : String
  agentDoc : String
  requestDoc : String
  signedSession : String
  signedAgent : String
  signer : String
  actions : List ActionWire
  claimSessionPermission : Bool
  claimAgentInvoke : Bool
  claimNativeCommit : Bool
  claimRequesterNodePermission : Bool
  deriving ToJson

def caseInput (c : Case) : CaseInput :=
  { name := c.name, invokeGrant := c.invokeGrant, nodeOwner := c.binding.nodeOwner
    parentAgent := c.binding.parentAgent, targetAgent := c.binding.targetAgent
    sessionDoc := c.binding.sessionDoc, agentDoc := c.binding.agentDoc
    requestDoc := c.binding.requestDoc, signedSession := c.binding.signedSession
    signedAgent := c.binding.signedAgent, signer := c.binding.signer
    actions := c.actions.map actionWire
    claimSessionPermission := c.claimCommit.sessionPermission
    claimAgentInvoke := c.claimCommit.agentInvoke
    claimNativeCommit := c.claimCommit.committed
    claimRequesterNodePermission := c.claimCommit.requesterNodePermission }

private def caseNamed (name : String) : Case :=
  (cases.find? fun c => c.name == name).getD
    ⟨"", false, binding, [], grantedCommit⟩

private def finalState (c : Case) : Identity.DelegatedPublication.Publication :=
  c.actions.foldl step (initial c.invokeGrant c.binding)

private def targetRead (c : Case) : Bool :=
  let s := finalState c
  Identity.Household.allowed 5 s.household s.binding.targetAgent "delivered_requests" s.binding.requestDoc "read"

private def claimOf (c : Case) : String :=
  Request.CrossActorAuthorization.decisionString
    (Identity.DelegatedPublication.claim (finalState c) c.claimCommit)

structure ReadinessOutput where
  targetRead : Bool
  requestPending : Bool
  claim : String
  deriving ToJson

/-- The served readiness fixture builds a visible request whose relation set
is still pending and then completes it through the creator's publication
owner, for a client-signed request and for an Agent that signs its own
request; it cannot inject a failed commit, a forged binding or a foreign
recovery actor. -/
def readinessNames : List String :=
  [ "visible-before-publication-complete", "published"
  , "same-agent-partial-visible", "same-agent-published" ]

@[conformance_family]
def claimReadiness : Family CaseInput ReadinessOutput where
  inputs := (cases.filter fun c => c.name ∈ readinessNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    { targetRead := targetRead c, requestPending := (finalState c).requestPending
      claim := claimOf c }

structure RecoveryOutput where
  observed : Bool
  requestPending : Bool
  targetRead : Bool
  deriving ToJson

/-- The served recovery fixture publishes a signed delegated request whose
session carries no creation receipt for it, so recovery finds no exact
receipt. -/
def recoveryNames : List String := ["wrong-receipt"]

@[conformance_family]
def recoveryReceipt : Family CaseInput RecoveryOutput where
  inputs := (cases.filter fun c => c.name ∈ recoveryNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    let s := finalState c
    { observed := s.observed, requestPending := s.requestPending, targetRead := targetRead c }

structure AgentNewOutput where
  creationReceipt : Bool
  targetRead : Bool
  deriving ToJson

/-- The served `agent_new` fixture commits one complete delegated publication
through the session-message owner. -/
def agentNewNames : List String := ["published"]

@[conformance_family]
def agentNewPublication : Family CaseInput AgentNewOutput where
  inputs := (cases.filter fun c => c.name ∈ agentNewNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    { creationReceipt := (finalState c).creationReceipt, targetRead := targetRead c }

structure CreatorRecoveryOutput where
  committed : Bool
  toolRunning : Bool
  sessionPending : Bool
  requestPending : Bool
  targetRead : Bool
  nodeOwnerRead : Bool
  claim : String
  deriving ToJson

/-- The served delegated-finality fixture drops the parent's committed
acknowledgement, then drives the creator's recovery watcher or a node-owner
completion attempt as the case's actions say.
Production publication always re-verifies the retained receipt and native
owner first, so a publication attempt the caller never observed has no
production call beyond leaving the dropped acknowledgement unrecovered. -/
def creatorRecoveryNames : List String :=
  ["commit-unconfirmed", "foreign-recovery-actor"]

@[conformance_family]
def creatorRecovery : Family CaseInput CreatorRecoveryOutput where
  inputs := (cases.filter fun c => c.name ∈ creatorRecoveryNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    let s := finalState c
    { committed := s.committed, toolRunning := s.toolRunning
      sessionPending := s.sessionPending, requestPending := s.requestPending
      targetRead := targetRead c
      nodeOwnerRead :=
        Identity.Household.allowed 5 s.household s.binding.nodeOwner "sessions" s.binding.sessionDoc "read"
      claim := claimOf c }

structure CommitRefusalOutput where
  committed : Bool
  toolRunning : Bool
  targetRead : Bool
  claim : String
  deriving ToJson

/-- The served delegated-finality fixture fails the commit transaction's first
mutation, replaces the planned starter with an unverifiable source session, or
names the node owner as the planned author instead of the parent Agent; the
atomic `agent_new` transaction then publishes nothing. -/
def commitRefusalNames : List String := ["native-commit-failed", "missing-starter", "wrong-signer"]

@[conformance_family]
def commitRefusal : Family CaseInput CommitRefusalOutput where
  inputs := (cases.filter fun c => c.name ∈ commitRefusalNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    let s := finalState c
    { committed := s.committed, toolRunning := s.toolRunning, targetRead := targetRead c
      claim := claimOf c }

structure Observation where
  committed : Bool
  observed : Bool
  toolRunning : Bool
  receipt : Bool
  starter : Bool
  creationReceipt : Bool
  sessionPending : Bool
  requestPending : Bool
  targetRead : Bool
  nodeOwnerRead : Bool
  nodeOwnerUpdate : Bool
  nodeOwnerAdmin : Bool
  otherUserRead : Bool
  otherUserAdmin : Bool
  claim : String
  deriving ToJson

private def observation (c : Case) : Observation :=
  let s := finalState c
  let b := s.binding
  { committed := s.committed, observed := s.observed, toolRunning := s.toolRunning
    receipt := s.receipt, starter := s.starterVerified, creationReceipt := s.creationReceipt
    sessionPending := s.sessionPending, requestPending := s.requestPending
    targetRead := targetRead c
    nodeOwnerRead := Identity.Household.allowed 5 s.household b.nodeOwner "sessions" b.sessionDoc "read"
    nodeOwnerUpdate := Identity.Household.allowed 5 s.household b.nodeOwner "sessions" b.sessionDoc "update"
    nodeOwnerAdmin := Identity.Household.allowed 5 s.household b.nodeOwner "sessions" b.sessionDoc "delete"
    otherUserRead := Identity.Household.allowed 5 s.household "other-user" "sessions" b.sessionDoc "read"
    otherUserAdmin := Identity.Household.allowed 5 s.household "other-user" "sessions" b.sessionDoc "delete"
    claim := claimOf c }

/-- model-production-gap: no production path reaches these cases.
`wrong-physical-session` and `wrong-physical-agent`: the `agent_new` commit
binds the physical session and Agent documents and then signs in the same
transaction, so no signature names other documents. `missing-creation-receipt`:
the receipt is issued in that transaction after the opening it needs is staged,
signed and starter-verified there. `wrong-owner`: the creator is the native
owner of the documents its transaction creates. `partial-grants` and
`partial-then-recover`: each relation set is staged atomically with its
pending marker. Disagreements: `invoke-denied` is refused at commit, not at
claim; `revoked-after-publication` fails because the parent Agent may not
delete its session's parent relation (NotAuthorized get-collection). Not built
yet: `invoke-revoked-at-commit`, `marker-commit-conflict` and
`same-agent-marker-conflict` need a concurrent change between the claim
transaction's check and its native commit; `mismatched-recovery-binding` needs
the child Agent document replaced between the commit and creator recovery. -/
@[conformance_family]
def unreached : Family CaseInput Observation where
  inputs := (cases.filter fun c =>
      !(c.name ∈ readinessNames || c.name ∈ recoveryNames || c.name ∈ agentNewNames ||
        c.name ∈ creatorRecoveryNames || c.name ∈ commitRefusalNames)).map caseInput
  model := fun i => observation (caseNamed i.name)

example : targetRead (caseNamed "visible-before-publication-complete") = true := by native_decide

example : claimOf (caseNamed "visible-before-publication-complete") = "unavailable" := by
  native_decide

example : claimOf (caseNamed "published") = "claimed" := by native_decide

end Conformance.DelegatedPublication
