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
  (cases.map fun c =>
    { signedSession := c.intent.session, signedAgentDocument := c.intent.agentDocument
      session := c.checkedObservation.session
      agentDocument := c.checkedObservation.agentDocument }).eraseDups

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

structure ClaimInput where
  name : String
  intent : Json
  observation : Json
  signatureValid : Bool
  agentInvoke : Bool
  requesterNodePermission : Bool
  scheduled : Bool
  steer : Bool
  deriving ToJson

structure ClaimOutput where
  decision : String
  lifecycle : String
  terminalOutput : Option String
  failureReason : Option String
  deriving ToJson

private def caseNamed (name : String) : Case :=
  (cases.find? fun c => c.name == name).getD (testCase "" permittedObservation)

private def claimInput (c : Case) (scheduled steer : Bool) : ClaimInput :=
  { name := c.name, intent := intentJson c.intent, observation := observationJson c.checkedObservation
    signatureValid := c.checkedObservation.signatureValid
    agentInvoke := c.checkedObservation.agentInvoke
    requesterNodePermission := c.checkedObservation.requesterNodePermission
    scheduled, steer }

private def claimOutput (name : String) : ClaimOutput :=
  let c := caseNamed name
  let effect := applyClaim c.prior c.intent c.checkedObservation c.nativeCommit
  { decision := decisionString (decideClaim c.intent c.checkedObservation c.nativeCommit)
    lifecycle := effect.lifecycle.toDefraDB
    terminalOutput := effect.terminalSelection.map terminalSelectionString
    failureReason := effect.failureReason }

/-- The served signed-client claim fixture builds a valid or altered client
signature, an Agent the requester cannot invoke and a revoked requester node
permission, each as an interactive, scheduled or steering request. -/
def signedClaimCases : List (String × Bool × Bool) :=
  [ ("committed_claim", false, false), ("committed_claim", true, false)
  , ("committed_claim", false, true), ("invalid_signature", false, false)
  , ("invalid_signature", false, true), ("invoke_denied_at_check", false, false)
  , ("invoke_denied_at_check", false, true), ("node_denied_at_check", false, false) ]

@[conformance_family]
def signedClientClaim : Family ClaimInput ClaimOutput where
  inputs := signedClaimCases.map fun (name, scheduled, steer) => claimInput (caseNamed name) scheduled steer
  model := fun i => claimOutput i.name

/-- model-production-gap: the served claim fixture cannot present an
unsupported or disabled native check, a transient or hidden observation, a
foreign or missing native owner or authenticated author, a mismatched request
document, physical target or executor, or a permission change or commit
failure between the claim transaction's check and its commit. -/
@[conformance_family]
def unreached : Family ClaimInput ClaimOutput where
  inputs := (cases.filter fun c => !(signedClaimCases.any fun s => s.1 == c.name)).map
    fun c => claimInput c false false
  model := fun i => claimOutput i.name

example : (let c := caseNamed "committed_claim"
    decideClaim c.intent c.checkedObservation c.nativeCommit) = .claimed := by native_decide

example : (let c := caseNamed "node_denied_at_check"
    decideClaim c.intent c.checkedObservation c.nativeCommit) = .denied := by native_decide

end Conformance.CrossActorAuthorization
