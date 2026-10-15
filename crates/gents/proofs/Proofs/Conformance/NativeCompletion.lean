import Proofs.NativeCompletion
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.NativeCompletion
open _root_.NativeCompletion Conformance.Contracts

structure Case where
  name : String
  transactional : Bool
  periodicRenewal : Bool
  events : List Event

def cases : List Case :=
  [ ⟨"timeout_keeps_native_live_and_gate", true, false,
      [.nativeStarted, .callerTimedOut, .rollbackReturned, .gateReleaseRequested]⟩
  , ⟨"cancel_keeps_native_live_and_blocks_retry", true, false,
      [.nativeStarted, .callerCancelled, .retryRequested]⟩
  , ⟨"drain_then_rollback_then_release", true, false,
      [.nativeStarted, .callerTimedOut, .nativeReturned, .rollbackReturned,
        .gateReleaseRequested]⟩
  , ⟨"auto_commit_gate_waits_for_native_return", false, false,
      [.nativeStarted, .callerTimedOut, .gateReleaseRequested, .nativeReturned,
        .gateReleaseRequested]⟩
  , ⟨"auto_commit_cancel_waits_for_native_return", false, false,
      [.nativeStarted, .callerCancelled, .gateReleaseRequested, .nativeReturned,
        .gateReleaseRequested]⟩
  , ⟨"concurrent_operations_drain_individually", true, false,
      [.nativeStarted, .nativeStarted, .callerTimedOut, .nativeReturned,
        .rollbackReturned]⟩
  , ⟨"registry_abandonment_retains_gate_until_all_native_returns", true, false,
      [.nativeStarted, .nativeStarted, .callerTimedOut, .registryAbandoned,
        .gateReleaseRequested, .nativeReturned, .gateReleaseRequested,
        .nativeReturned, .gateReleaseRequested, .retryRequested]⟩
  , ⟨"renewal_waits_for_abandoned_native_work", true, true,
      [.nativeStarted, .callerTimedOut, .renewalAttemptReturned, .renewalRequested]⟩
  , ⟨"renewal_resumes_after_native_return", true, true,
      [.nativeStarted, .callerTimedOut, .nativeReturned, .renewalAttemptReturned,
        .renewalRequested]⟩ ]

def initialState (c : Case) : State :=
  { (if c.transactional then transaction else autoCommit) with
    periodicRenewal := c.periodicRenewal
    gateHeld := !c.periodicRenewal }

def initialObservedState (c : Case) : State :=
  { initialState c with nativeOutstanding := 0 }

def stateAfterEvents (c : Case) : List State :=
  (c.events.scanl step (initialObservedState c)).drop 1

def eventName : Event → String
  | .nativeStarted => "native_started"
  | .nativeReturned => "native_returned"
  | .callerReturned => "caller_returned"
  | .callerTimedOut => "caller_timed_out"
  | .callerCancelled => "caller_cancelled"
  | .registryAbandoned => "registry_abandoned"
  | .rollbackReturned => "rollback_returned"
  | .retryRequested => "retry_requested"
  | .gateReleaseRequested => "gate_release_requested"
  | .renewalAttemptReturned => "renewal_attempt_returned"
  | .renewalRequested => "renewal_requested"

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def callerName : Caller → String
  | .waiting => "waiting"
  | .returned => "returned"
  | .timedOut => "timed_out"
  | .cancelled => "cancelled"

def caseJson (c : Case) : String :=
  let initial := initialObservedState c
  "{\"name\":" ++ jsonString c.name ++
  ",\"periodic_renewal\":" ++ boolJson initial.periodicRenewal ++
  ",\"initial\":{\"cleanup_required\":" ++ boolJson initial.cleanupRequired ++
  ",\"native_outstanding\":" ++ toString initial.nativeOutstanding ++
  ",\"gate_held\":" ++ boolJson initial.gateHeld ++ "}" ++
  ",\"events\":" ++ jsonStringArray (c.events.map eventName) ++
  ",\"observations\":" ++ jsonArray ((stateAfterEvents c).map observationJson) ++ "}"

where
  observationJson (state : State) : String :=
    "{\"caller\":" ++ jsonString (callerName state.caller) ++
    ",\"native_outstanding\":" ++ toString state.nativeOutstanding ++
    ",\"poisoned\":" ++ boolJson state.poisoned ++
    ",\"registry_abandoned\":" ++ boolJson state.registryAbandoned ++
    ",\"rollback_returned\":" ++ boolJson state.rollbackReturned ++
    ",\"gate_held\":" ++ boolJson state.gateHeld ++
    ",\"retry_started\":" ++ boolJson state.retryStarted ++
    ",\"renewal_attempt_returned\":" ++ boolJson state.renewalAttemptReturned ++
    ",\"next_renewal_started\":" ++ boolJson state.nextRenewalStarted ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.NativeCompletion

def main : IO Unit := IO.println Conformance.NativeCompletion.casesJson
