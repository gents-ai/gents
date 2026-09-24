import Proofs.CompletionRetry.Executable
import Proofs.CompletionRetry.OutputObligation
import Proofs.Conformance.ContractTypes

namespace CompletionRetry.Contracts

open Conformance.Contracts

def failureClassVocabulary : List String :=
  ["transport", "parse_bad_request", "permanent"]

def failureClassName : FailureClass → String
  | .transport => "transport"
  | .parseBadRequest => "parse_bad_request"
  | .permanent => "permanent"

def failureOriginName : FailureOrigin → String
  | .localRequestBuild => "local_request_build"
  | .retryableTransport => "retryable_transport"

def phaseName : Phase → String
  | .issuing => "issuing"
  | .streaming => "streaming"
  | .retractRequired .. => "retract_required"
  | .retracted .. => "retracted"
  | .backingOff _ => "backing_off"
  | .repairing => "repairing"
  | .accepted _ => "accepted"
  | .acceptedToolFailed _ => "accepted_tool_failed"
  | .exhausted => "exhausted"
  | .failedPermanent => "failed_permanent"

def actionName : Action → String
  | .issue => "issue"
  | .observeFailure .. => "observe_failure"
  | .confirmRetraction _ => "confirm_retraction"
  | .schedule => "schedule"
  | .wake _ => "wake"
  | .accept _ => "accept_and_publish"
  | .acceptedToolFailure => "accepted_tool_failure"
  | .repairIssue => "repair_issue"
  | .recordUsage _ => "record_usage"

def boolJson (value : Bool) : String := if value then "true" else "false"

def jsonOptionalPhase : Option Phase → String
  | none => "null"
  | some phase => jsonString (phaseName phase)

def phaseScheduledWake : Phase → Option Time
  | .retractRequired _ _ wake | .retracted _ _ wake | .backingOff wake => some wake
  | _ => none

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

def RetryCase.toJson (c : RetryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"domain\":\"completionRetry\","
    ++ "\"action\":" ++ jsonString (actionName c.action) ++ ","
    ++ "\"failure_origin\":" ++
      (c.origin.map (jsonString ∘ failureOriginName)).getD "null" ++ ","
    ++ "\"classified_failure\":" ++
      (c.origin.map (jsonString ∘ failureClassName ∘ FailureOrigin.class)).getD "null" ++ ","
    ++ "\"legal\":" ++ boolJson c.post.isSome ++ ","
    ++ "\"pre_phase\":" ++ jsonString (phaseName c.pre.phase) ++ ","
    ++ "\"pre_now\":" ++ toString c.pre.now ++ ","
    ++ "\"pre_deadline\":" ++ jsonOptionalNat c.pre.deadline ++ ","
    ++ "\"pre_scheduled_wake\":" ++ jsonOptionalNat (phaseScheduledWake c.pre.phase) ++ ","
    ++ "\"expected_phase\":" ++ jsonOptionalPhase (c.post.map (·.phase)) ++ ","
    ++ "\"expected_transport_used\":" ++
      (c.post.map (fun state => toString state.transportUsed)).getD "null" ++ ","
    ++ "\"expected_resample_used\":" ++
      (c.post.map (fun state => toString state.resampleUsed)).getD "null" ++ ","
    ++ "\"expected_attempt\":" ++
      (c.post.map (fun state => toString state.attempt)).getD "null" ++ ","
    ++ "\"expected_repair_used\":" ++
      (c.post.map (fun state => boolJson state.repairUsed)).getD "null" ++ ","
    ++ "\"expected_usage_charged\":" ++
      (c.post.map (fun state => toString state.usageCharged)).getD "null"
    ++ "}"

def cases : List RetryCase := retryCases

def casesJson : String := jsonArray (cases.map RetryCase.toJson)

end CompletionRetry.Contracts
