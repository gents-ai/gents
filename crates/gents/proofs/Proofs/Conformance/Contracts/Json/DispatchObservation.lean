import Proofs.CanonicalOutput.Execution.DispatchObservationCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.RequestExecutionLease

namespace Conformance.DispatchObservationContracts

open CanonicalOutput.Execution.DispatchObservation
open CanonicalOutput.Execution.DispatchObservation.Cases
open Conformance.Contracts

def observationName : Observation → String
  | .rejected => "rejected"
  | .unacknowledged => "unacknowledged"
  | .replay => "replay"
  | .fresh => "fresh"

def inputJson (input : Input) : String :=
  "{\"acknowledged\":" ++ jsonOptionalBool (some input.acknowledged) ++
    ",\"policy_allows\":" ++ jsonOptionalBool (some input.policyAllows) ++ "}"

def resultJson (result : Result) : String :=
  "{\"observation\":" ++ jsonString (observationName result.observation) ++
    ",\"may_invoke\":" ++ jsonOptionalBool (some result.mayInvoke) ++
    ",\"running\":" ++ jsonOptionalBool (some result.running) ++
    ",\"in_flight\":" ++ jsonOptionalBool (some result.inFlight) ++ "}"

def failureClassJson : Option ToolExecution.FailureClass → String
  | none => "null"
  | some failure => jsonString failure.toDefraDB

def cases : List (String × List Input) :=
  [("lost_receipt_then_replay", lostThenReplay),
   ("won_dispatch_then_replay", wonThenReplay),
   ("rejected_then_won_dispatch", rejectedThenWon),
   ("policy_rejected_then_settled", policyRejected)]

def caseJson (entry : String × List Input) : String :=
  "{\"name\":" ++ jsonString entry.1 ++
    ",\"inputs\":" ++ jsonArray (entry.2.map inputJson) ++
    ",\"expected\":" ++ (match run entry.2 with
      | none => "null"
      | some results => jsonArray (results.map resultJson)) ++
    ",\"completion_probe_outcome\":" ++ jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName completionProbeOutcome) ++
    ",\"completion_probe_accepted\":" ++ jsonOptionalBool (completionProbe entry.2) ++
    ",\"parent_outcome\":" ++ jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName parentOutcome) ++
    ",\"expected_after_parent_failure\":" ++
    (match afterParentFailure entry.2 with
      | none => "null"
      | some (running, _, needsRecovery, messages) =>
          "{\"running\":" ++ jsonOptionalBool (some running) ++
          ",\"needs_recovery\":" ++ jsonOptionalBool (some needsRecovery) ++
          ",\"message_count\":" ++ toString messages ++ "}") ++
    ",\"expected_after_policy_settlement\":" ++
    (match afterPolicySettlement entry.2 with
      | none => "null"
      | some settlement =>
          "{\"failed\":" ++ jsonOptionalBool (some settlement.failed) ++
          ",\"running\":" ++ jsonOptionalBool (some settlement.running) ++
          ",\"started\":" ++ jsonOptionalBool (some settlement.started) ++
          ",\"failure_class\":" ++ failureClassJson settlement.failureClass ++
          ",\"completion_accepted\":" ++
            jsonOptionalBool (some settlement.completionAccepted) ++ "}") ++
    ",\"expected_after_parent_recovery\":" ++
    (match afterParentFailureRecovery entry.2 with
      | some (some recovery) =>
          "{\"state\":" ++ jsonString recovery.state.toDefraDB ++
          ",\"dispatchable\":" ++ jsonOptionalBool (some recovery.dispatchable) ++
          ",\"terminalized\":" ++ toString recovery.terminalized ++ "}"
      | _ => "null") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

/-- The `spawn_process` parent is accepted and wins its own dispatch; its
target's command policy then denies the target. -/
def spawnedTargetCasesJson : String :=
  jsonArray [
    "{\"name\":\"spawn_process_target_policy_denied\"" ++
    ",\"parent_tool\":" ++ jsonString "spawn_process" ++
    ",\"completion_probe_outcome\":" ++ jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName completionProbeOutcome) ++
    ",\"expected\":" ++ (match afterSpawnedTargetRejection with
      | none => "null"
      | some rejection =>
          "{\"failed\":" ++ jsonOptionalBool (some rejection.failed) ++
          ",\"started\":" ++ jsonOptionalBool (some rejection.started) ++
          ",\"failure_class\":" ++ failureClassJson rejection.failureClass ++
          ",\"spawned_admitted\":" ++ jsonOptionalBool (some rejection.spawnedAdmitted) ++
          ",\"completion_accepted\":" ++
            jsonOptionalBool (some rejection.completionAccepted) ++ "}") ++ "}"]

example : afterSpawnedTargetRejection.isSome = true := by native_decide

example : cases.all (fun entry => (run entry.2).isSome) = true := by native_decide

example : cases.all (fun entry => (completionProbe entry.2).isSome) = true := by native_decide

end Conformance.DispatchObservationContracts
