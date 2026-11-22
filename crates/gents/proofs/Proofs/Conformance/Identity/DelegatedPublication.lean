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
  claimRaces : List String
  deriving ToJson

def caseInput (c : Case) : CaseInput :=
  { name := c.name, invokeGrant := c.invokeGrant, nodeOwner := c.binding.nodeOwner
    parentAgent := c.binding.parentAgent, targetAgent := c.binding.targetAgent
    sessionDoc := c.binding.sessionDoc, agentDoc := c.binding.agentDoc
    requestDoc := c.binding.requestDoc, signedSession := c.binding.signedSession
    signedAgent := c.binding.signedAgent, signer := c.binding.signer
    actions := c.actions.map actionWire
    claimRaces := c.claimRaces.map Identity.DelegatedPublication.ClaimRace.wire }

private def caseNamed (name : String) : Case :=
  (cases.find? fun c => c.name == name).getD
    ⟨"", false, binding, [], []⟩

private def finalState (c : Case) : Identity.DelegatedPublication.Publication :=
  c.actions.foldl step (initial c.invokeGrant c.binding)

private def targetRead (c : Case) : Bool :=
  let s := finalState c
  Identity.Household.allowed 5 s.household s.binding.targetAgent "delivered_requests" s.binding.requestDoc "read"

private def claimOf (c : Case) : String :=
  Request.CrossActorAuthorization.decisionString
    (Identity.DelegatedPublication.claimRetrying (finalState c) c.claimRaces)

structure ReadinessOutput where
  targetRead : Bool
  requestPending : Bool
  claim : String
  deriving ToJson

/-- The served readiness fixture builds a visible request whose relation set
is still pending and then completes it through the creator's publication
owner, for a client-signed request and for an Agent that signs its own
request. -/
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

/-- The served recovery fixture publishes a parent-signed delegated request
whose session carries no creation receipt for it, so the creator's recovery
finds no exact receipt; for a foreign owner the node owner writes that signed
request and the parent asks the publication owner to complete it. -/
def recoveryNames : List String := ["wrong-receipt", "wrong-owner"]

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
completion attempt as the case's actions say. A recovery for a mismatched
binding runs after the configuring User replaces the child Agent document;
the parent Agent's attempt to detach the published session from its Agent runs
after the recovery. Production publication always re-verifies the retained receipt and native
owner first, so a publication attempt the caller never observed has no
production call beyond leaving the dropped acknowledgement unrecovered. -/
def creatorRecoveryNames : List String :=
  ["commit-unconfirmed", "foreign-recovery-actor", "mismatched-recovery-binding",
   "revoked-after-publication"]

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
mutation, replaces the planned starter with an unverifiable source session,
names the node owner as the planned author instead of the parent Agent, or
withdraws the parent's invoke on the child Agent before the commit; the atomic
`agent_new` transaction then publishes nothing. -/
def commitRefusalNames : List String :=
  ["native-commit-failed", "missing-starter", "wrong-signer", "invoke-denied"]

@[conformance_family]
def commitRefusal : Family CaseInput CommitRefusalOutput where
  inputs := (cases.filter fun c => c.name ∈ commitRefusalNames).map caseInput
  model := fun i =>
    let c := caseNamed i.name
    let s := finalState c
    { committed := s.committed, toolRunning := s.toolRunning, targetRead := targetRead c
      claim := claimOf c }

/-- The served delegated-finality fixture commits one complete publication,
then holds the child's claim at its native commit boundary while the race lands:
the configuring User revokes the parent's `session_writer`, or the parent
rewrites the pending markers complete or resets the request's. -/
def claimRaceNames : List String :=
  ["invoke-revoked-at-commit", "marker-commit-conflict", "marker-reset-at-commit"]

@[conformance_family]
def claimRace : Family CaseInput String where
  inputs := (cases.filter fun c => c.name ∈ claimRaceNames).map caseInput
  model := fun i => claimOf (caseNamed i.name)

/-- The served readiness fixture's same-Agent run holds the Agent's own claim at
its native commit boundary while the author rewrites both markers complete. -/
def sameAgentClaimRaceNames : List String := ["same-agent-marker-conflict"]

@[conformance_family]
def sameAgentClaimRace : Family CaseInput String where
  inputs := (cases.filter fun c => c.name ∈ sameAgentClaimRaceNames).map caseInput
  model := fun i => claimOf (caseNamed i.name)

/-- Five cases have no production form. The `agent_new` commit binds the
physical session and Agent documents and then signs in the same transaction,
so no signature names other documents (`wrong-physical-session`,
`wrong-physical-agent`); the creation receipt is issued in that transaction
after the opening it needs is staged, signed and starter-verified there
(`missing-creation-receipt`); each relation set is staged atomically with its
pending marker (`partial-grants`, `partial-then-recover`). The model refuses
the first three at commit, leaves a partial set unclaimable and completes it on
recovery. -/
theorem unpublishable_cases :
    ["wrong-physical-session", "wrong-physical-agent", "missing-creation-receipt",
      "partial-grants", "partial-then-recover"].map (fun name =>
        let c := caseNamed name
        ((finalState c).committed, claimOf c)) =
      [(false, "unavailable"), (false, "unavailable"), (false, "unavailable"),
        (true, "unavailable"), (true, "claimed")] := by
  native_decide

example : targetRead (caseNamed "visible-before-publication-complete") = true := by native_decide

example : claimOf (caseNamed "visible-before-publication-complete") = "unavailable" := by
  native_decide

example : claimOf (caseNamed "published") = "claimed" := by native_decide

end Conformance.DelegatedPublication
