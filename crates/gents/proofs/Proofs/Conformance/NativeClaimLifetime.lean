import Proofs.NativeClaimLifetime
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.NativeClaimLifetime
open _root_.NativeClaimLifetime Conformance.Contracts

structure Case where
  name : String
  initial : State
  events : List Event

def cases : List Case :=
  [ ⟨"claimed_read_timeout_retains_claim", claimedRead,
      [.nativeStarted, .callerTimedOut, .accessReleased, .gateReleaseRequested,
        .nativeReturned, .gateReleaseRequested]⟩
  , ⟨"auto_commit_cancel_retains_claim", autoCommitWrite,
      [.nativeStarted, .callerCancelled, .accessReleased, .gateReleaseRequested,
        .nativeReturned, .gateReleaseRequested]⟩
  , ⟨"readonly_transaction_cleanup_retains_claim", readOnlyTransaction,
      [.nativeStarted, .callerTimedOut, .transactionDropped, .accessReleased,
        .rollbackReturned, .gateReleaseRequested, .nativeReturned, .rollbackReturned,
        .gateReleaseRequested]⟩
  , ⟨"mutation_transaction_cleanup_retains_claim", mutationTransaction,
      [.nativeStarted, .callerTimedOut, .transactionDropped, .accessReleased,
        .rollbackReturned, .gateReleaseRequested, .nativeReturned, .rollbackReturned,
        .gateReleaseRequested]⟩
  , ⟨"periodic_renewal_retains_claim_without_mutex", periodicRenewal,
      [.nativeStarted, .callerTimedOut, .renewalAttemptReturned, .renewalRequested,
        .transactionDropped, .accessReleased, .nativeReturned, .rollbackReturned]⟩
  , ⟨"schema_future_timeout_retains_claim_until_return", claimedSchema,
      [.nativeStarted, .callerTimedOut, .accessReleased, .gateReleaseRequested,
        .nativeReturned, .gateReleaseRequested]⟩
  , ⟨"client_shutdown_cancel_retains_claim_until_return", clientShutdown,
      [.nativeStarted, .callerCancelled, .nativeReturned, .accessReleased,
        .lifecycleGuardReleased, .gateReleaseRequested]⟩
  , ⟨"committed_transaction_keeps_live_access_claim", readOnlyTransaction,
      [.nativeStarted, .nativeReturned, .transactionCommitted,
        .gateReleaseRequested, .accessReleased]⟩ ]

def laneName : Lane → String
  | .mutationTransaction => "mutation_transaction"
  | .autoCommitWrite => "auto_commit_write"
  | .readOnlyTransaction => "read_only_transaction"
  | .periodicRenewal => "periodic_renewal"
  | .claimedRead => "claimed_read"
  | .claimedSchema => "claimed_schema"
  | .clientShutdown => "client_shutdown"

def callerName : Caller → String
  | .waiting => "waiting"
  | .returned => "returned"
  | .timedOut => "timed_out"
  | .cancelled => "cancelled"

def eventName : Event → String
  | .nativeStarted => "native_started"
  | .nativeReturned => "native_returned"
  | .callerReturned => "caller_returned"
  | .callerTimedOut => "caller_timed_out"
  | .callerCancelled => "caller_cancelled"
  | .accessReleased => "access_released"
  | .transactionDropped => "transaction_dropped"
  | .transactionCommitted => "transaction_committed"
  | .rollbackReturned => "rollback_returned"
  | .retryRequested => "retry_requested"
  | .gateReleaseRequested => "gate_release_requested"
  | .lifecycleGuardReleased => "lifecycle_guard_released"
  | .renewalAttemptReturned => "renewal_attempt_returned"
  | .renewalRequested => "renewal_requested"

private def boolJson (value : Bool) : String := if value then "true" else "false"

def observationJson (state : State) : String :=
  "{\"lane\":" ++ jsonString (laneName state.lane) ++
  ",\"caller\":" ++ jsonString (callerName state.caller) ++
  ",\"claim_held\":" ++ boolJson (claimHeld state) ++
  ",\"claim_attached\":" ++ boolJson state.claimAttached ++
  ",\"access_owner_alive\":" ++ boolJson state.accessOwnerAlive ++
  ",\"transaction_owner_alive\":" ++ boolJson state.transactionOwnerAlive ++
  ",\"native_outstanding\":" ++ toString state.nativeOutstanding ++
  ",\"poisoned\":" ++ boolJson state.poisoned ++
  ",\"cleanup_pending\":" ++ boolJson state.cleanupPending ++
  ",\"rollback_returned\":" ++ boolJson state.rollbackReturned ++
  ",\"mutation_gate_held\":" ++ boolJson state.mutationGateHeld ++
  ",\"retry_started\":" ++ boolJson state.retryStarted ++
  ",\"renewal_attempt_returned\":" ++ boolJson state.renewalAttemptReturned ++
  ",\"next_renewal_started\":" ++ boolJson state.nextRenewalStarted ++
  ",\"lifecycle_guard_held\":" ++ boolJson state.lifecycleGuardHeld ++ "}"

def caseJson (c : Case) : String :=
  let observations := (c.events.scanl step c.initial).drop 1
  "{\"name\":" ++ jsonString c.name ++
  ",\"initial\":" ++ observationJson c.initial ++
  ",\"events\":" ++ jsonStringArray (c.events.map eventName) ++
  ",\"observations\":" ++ jsonArray (observations.map observationJson) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.NativeClaimLifetime

def main : IO Unit := IO.println Conformance.NativeClaimLifetime.casesJson
