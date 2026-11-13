import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Tools
import Proofs.Conformance.ContractCases
import Proofs.Session.ToolExecution
import Proofs.Tools.MCPHealth.Executable
import Proofs.Runtime.BackendHealth.Executable
import Proofs.Conformance.Functions.Runtime
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def nativeFilesystemBoundaryCaseJson
    (witness : NativeFilesystemBoundaryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"tool_name\":" ++ jsonString witness.toolName ++ ","
    ++ "\"work_class\":" ++ jsonString witness.workClass ++ ","
    ++ "\"boundary\":" ++ jsonString witness.boundary ++ ","
    ++ "\"inner_poll_blocks\":" ++ boolString witness.innerPollBlocks ++ ","
    ++ "\"request_deadline_ms\":" ++ toString witness.requestDeadlineMs ++ ","
    ++ "\"blocker_ms\":" ++ toString witness.blockerMs ++ ","
    ++ "\"expected_terminal\":" ++ jsonString witness.expectedTerminal ++ ","
    ++ "\"expected_failure_class\":"
      ++ jsonOptionalString witness.expectedFailureClass ++ ","
    ++ "\"queue_advances_before_blocker_returns\":"
      ++ boolString witness.queueAdvancesBeforeBlockerReturns
    ++ "}"

def managedExecToolBoundaryCaseJson
    (witness : ManagedExecToolBoundaryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"tool_name\":" ++ jsonString witness.toolName ++ ","
    ++ "\"work_class\":" ++ jsonString witness.workClass ++ ","
    ++ "\"boundary\":" ++ jsonString witness.boundary ++ ","
    ++ "\"kill_scope\":" ++ jsonString witness.killScope ++ ","
    ++ "\"timeout_requires_kill\":"
      ++ boolString witness.timeoutRequiresKill ++ ","
    ++ "\"cancel_requires_kill\":"
      ++ boolString witness.cancelRequiresKill ++ ","
    ++ "\"descendants_in_termination_scope\":"
      ++ boolString witness.descendantsInTerminationScope ++ ","
    ++ "\"capture_drain_bounded\":"
      ++ boolString witness.captureDrainBounded
    ++ "}"

def managedExecLivenessCaseJson
    (witness : ManagedExecLivenessCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"trigger\":" ++ jsonString witness.trigger ++ ","
    ++ "\"pre_exec_state\":" ++ jsonString witness.preExecState ++ ","
    ++ "\"pre_tool_state\":" ++ jsonString witness.preToolState ++ ","
    ++ "\"expected_exec_state\":"
      ++ jsonString witness.expectedExecState ++ ","
    ++ "\"expected_tool_state\":"
      ++ jsonString witness.expectedToolState ++ ","
    ++ "\"max_steps\":" ++ toString witness.maxSteps ++ ","
    ++ "\"kill_signal_required\":"
      ++ boolString witness.killSignalRequired
    ++ "}"

def processStopCaseJson (witness : ProcessStopCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"live_evidence\":" ++ jsonString witness.liveEvidence ++ ","
    ++ "\"worker_released\":" ++ boolString witness.workerReleased ++ ","
    ++ "\"settle_completed\":" ++ boolString witness.settleCompleted ++ ","
    ++ "\"settle_cancelled\":" ++ boolString witness.settleCancelled ++ ","
    ++ "\"settle_timed_out\":" ++ boolString witness.settleTimedOut ++ ","
    ++ "\"settle_failed\":" ++ boolString witness.settleFailed ++ ","
    ++ "\"settle_panicked\":" ++ boolString witness.settlePanicked ++ ","
    ++ "\"retire_before_commit\":" ++ boolString witness.retireBeforeCommit ++ ","
    ++ "\"retire_after_commit\":" ++ boolString witness.retireAfterCommit ++ ","
    ++ "\"preserve_timeout\":" ++ boolString witness.preserveTimeout ++ ","
    ++ "\"preserve_failure\":" ++ boolString witness.preserveFailure ++ ","
    ++ "\"pending_failure_beats_cancel\":" ++ boolString witness.pendingFailureBeatsCancel ++ ","
    ++ "\"pending_failure_resolves\":" ++ boolString witness.pendingFailureResolves ++ ","
    ++ "\"retained_completion_notified\":" ++ boolString witness.retainedCompletionNotified ++ ","
    ++ "\"retired_completion_notified\":" ++ boolString witness.retiredCompletionNotified ++ ","
    ++ "\"before\":" ++ jsonString witness.before ++ ","
    ++ "\"after\":" ++ jsonString witness.after ++ ","
    ++ "\"may_terminate\":" ++ boolString witness.mayTerminate ++ ","
    ++ "\"retain_record\":" ++ boolString witness.retainRecord ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"cancel_reply\":" ++ jsonString witness.cancelReply ++ ","
    ++ "\"durable_state\":" ++ jsonString witness.durableState
    ++ "}"

deriving instance Lean.ToJson for ToolExecution.SchemaStatus, ToolExecution.ToolOperation,
  ToolExecution.IdempotencyEvidence, ToolExecution.RetryDisposition

structure ToolPreflightInput where
  name : String
  health : ToolExecution.Health
  schemaStatus : ToolExecution.SchemaStatus
  deriving Lean.ToJson

structure ToolPreflightOutput where
  decision : String
  failureClass : Option String
  deriving Lean.ToJson

def toolPreflightOf (i : ToolPreflightInput) : ToolPreflightOutput :=
  let decision := ToolExecution.preflight i.health i.schemaStatus
  { decision := decision.toContract
  , failureClass := decision.failureClass.map ToolExecution.FailureClass.toDefraDB }

def toolPreflightInputs : List ToolPreflightInput :=
  ToolExecution.preflightCases.map fun c =>
    { name := c.name, health := c.health, schemaStatus := c.schema }

@[conformance_family]
def toolPreflight : Family ToolPreflightInput ToolPreflightOutput where
  inputs := toolPreflightInputs
  model := toolPreflightOf

/-- Discovery validates no arguments: only its unchecked-schema cases have a
discovery counterpart, where dispatch is the endpoint contact and discovery
reports no failure class. -/
@[conformance_family]
def toolPreflightDiscover : Family ToolPreflightInput String where
  inputs := toolPreflightInputs.filter fun i => i.schemaStatus == .unchecked
  model := fun i => (ToolExecution.preflight i.health i.schemaStatus).toContract

structure ToolRetryInput where
  name : String
  operation : ToolExecution.ToolOperation
  idempotency : ToolExecution.IdempotencyEvidence
  failureClass : String
  deriving Lean.ToJson

def ToolRetryInput.ofCase (c : ToolExecution.RetryCase) : ToolRetryInput :=
  { name := c.name, operation := c.operation, idempotency := c.idempotency
    failureClass := c.failure.toDefraDB }

def toolRetryDisposition (i : ToolRetryInput) : ToolExecution.RetryDisposition :=
  match ToolExecution.FailureClass.all.find? (·.toDefraDB == i.failureClass) with
  | some failure => ToolExecution.retryDisposition i.operation i.idempotency failure
  | none => .doNotRetry

def toolRetryReached (c : ToolExecution.RetryCase) (operation : ToolExecution.ToolOperation) :
    Bool :=
  c.operation == operation && c.idempotency == .unknown && c.failure == .transport

/-- The MCP pool's `list_tools` path: a transport failure without idempotency
evidence. -/
@[conformance_family]
def toolRetryListTools : Family ToolRetryInput ToolExecution.RetryDisposition where
  inputs := (ToolExecution.retryCases.filter (toolRetryReached · .mcpListTools)).map .ofCase
  model := toolRetryDisposition

/-- The MCP pool's `call_tool` path: a transport failure after dispatch without
idempotency metadata. -/
@[conformance_family]
def toolRetryCall : Family ToolRetryInput ToolExecution.RetryDisposition where
  inputs := (ToolExecution.retryCases.filter (toolRetryReached · .mcpCall)).map .ofCase
  model := toolRetryDisposition

/-- model-production-gap: production has no retry classifier; the MCP pool
retries a `list_tools` transport failure and never retries `call_tool`, and no
production path carries idempotency evidence or the other operations and
failure classes. -/
@[conformance_family]
def toolRetryUnreached : Family ToolRetryInput ToolExecution.RetryDisposition where
  inputs := (ToolExecution.retryCases.filter fun c =>
    !toolRetryReached c .mcpListTools && !toolRetryReached c .mcpCall).map .ofCase
  model := toolRetryDisposition

structure McpHealthInput where
  name : String
  startState : Proofs.MCPHealth.HealthState
  startCount : Nat
  event : String
  thresholdK : Nat
  deriving Lean.ToJson

structure McpHealthOutput where
  nextState : Option Proofs.MCPHealth.HealthState
  nextCount : Option Nat
  rustProjection : Option ToolExecution.Health
  deriving Lean.ToJson

def mcpHealthStep (i : McpHealthInput) : McpHealthOutput :=
  let next := if h : i.thresholdK ≥ 1 then
      (Proofs.MCPHealth.Event.all.find? (·.toDefraDB == i.event)).bind fun e =>
        Proofs.MCPHealth.step? { state := i.startState, failureCount := i.startCount } e
          (Proofs.MCPHealth.Threshold.ofNat i.thresholdK h)
    else none
  { nextState := next.map (·.state)
  , nextCount := next.map (·.failureCount)
  , rustProjection := next.map fun sm => Proofs.MCPHealth.healthProjection sm.state }

@[conformance_family]
def mcpHealthTransition : Family McpHealthInput McpHealthOutput where
  inputs := Proofs.MCPHealth.transitionCases.map fun c =>
    { name := c.name, startState := c.startState, startCount := c.startCount
      event := c.event.toDefraDB, thresholdK := c.thresholdK }
  model := mcpHealthStep

structure McpHealthViewInput where
  name : String
  nextState : Proofs.MCPHealth.HealthState
  nextCount : Nat
  thresholdK : Nat
  deriving Lean.ToJson

/-- The operator view projects only a surviving service model: a removed
service has no row. -/
@[conformance_family]
def mcpHealthView : Family McpHealthViewInput ToolExecution.Health where
  inputs := Proofs.MCPHealth.transitionCases.filterMap fun c =>
    match c.nextState, c.nextCount with
    | some s, some n => some { name := c.name, nextState := s, nextCount := n, thresholdK := c.thresholdK }
    | _, _ => none
  model := fun i => Proofs.MCPHealth.healthProjection i.nextState

example : Proofs.MCPHealth.transitionCases.all (fun c =>
    c.thresholdK ≥ 1 &&
    (c.nextState != some .evicted || c.event == .probeFail) &&
    c.nextState.isSome == c.nextCount.isSome &&
    c.rustProjection.isSome == c.nextState.isSome) = true := by
  native_decide

example :
    Proofs.MCPHealth.transitionCases.any (·.thresholdK == 3) = true ∧
    Proofs.MCPHealth.transitionCases.any (·.thresholdK ≥ 2) = true ∧
    Proofs.MCPHealth.transitionCases.any (fun c => c.thresholdK == 1 &&
      c.startState == .healthy && c.event == .probeFail && c.nextState == some .evicted) = true ∧
    [Proofs.MCPHealth.HealthState.healthy, .degraded, .evicted].all (fun s =>
      Proofs.MCPHealth.transitionCases.any (·.nextState == some s)) = true ∧
    Proofs.MCPHealth.transitionCases.any (fun c => c.nextState.isSome && c.thresholdK == 1) = true ∧
    Proofs.MCPHealth.transitionCases.any (fun c => c.nextState.isSome && c.thresholdK ≥ 2) = true := by
  native_decide

namespace BackendHealthTransition

open Lean

deriving instance ToJson for Proofs.BackendHealth.Event

structure Input where
  startState : Proofs.BackendHealth.HealthState
  startCount : Nat
  event : Proofs.BackendHealth.Event
  thresholdK : Nat
  deriving ToJson

structure Output where
  nextState : Proofs.BackendHealth.HealthState
  nextCount : Nat
  blocksRouting : Bool
  readinessReason : Option String
  deriving ToJson

/-- Production clamps a configured threshold below one to one; every modeled
input carries a threshold of at least one. -/
def threshold (k : Nat) : Proofs.BackendHealth.Threshold :=
  ⟨max k 1, Nat.le_max_right k 1⟩

example : [1, 2, 3].all fun k => Proofs.BackendHealth.transitionCases.any (·.thresholdK == k) := by native_decide

example : Proofs.BackendHealth.HealthState.all.all fun s => Proofs.BackendHealth.transitionCases.any (·.startState == s) := by native_decide

example :
    (Proofs.BackendHealth.step { state := .degraded, failureCount := 2 } .probeFail (threshold 3)).state = .unhealthy := by
  decide

end BackendHealthTransition

@[conformance_family]
def backendHealthTransition :
    Family BackendHealthTransition.Input BackendHealthTransition.Output where
  inputs := Proofs.BackendHealth.transitionCases.map fun c =>
    { startState := c.startState, startCount := c.startCount, event := c.event
      thresholdK := c.thresholdK }
  model := fun i =>
    let next := Proofs.BackendHealth.step
      { state := i.startState, failureCount := i.startCount } i.event
      (BackendHealthTransition.threshold i.thresholdK)
    { nextState := next.state
      nextCount := next.failureCount
      blocksRouting := next.state.blocksRouting
      readinessReason := next.state.readinessReason.map
        RuntimeState.AgentUnavailableReason.code }

def backendProbeConfigurationJson (config : Proofs.BackendHealth.ProbeConfiguration) : String :=
  "{\"backend_id\":" ++ toString config.backendId
    ++ ",\"revision\":" ++ toString config.revision
    ++ ",\"enabled\":" ++ boolString config.enabled ++ "}"

def backendProbeScheduleCaseJson (witness : Proofs.BackendHealth.ScheduleCase) : String :=
  "{\"name\":" ++ jsonString witness.name
    ++ ",\"known\":" ++ jsonArray (witness.known.map backendProbeConfigurationJson)
    ++ ",\"current\":" ++ jsonArray (witness.current.map backendProbeConfigurationJson)
    ++ ",\"periodic\":" ++ boolString witness.periodic
    ++ ",\"due\":" ++ jsonArray (witness.due.map toString)
    ++ ",\"observed\":" ++ jsonArray (witness.observed.map backendProbeConfigurationJson)
    ++ "}"

end Conformance.Contracts
