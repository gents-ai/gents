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

def cases : List (String × List Input) :=
  [("lost_receipt_then_replay", lostThenReplay),
   ("won_dispatch_then_replay", wonThenReplay),
   ("rejected_then_won_dispatch", rejectedThenWon)]

def caseJson (entry : String × List Input) : String :=
  "{\"name\":" ++ jsonString entry.1 ++
    ",\"inputs\":" ++ jsonArray (entry.2.map inputJson) ++
    ",\"expected\":" ++ (match run entry.2 with
      | none => "null"
      | some results => jsonArray (results.map resultJson)) ++
    ",\"parent_outcome\":" ++ jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName parentOutcome) ++
    ",\"expected_after_parent_failure\":" ++
    (match afterParentFailure entry.2 with
      | none => "null"
      | some (running, inFlight, needsRecovery, messages) =>
          "{\"running\":" ++ jsonOptionalBool (some running) ++
          ",\"in_flight\":" ++ jsonOptionalBool (some inFlight) ++
          ",\"needs_recovery\":" ++ jsonOptionalBool (some needsRecovery) ++
          ",\"message_count\":" ++ toString messages ++ "}") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.all (fun entry => (run entry.2).isSome) = true := by native_decide

end Conformance.DispatchObservationContracts
