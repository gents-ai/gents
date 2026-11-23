import Proofs.Session.Request.CrossActorAuthorization
import Proofs.Conformance.Family

namespace Conformance.CrossActorAuthorization

open Lean Request.CrossActorAuthorization

structure SignedTargetsInput where
  signedSession : String
  signedAgentDocument : String
  session : String
  agentDocument : String
  deriving ToJson, BEq

private def signedTargetsInputs : List SignedTargetsInput :=
  [ ⟨"session-doc", "agent-doc", "session-doc", "agent-doc"⟩
  , ⟨"session-doc", "agent-doc", "other-session", "agent-doc"⟩
  , ⟨"session-doc", "agent-doc", "session-doc", "other-agent"⟩ ]

private def signedTargetsVerified (i : SignedTargetsInput) : Bool :=
  signedTargetsMatch i.session i.agentDocument
    { exampleIntent with session := i.signedSession, agentDocument := i.signedAgentDocument }

/-- A client-signed AgentRequest verifies only for the session and Agent
documents it signed. -/
@[conformance_family]
def requestSignedTargets : Family SignedTargetsInput Bool where
  inputs := signedTargetsInputs
  model := signedTargetsVerified

/-- A client-signed goal command verifies only for the session and Agent
documents it signed. -/
@[conformance_family]
def goalCommandSignedTargets : Family SignedTargetsInput Bool where
  inputs := signedTargetsInputs
  model := signedTargetsVerified

/-- The facts of one case, named as the served fixture builds them. `author`
signs the request (`user` or `collaborator`, whose update on the user's
session is a relation the user grants); `signatureValid` is whether the
retained payload is the one the live row carries; `claimedDocument` is
`retained` or a second `replayed` document with the same signed fields;
`liveAuthorPresent` is whether that row's admission decodes; `owner` is who
created the request; `namedSessionMatches` and `agentRow` (`selected`,
`other`, `disabled` or `absent`) are the targets the claim reads; the three
permissions are the signer's; `before` and `race` are the writes committed
before the claim and while its first attempt is open. -/
structure ClaimInput where
  name : String
  author : String
  signatureValid : Bool
  requestRead : String
  sessionRead : String
  claimedDocument : String
  liveAuthorPresent : Bool
  owner : Option String
  namedSessionMatches : Bool
  agentRow : String
  sessionUpdate : Bool
  agentInvoke : Bool
  requesterNodePermission : Bool
  transientFailure : Bool
  before : List String
  race : List String
  priorClaimed : Bool
  scheduled : Bool
  steer : Bool
  deriving ToJson

/-- `decision` is what the claim transaction decides over the facts as they
stand when the claim runs; the effect is the request's state after the claim,
which reruns over the state a race leaves. -/
structure ClaimOutput where
  decision : String
  lifecycle : String
  terminalOutput : Option String
  failureReason : Option String
  deriving ToJson

private def agentRowName (c : Case) : String :=
  match c.observation.agent with
  | none => "absent"
  | some agent =>
    if !agent.enabled then "disabled"
    else if agent.agent != c.intent.selectedAgent then "other" else "selected"

private def claimInput (c : Case) (scheduled steer : Bool) : ClaimInput :=
  let o := c.observation
  { name := c.name, author := c.intent.author,
    signatureValid := c.intent.content == o.live.content,
    requestRead := rowReadString o.request, sessionRead := rowReadString o.session,
    claimedDocument := if o.live.document == c.intent.document then "retained" else "replayed",
    liveAuthorPresent := o.live.author.isSome,
    owner := o.owner,
    namedSessionMatches := o.namedSession.all (fun named => named.document == c.intent.session),
    agentRow := agentRowName c,
    sessionUpdate := o.sessionUpdate, agentInvoke := o.agentInvoke,
    requesterNodePermission := o.requesterNodePermission,
    transientFailure := o.transientFailure,
    before := c.before.map Write.wire, race := c.race.map Write.wire,
    priorClaimed := c.prior.lifecycle == .claimed
    scheduled, steer }

private def claimOutput (c : Case) : ClaimOutput :=
  let effect := c.effect
  { decision := decisionString c.decision,
    lifecycle := effect.lifecycle.toDefraDB,
    terminalOutput := effect.terminalSelection.map terminalSelectionString,
    failureReason := effect.failureReason }

private def caseNamed (name : String) : Case :=
  (cases.find? fun c => c.name == name).getD { name := "" }

/-- A claimed request is also claimed scheduled and as a steering input, and a
refused one as a steering input. -/
private def deliveries : List (String × Bool × Bool) :=
  [ ("committed_claim", true, false), ("committed_claim", false, true)
  , ("invalid_signature", false, true), ("invoke_denied_at_check", false, true) ]

@[conformance_family]
def signedClientClaim : Family ClaimInput ClaimOutput where
  inputs := cases.map (fun c => claimInput c false false) ++
    deliveries.map fun (name, scheduled, steer) => claimInput (caseNamed name) scheduled steer
  model := fun i => claimOutput (caseNamed i.name)

example : (caseNamed "committed_claim").decision = .claimed := by native_decide

example : (caseNamed "wrong_session").decision = .denied := by native_decide

example : (caseNamed "node_revoked_before_commit").effect.lifecycle = .failed := by native_decide

end Conformance.CrossActorAuthorization
