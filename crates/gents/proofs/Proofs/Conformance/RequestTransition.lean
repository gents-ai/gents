import Proofs.Session.Request.Projection
import Proofs.Session.RequestExecutionLease.Transition
import Proofs.Conformance.ContractCases.LifecycleTransitions
import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Family

namespace Conformance.RequestTransition

open Lean
open Conformance.Contracts (requestStates requestActions requestSamples)
open Conformance.ContractCases (actionForPairFromSamples LifecycleTransitionClassification)

/-- The request action that takes `source` to `target`, read from the
request machine's sampled steps. -/
def action? (source target : RequestState) : Option String :=
  actionForPairFromSamples
    requestSamples
    requestActions
    RequestContext.step?
    (fun ctx => ctx.state.toDefraDB)
    source.toDefraDB
    target.toDefraDB

/-- One lease world per request state and lease shape: vacant, live (owner
generation `0`, deadline `10`, observed at `0`), expired (observed at `10`) and
dropped. -/
def leaseSamples : List (RequestExecutionLease.World Nat) :=
  requestStates.flatMap fun request =>
    ([(.vacant, 0), (.active 0 10 10, 0), (.active 0 10 10, 10), (.recoverable 0 10 10, 0)] :
        List (RequestExecutionLease.Lease Nat × Nat)).map fun (lease, now) =>
      { RequestExecutionLease.initial Nat with
        request := request, lease := lease, usedGenerations := [0], now := now }

def leaseOutcomes : List RequestExecutionLease.Outcome :=
  [.completed, .failed, .interrupted, .dead, .superseded]

/-- Every lease action, with owner generation `0`, fresh generation `1`, a
positive duration and an explicit deadline beyond every sample's clock. -/
def leaseActions : List (RequestExecutionLease.Action Nat) :=
  [ .claim .mutationWriteGate 1 10 20
  , .parkBackend .mutationWriteGate 0 true
  , .resumeBackend .mutationWriteGate 1 10 20 true
  , .reparkExpiredResume .mutationWriteGate 0 10
  , .begin .mutationWriteGate 0
  , .appendOutput .mutationWriteGate 0
  , .renew .mutationWriteGate 0 10
  , .authorizeProducerDecision .mutationWriteGate 0 .dispatch
  , .socketTraffic 0
  , .noOp 0
  , .advanceTime 10
  , .drop .mutationWriteGate 0
  , .recoverExpired .mutationWriteGate 0 1 10 20
  , .recoverDropped .mutationWriteGate 0 1 10 20
  , .recoverExpiredAndFail .mutationWriteGate 0 1
  , .recoverDroppedAndFail .mutationWriteGate 0 1 ]
  ++ leaseOutcomes.flatMap fun outcome =>
    [ .finalizeParked .mutationWriteGate 1 outcome
    , .finalize .mutationWriteGate 0 outcome
    , .policyRevoke .mutationWriteGate 0 1 outcome
    , .recoverExpiredTerminal .mutationWriteGate 0 1 outcome ]

/-- Whether an execution-lease authority changes the request from `source` to
`target`. A lease step that leaves the request state unchanged is not a
lifecycle change. -/
def leaseChanges (source target : RequestState) : Bool :=
  source != target && leaseSamples.any fun pre =>
    pre.request == source && leaseActions.any fun action =>
      match RequestExecutionLease.step? pre action with
      | some post => post.request == target
      | none => false

/-- A change no single request action takes is still reachable when an
execution-lease authority takes it: the expired-generation recovery sweep
(`dead`), latest-only trigger supersession of a running or parked request
(`superseded`, through `policyRevoke` or `finalizeParked`), and the re-park of
an expired resume (`reparkExpiredResume`). Production performs each through its
terminal or backend-wait owner, so the classification follows the lease model
rather than a separate table of sweep edges. -/
def classify (source target : RequestState) : LifecycleTransitionClassification :=
  match action? source target with
  | some _ => .legal
  | none => if leaseChanges source target then .recoveryReachable else .illegal

theorem latest_only_supersession_reaches_running_and_parked_requests :
    classify .claimed .superseded = .recoveryReachable ∧
    classify .processing .superseded = .recoveryReachable ∧
    classify .resumingBackend .superseded = .recoveryReachable ∧
    classify .waitingForBackend .superseded = .recoveryReachable := by
  native_decide

theorem expired_resume_returns_to_backend_wait :
    classify .resumingBackend .waitingForBackend = .recoveryReachable := by
  native_decide

theorem recovery_sweep_kills_expired_executions :
    classify .claimed .dead = .recoveryReachable ∧
    classify .processing .dead = .recoveryReachable := by
  native_decide

theorem terminal_requests_never_change :
    ([RequestState.completed, .failed, .superseded, .dead, .interrupted] : List RequestState).all
      (fun source => requestStates.all fun target => classify source target == .illegal)
      = true := by
  native_decide

def requestPairs : List (RequestState × RequestState) :=
  requestStates.flatMap fun source => requestStates.map fun target => (source, target)

example : requestPairs.Nodup := by native_decide

example : requestPairs.all (fun (source, target) =>
    (classify source target == .legal) == (action? source target).isSome) = true := by
  native_decide

structure TransitionInput where
  fromState : RequestState
  toState : RequestState
  deriving ToJson

structure TransitionOutput where
  action : Option String
  classification : String
  stoppedBeforeRunning : Bool
  stoppedBeforeRunningUnowned : Bool
  deriving ToJson

@[conformance_family]
def classification : Family TransitionInput TransitionOutput where
  inputs := requestPairs.map fun (source, target) => { fromState := source, toState := target }
  model := fun i =>
    { action := action? i.fromState i.toState
      classification := (classify i.fromState i.toState).toContract
      stoppedBeforeRunning := Request.Projection.stoppedBeforeRunning true i.fromState i.toState
      stoppedBeforeRunningUnowned :=
        Request.Projection.stoppedBeforeRunning false i.fromState i.toState }

end Conformance.RequestTransition
