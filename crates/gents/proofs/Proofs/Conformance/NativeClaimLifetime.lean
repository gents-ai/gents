import Proofs.Store.NativeClaimLifetime
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.NativeClaimLifetime
open Lean _root_.NativeClaimLifetime Conformance.Contracts

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

def initialOf : Lane → State
  | .mutationTransaction => mutationTransaction
  | .autoCommitWrite => autoCommitWrite
  | .readOnlyTransaction => readOnlyTransaction
  | .periodicRenewal => periodicRenewal
  | .claimedRead => claimedRead
  | .claimedSchema => claimedSchema
  | .clientShutdown => clientShutdown

example : cases.all (fun c => initialOf c.initial.lane == c.initial) = true := by native_decide

def observations (lane : Lane) (events : List Event) : List State :=
  (events.scanl step (initialOf lane)).drop 1

def observedAt (lane : Lane) (events : List Event) (index : Nat) : State :=
  (observations lane events).getD index (initialOf lane)

private def caseNamed (name : String) : Case :=
  (cases.find? (·.name == name)).getD ⟨name, clientShutdown, []⟩

private def finalOf (c : Case) : State := run c.initial c.events

example : [ "claimed_read_timeout_retains_claim", "auto_commit_cancel_retains_claim"
    , "readonly_transaction_cleanup_retains_claim", "mutation_transaction_cleanup_retains_claim"
    , "schema_future_timeout_retains_claim_until_return" ].all (fun name =>
      !claimHeld (finalOf (caseNamed name)) && !(finalOf (caseNamed name)).mutationGateHeld) =
    true := by decide

example :
    let c := caseNamed "periodic_renewal_retains_claim_without_mutex"
    let renewal := observedAt c.initial.lane c.events 3
    let cleanup := observedAt c.initial.lane c.events 7
    (claimHeld renewal && renewal.transactionOwnerAlive && !claimHeld cleanup &&
      cleanup.rollbackReturned && !cleanup.cleanupPending &&
      !(observedAt c.initial.lane c.events 0).mutationGateHeld) = true := by native_decide

example :
    let c := caseNamed "committed_transaction_keeps_live_access_claim"
    (!claimHeld (finalOf c) && !(finalOf c).mutationGateHeld) = true := by native_decide

deriving instance ToJson for Lane, Event

structure TraceInput where
  name : String
  lane : Lane
  events : List Event
  /-- The positions in `events` after which the scenario observes production;
  the other events have no synchronous observation point in it. -/
  checkpoints : List Nat
  deriving ToJson

private def traceOf (name : String) (checkpoints : List Nat) : TraceInput :=
  let c := caseNamed name
  { name, lane := c.initial.lane, events := c.events, checkpoints }

structure StoreView where
  claimHeld : Bool
  mutationGateHeld : Bool
  deriving ToJson

/-- The served store scenarios: each drives one claimed-store owner through its
trace on a real embedded node and reads the store lock and the mutation gate. -/
@[conformance_family]
def storeScenario : Family TraceInput (List StoreView) where
  inputs :=
    [ traceOf "auto_commit_cancel_retains_claim" [0, 1, 2, 3]
    , traceOf "claimed_read_timeout_retains_claim" [0, 1, 2, 3]
    , traceOf "schema_future_timeout_retains_claim_until_return" [0, 1, 2, 3]
    , traceOf "readonly_transaction_cleanup_retains_claim" [0, 1, 2, 3, 5]
    , traceOf "mutation_transaction_cleanup_retains_claim" [0, 1, 2, 3, 5]
    , traceOf "periodic_renewal_retains_claim_without_mutex" [0, 1, 2, 3, 5]
    , traceOf "committed_transaction_keeps_live_access_claim" [2, 3] ]
  model := fun i => i.checkpoints.map fun index =>
    let s := observedAt i.lane i.events index
    { claimHeld := claimHeld s, mutationGateHeld := s.mutationGateHeld }

/-- The poisoned mutation transaction dropped from an OS thread: the rollback
cleanup is paused, so the native return is observable before cleanup ends. -/
@[conformance_family]
def threadDropScenario : Family TraceInput (List StoreView) where
  inputs := [traceOf "mutation_transaction_cleanup_retains_claim" [1, 3, 5, 6]]
  model := fun i => i.checkpoints.map fun index =>
    let s := observedAt i.lane i.events index
    { claimHeld := claimHeld s, mutationGateHeld := s.mutationGateHeld }

structure ExclusionView where
  claimHeld : Bool
  lifecycleGuardHeld : Bool
  deriving ToJson

/-- The desktop bridge's lifecycle exclusion with a cancelled waiter; it
observes the store lock (`exclusionStoreHeld`: the claim or the owner's node)
and the lifecycle guard. -/
@[conformance_family]
def clientExclusionScenario : Family TraceInput (List ExclusionView) where
  inputs := [traceOf "client_shutdown_cancel_retains_claim_until_return" [1, 3, 4]]
  model := fun i => i.checkpoints.map fun index =>
    let s := observedAt i.lane i.events index
    { claimHeld := exclusionStoreHeld s, lifecycleGuardHeld := s.lifecycleGuardHeld }

end Conformance.NativeClaimLifetime
