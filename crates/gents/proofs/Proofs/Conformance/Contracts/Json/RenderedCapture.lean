import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

structure CaptureScenarioInput where
  nodeDid : Nat
  sessionId : Nat
  requestId : Nat
  turnIndex : Nat
  attempt : Nat
  /-- Opaque canonical-request identity. Equal values mean equal canonical JSON. -/
  request : Nat
  priorBinding : Option Nat
  deriving Lean.ToJson

structure CaptureScenarioOutput where
  captureOutcome : String
  captureDurable : Bool
  postStage : String
  sendPermitted : Bool
  providerRequestsObserved : Nat
  durableAfter : Option Nat
  finalStage : String
  deriving Lean.ToJson

example : renderedCaptureCases.any (·.captureOutcome == "rejected") := by native_decide

example : renderedCaptureCases.any (·.captureOutcome == "idempotent") := by native_decide

example : renderedCaptureCases.all (fun c =>
    c.captureDurable == (c.captureOutcome != "rejected") &&
      (if c.sendPermitted then
        c.durableAfter == some c.request && c.postStage == "durablyCaptured" &&
          c.finalStage == "sent" && c.providerRequestsObserved == 1
      else
        c.durableAfter != some c.request && c.postStage == "assembled" &&
          c.finalStage == "assembled" && c.providerRequestsObserved == 0)) := by
  native_decide

/-- One provider attempt through the owned loop whose capture sink is the real
durable sink, seeded with the scenario's prior binding for the same key. -/
@[conformance_family]
def renderedCapture : Family CaptureScenarioInput CaptureScenarioOutput where
  inputs := renderedCaptureCases.map fun c =>
    { nodeDid := c.nodeDid, sessionId := c.sessionId, requestId := c.requestId
      turnIndex := c.turnIndex, attempt := c.attempt, request := c.request
      priorBinding := c.priorBinding }
  model := fun i =>
    let sc : RenderedCapture.Scenario :=
      { key := { nodeDid := i.nodeDid, sessionId := i.sessionId, requestId := i.requestId
                 turnIndex := i.turnIndex, attempt := i.attempt }
        request := { value := i.request }
        priorBinding := i.priorBinding.map fun value => { value } }
    { captureOutcome := sc.outcome.toContract
      captureDurable := sc.outcome.durable
      postStage := sc.postStage.toContract
      sendPermitted := sc.sendPermitted
      providerRequestsObserved := sc.providerRequests
      durableAfter := sc.durableAfter.map (·.value)
      finalStage := sc.finalStage.toContract }

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
