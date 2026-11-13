import Proofs.Store.NativeCompletion
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.NativeCompletion
open Lean _root_.NativeCompletion

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

private def caseNamed (name : String) : Case :=
  (cases.find? (·.name == name)).getD ⟨name, false, false, []⟩

example : (cases.filter fun c => !c.transactional).all (fun c =>
    (initialObservedState c).nativeOutstanding == 0 && (initialObservedState c).gateHeld) = true :=
  by native_decide

example : (cases.filter (·.periodicRenewal)).all (fun c =>
    (initialObservedState c).cleanupRequired && (initialObservedState c).nativeOutstanding == 0 &&
      !(initialObservedState c).gateHeld &&
      (stateAfterEvents c).all (fun s => !s.gateHeld)) = true := by native_decide

example :
    let states := stateAfterEvents (caseNamed
      "registry_abandonment_retains_gate_until_all_native_returns")
    (states.all (fun s => !s.rollbackReturned)) = true := by native_decide

deriving instance ToJson for Event

structure TraceInput where
  name : String
  transactional : Bool
  periodicRenewal : Bool
  events : List Event
  deriving ToJson

def TraceInput.ofCase (c : Case) : TraceInput :=
  { name := c.name, transactional := c.transactional, periodicRenewal := c.periodicRenewal
    events := c.events }

def TraceInput.toCase (i : TraceInput) : Case :=
  ⟨i.name, i.transactional, i.periodicRenewal, i.events⟩

/-- The auto-commit traces whose native statement returns: a served node blocks
the signed write and releases it, so only traces that end the native work can
be driven. The gate is read after every event but the native return, which
the driver awaits by acquiring the drained gate. -/
@[conformance_family]
def autoCommitScenario : Family TraceInput (List Bool) where
  inputs := (cases.filter fun c => !c.transactional && c.events.contains .nativeReturned).map
    TraceInput.ofCase
  model := fun i =>
    (i.events.zip (stateAfterEvents i.toCase)).filterMap fun (event, state) =>
      if event = .nativeReturned then none else some state.gateHeld

structure RegistryView where
  nativeOutstanding : Nat
  gateHeld : Bool
  registryAbandoned : Bool
  deriving ToJson

structure RegistryOutput where
  checkpoints : List RegistryView
  retryStarted : Bool
  deriving ToJson

/-- The abandoned registry with two native statements: the served scenario
observes the registry after the abandonment's first gate release request,
after the first native return's gate release request and after the refused
retry. -/
@[conformance_family]
def registryScenario : Family TraceInput RegistryOutput where
  inputs := [TraceInput.ofCase (caseNamed
    "registry_abandonment_retains_gate_until_all_native_returns")]
  model := fun i =>
    let states := stateAfterEvents i.toCase
    { checkpoints := [4, 6, states.length - 1].map fun index =>
        let s := states.getD index (initialObservedState i.toCase)
        { nativeOutstanding := s.nativeOutstanding, gateHeld := s.gateHeld
          registryAbandoned := s.registryAbandoned }
      retryStarted := (states.getLast?.map (·.retryStarted)).getD false }

structure RenewalView where
  poisoned : Bool
  callerAbandonmentObserved : Bool
  nativeOutstanding : Nat
  renewalAttemptReturned : Bool
  nextRenewalStarted : Bool
  deriving ToJson

@[conformance_family]
def renewalScenario : Family TraceInput (List RenewalView) where
  inputs := (cases.filter (·.periodicRenewal)).map TraceInput.ofCase
  model := fun i => (stateAfterEvents i.toCase).map fun s =>
    { poisoned := s.poisoned, callerAbandonmentObserved := callerAbandonmentObserved s
      nativeOutstanding := s.nativeOutstanding, renewalAttemptReturned := s.renewalAttemptReturned
      nextRenewalStarted := s.nextRenewalStarted }

structure TraceView where
  callerAbandonmentObserved : Bool
  nativeOutstanding : Nat
  registryAbandoned : Bool
  rollbackReturned : Bool
  gateHeld : Bool
  retryStarted : Bool
  deriving ToJson

/-- model-production-gap: these transactional traces drive an explicit
rollback or a retry on a timed-out or cancelled transaction while native work
is outstanding; no served scenario blocks a transaction statement, observes
the rollback owner's return and then retries, so only the auto-commit,
registry-abandonment and renewal traces reach production. -/
@[conformance_family]
def traceUnreached : Family TraceInput (List TraceView) where
  inputs := (cases.filter fun c => c.transactional && !c.periodicRenewal &&
    c.name != "registry_abandonment_retains_gate_until_all_native_returns").map TraceInput.ofCase
  model := fun i => (stateAfterEvents i.toCase).map fun s =>
    { callerAbandonmentObserved := callerAbandonmentObserved s
      nativeOutstanding := s.nativeOutstanding, registryAbandoned := s.registryAbandoned
      rollbackReturned := s.rollbackReturned, gateHeld := s.gateHeld
      retryStarted := s.retryStarted }

end Conformance.NativeCompletion
