import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def renderedCaptureCaseJson (witness : RenderedCaptureCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"node_did\":" ++ toString witness.nodeDid ++ ","
    ++ "\"session_id\":" ++ toString witness.sessionId ++ ","
    ++ "\"request_id\":" ++ toString witness.requestId ++ ","
    ++ "\"turn_index\":" ++ toString witness.turnIndex ++ ","
    ++ "\"attempt\":" ++ toString witness.attempt ++ ","
    ++ "\"request\":" ++ toString witness.request ++ ","
    ++ "\"prior_binding\":" ++ jsonOptionalNat witness.priorBinding ++ ","
    ++ "\"capture_outcome\":" ++ jsonString witness.captureOutcome ++ ","
    ++ "\"capture_durable\":" ++ boolString witness.captureDurable ++ ","
    ++ "\"post_stage\":" ++ jsonString witness.postStage ++ ","
    ++ "\"send_permitted\":" ++ boolString witness.sendPermitted ++ ","
    ++ "\"provider_requests_observed\":"
      ++ toString witness.providerRequestsObserved ++ ","
    ++ "\"durable_after\":" ++ jsonOptionalNat witness.durableAfter ++ ","
    ++ "\"final_stage\":" ++ jsonString witness.finalStage
    ++ "}"

def renderedCaptureCasesJson : String :=
  jsonArray (renderedCaptureCases.map renderedCaptureCaseJson)

structure CaptureKeyPair where
  leftNodeDid : Nat
  leftSessionId : Nat
  leftRequestId : Nat
  leftTurnIndex : Nat
  leftAttempt : Nat
  rightNodeDid : Nat
  rightSessionId : Nat
  rightRequestId : Nat
  rightTurnIndex : Nat
  rightAttempt : Nat
  deriving Lean.ToJson

example :
    renderedCaptureKeyCases.all fun c =>
      let varied :=
        [ c.leftNodeDid != c.rightNodeDid, c.leftSessionId != c.rightSessionId
        , c.leftRequestId != c.rightRequestId, c.leftTurnIndex != c.rightTurnIndex
        , c.leftAttempt != c.rightAttempt ].count true
      varied ≤ 1 && (varied == 0) == c.sameFact := by
  native_decide

/-- Whether the production capture key names both provider attempts as one fact. -/
@[conformance_family]
def renderedCaptureKey : Family CaptureKeyPair Bool where
  inputs := renderedCaptureKeyCases.map fun c =>
    { leftNodeDid := c.leftNodeDid, leftSessionId := c.leftSessionId
      leftRequestId := c.leftRequestId, leftTurnIndex := c.leftTurnIndex
      leftAttempt := c.leftAttempt, rightNodeDid := c.rightNodeDid
      rightSessionId := c.rightSessionId, rightRequestId := c.rightRequestId
      rightTurnIndex := c.rightTurnIndex, rightAttempt := c.rightAttempt }
  model := fun i =>
    decide (({ nodeDid := i.leftNodeDid, sessionId := i.leftSessionId
               requestId := i.leftRequestId, turnIndex := i.leftTurnIndex
               attempt := i.leftAttempt } : RenderedCapture.CaptureKey) =
      { nodeDid := i.rightNodeDid, sessionId := i.rightSessionId
        requestId := i.rightRequestId, turnIndex := i.rightTurnIndex
        attempt := i.rightAttempt })

def captureScopeCaseJson (witness : CaptureScopeCase) : String :=
  "{"
    ++ "\"label\":" ++ jsonString witness.label ++ ","
    ++ "\"kind\":" ++ jsonString witness.kind ++ ","
    ++ "\"seq\":" ++ toString witness.seq ++ ","
    ++ "\"valid\":" ++ boolString witness.valid
    ++ "}"

def captureScopeCasesJson : String :=
  jsonArray (captureScopeCases.map captureScopeCaseJson)

structure CaptureOrderPair where
  leftLabel : String
  leftTurn : Nat
  leftAttempt : Nat
  rightLabel : String
  rightTurn : Nat
  rightAttempt : Nat
  deriving Lean.ToJson

/-- The scope kind and sequence a well-formed `"{kind}.{seq}"` label names. -/
def captureScopeOf (label : String) : Option (CaptureScopeKind × Nat) :=
  allCaptureScopeKinds.findSome? fun kind =>
    if label.startsWith (kind.label ++ ".") then
      (label.drop (kind.label.length + 1)).toNat?.map (kind, ·)
    else none

/-- Whether the left capture orders strictly before the right one by
`(kind rank, seq, turn, attempt)`. -/
def captureOrderBefore (pair : CaptureOrderPair) : Bool :=
  match captureScopeOf pair.leftLabel, captureScopeOf pair.rightLabel with
  | some (leftKind, leftSeq), some (rightKind, rightSeq) =>
      tupleLt (orderTuple leftKind leftSeq pair.leftTurn pair.leftAttempt)
        (orderTuple rightKind rightSeq pair.rightTurn pair.rightAttempt)
  | _, _ => false

def captureOrderPairs : List CaptureOrderPair :=
  captureOrderCases.map fun c =>
    { leftLabel := c.leftLabel, leftTurn := c.leftTurn, leftAttempt := c.leftAttempt
      rightLabel := c.rightLabel, rightTurn := c.rightTurn, rightAttempt := c.rightAttempt }

example :
    (captureOrderCases.zip captureOrderPairs).all fun (c, pair) =>
      captureOrderBefore pair == c.leftBeforeRight := by
  native_decide

@[conformance_family]
def captureOrder : Family CaptureOrderPair Bool where
  inputs := captureOrderPairs
  model := captureOrderBefore

end Conformance.Contracts
