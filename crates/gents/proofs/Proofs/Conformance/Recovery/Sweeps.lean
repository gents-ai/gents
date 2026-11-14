import Proofs.Runtime.Recovery.Sweeps
import Proofs.Conformance.ContractCases
import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Contracts.Machines.InferenceCall
import Proofs.Conformance.PluginJob
import Proofs.Conformance.Family

namespace Conformance.RecoverySweeps

open Lean Recovery ToolExecution Conformance.ContractCases

structure Swept where
  terminalState : String
  measureBefore : Nat
  measureAfter : Nat
  deriving ToJson

structure SweptRow where
  terminalState : String
  measureBefore : Nat
  measureAfter : Nat
  cancelCause : Option String
  toolFailureClass : Option String
  deriving ToJson

def SweptRow.swept (row : SweptRow) : Swept :=
  ⟨row.terminalState, row.measureBefore, row.measureAfter⟩

def sweptDecreasesOrDefers (preState : String) (swept : Swept) : Bool :=
  swept.measureAfter == 0 &&
    (decide (swept.measureBefore > swept.measureAfter) ||
      (swept.measureBefore == 0 && preState == swept.terminalState))

structure RequestInput where
  name : String
  preState : String
  interruptRequested : Bool
  deriving ToJson

def requestRow (state : RequestState) (interruptRequested : Bool) : RequestRecoveryRow :=
  { request := Conformance.Contracts.requestContext state .acquired
    leaseExpired := true
    interruptRequested := interruptRequested }

def requestRows : List (String × RequestRecoveryRow) :=
  [ ("request_claimed_recovery_to_failed", requestRow .claimed false)
  , ("request_processing_recovery_to_failed", requestRow .processing false)
  , ("request_processing_interrupted_recovery_to_interrupted", requestRow .processing true) ]

def requestOf (i : RequestInput) : RequestRecoveryRow :=
  requestRow ((RequestState.fromDefraDB? i.preState).getD .pending) i.interruptRequested

@[conformance_family]
def request : Family RequestInput Swept where
  inputs := requestRows.map fun (name, row) =>
    { name, preState := row.request.state.toDefraDB
      interruptRequested := row.interruptRequested }
  model := fun i =>
    let row := requestOf i
    { terminalState := (requestRecover row).request.state.toDefraDB
      measureBefore := requestRecoveryMeasure row
      measureAfter := requestRecoveryMeasure (requestRecover row) }

structure InferenceInput where
  name : String
  preState : String
  cause : String
  deriving ToJson

def inferenceCauses : List InferenceRecoveryCause :=
  [.staleQueued, .staleRunning, .interruptedParent]

def inferenceRows : List (String × InferenceCallRecoveryRow) :=
  [ ("inference_queued_stale_to_cancelled",
      ⟨Conformance.Contracts.inferenceCallWithState .queued, .staleQueued⟩)
  , ("inference_running_stale_to_failed",
      ⟨Conformance.Contracts.inferenceCallWithState .running, .staleRunning⟩)
  , ("inference_interrupted_parent_to_cancelled",
      ⟨Conformance.Contracts.inferenceCallWithState .running, .interruptedParent⟩) ]

def inferenceOf (i : InferenceInput) : InferenceCallRecoveryRow :=
  ⟨Conformance.Contracts.inferenceCallWithState
      ((InferenceCallState.fromDefraDB? i.preState).getD .running),
    (inferenceCauses.find? fun cause => cause.toContract == i.cause).getD .staleRunning⟩

@[conformance_family]
def inference : Family InferenceInput Swept where
  inputs := inferenceRows.map fun (name, row) =>
    { name, preState := row.call.state.toDefraDB, cause := row.cause.toContract }
  model := fun i =>
    let row := inferenceOf i
    { terminalState := (inferenceCallRecover row).call.state.toDefraDB
      measureBefore := inferenceCallRecoveryMeasure row
      measureAfter := inferenceCallRecoveryMeasure (inferenceCallRecover row) }

def toolCauses : List ToolRecoveryCause :=
  [ .deadlineExceeded, .parentInterrupted, .parentTerminal, .terminalizeBackgroundedAsInterrupted
  , .processLost, .taskDeleted, .sessionStopped, .causedRequestUnbound, .requestCompleted
  , .requestFailed, .requestDead, .requestInterrupted, .requestSuperseded ]

def toolCauseOf (cause : String) : Option ToolRecoveryCause :=
  toolCauses.find? fun candidate => candidate.toContract == cause

structure ToolCallInput where
  name : String
  preState : String
  awaitMode : String
  cause : String
  deriving ToJson

def toolCallRow (awaitMode : AwaitMode) (cause : ToolRecoveryCause) : ToolCallRecoveryRow :=
  { call := r6NativeToolFixture awaitMode, cause }

def toolCallRows : List (String × ToolCallRecoveryRow) :=
  [ ("tool_running_deadline_exceeded_to_timed_out", toolCallRow .foreground .deadlineExceeded)
  , ("tool_running_parent_interrupted_to_cancelled", toolCallRow .foreground .parentInterrupted)
  , ("tool_running_terminal_parent_to_failed", toolCallRow .foreground .parentTerminal)
  , ("tool_backgrounded_running_unowned_process_to_failed",
      toolCallRow .background .processLost) ]

def toolCallOf (i : ToolCallInput) : ToolCallRecoveryRow :=
  toolCallRow ((AwaitMode.fromDefraDB? i.awaitMode).getD .foreground)
    ((toolCauseOf i.cause).getD .deadlineExceeded)

@[conformance_family]
def toolCall : Family ToolCallInput SweptRow where
  inputs := toolCallRows.map fun (name, row) =>
    { name, preState := row.call.state.toDefraDB, awaitMode := row.call.awaitMode.toDefraDB
      cause := row.cause.toContract }
  model := fun i =>
    let row := toolCallOf i
    { terminalState := (toolCallRecover row).call.state.toDefraDB
      measureBefore := toolCallRecoveryMeasure .native row
      measureAfter := toolCallRecoveryMeasure .native (toolCallRecover row)
      cancelCause := sweptCancelCause (some row.cause)
      toolFailureClass := sweptFailureClass (some row.cause) }

structure TerminalParentInput where
  name : String
  preState : String
  parentState : String
  deriving ToJson

def terminalParentRow (parent : RequestState) : TerminalParentToolRow :=
  { call := r6NativeToolFixture .foreground
    parentTerminal := true
    parentInterrupted := parent == .interrupted }

def terminalParentRows : List (String × RequestState) :=
  [ ("live_running_native_tool_parent_interrupted_to_cancelled", .interrupted)
  , ("live_running_tool_parent_terminal_to_failed", .completed) ]

@[conformance_family]
def terminalParent : Family TerminalParentInput SweptRow where
  inputs := terminalParentRows.map fun (name, parent) =>
    { name, preState := (terminalParentRow parent).call.state.toDefraDB
      parentState := parent.toDefraDB }
  model := fun i =>
    let row := terminalParentRow ((RequestState.fromDefraDB? i.parentState).getD .completed)
    { terminalState := (terminalParentToolRecover row).call.state.toDefraDB
      measureBefore := terminalParentToolMeasure row
      measureAfter := terminalParentToolMeasure (terminalParentToolRecover row)
      cancelCause := sweptCancelCause (sweptCause row)
      toolFailureClass := sweptFailureClass (sweptCause row) }

structure OrphanInput where
  name : String
  preState : String
  deadlineExpired : Bool
  parentLive : Bool
  parentInterrupted : Bool
  parentTerminal : Bool
  executionRegistered : Bool
  processOutcome : String
  ownerTaskDeleted : Bool
  ownerSessionStopped : Bool
  deriving ToJson

structure OrphanCause where
  recoveryCause : Option String
  deriving ToJson

structure OrphanSettled where
  terminalState : String
  measureBefore : Nat
  measureAfter : Nat
  notificationReason : Option String
  cancelCause : Option String
  toolFailureClass : Option String
  deriving ToJson

def orphanRow
    (deadlineExpired parentLive parentInterrupted parentTerminal executionRegistered : Bool)
    (process : ManagedExec.StopOutcome := .stopped)
    (ownerTaskDeleted : Bool := false)
    (ownerSessionStopped : Bool := false) : OrphanedBackgroundToolRow :=
  { call := r6NativeToolFixture
    deadlineExpired, parentLive, parentInterrupted, parentTerminal, executionRegistered
    process, ownerTaskDeleted, ownerSessionStopped }

def orphanRows : List (String × OrphanedBackgroundToolRow) :=
  [ ("orphaned_background_tool_without_execution_to_cancelled",
      orphanRow false true false false false)
  , ("orphaned_background_tool_expired_missing_parent_deferred",
      orphanRow true false false false false)
  , ("orphaned_background_tool_expired_terminal_parent_to_timed_out",
      orphanRow true false false true false)
  , ("orphaned_background_tool_terminal_parent_to_cancelled",
      orphanRow false false false true false)
  , ("orphaned_background_tool_unowned_process_to_failed",
      orphanRow false true false false false (process := .notOwned))
  , ("orphaned_background_tool_exited_process_to_failed",
      orphanRow false false false true false (process := .alreadyExited))
  , ("orphaned_background_tool_still_running_deferred",
      orphanRow false true false false false (process := .stillRunning))
  , ("registered_background_tool_left_to_worker_deferred",
      orphanRow false true false false true)
  , ("registered_background_tool_task_deleted_to_cancelled",
      orphanRow false true false false true (ownerTaskDeleted := true))
  , ("registered_background_tool_session_stopped_to_cancelled",
      orphanRow false true false false true (ownerSessionStopped := true))
  , ("registered_background_tool_session_stop_still_running_deferred",
      orphanRow false true false false true (process := .stillRunning)
        (ownerSessionStopped := true))
  , ("orphaned_background_tool_session_stopped_to_cancelled",
      orphanRow false false false true false (ownerSessionStopped := true))
  , ("orphaned_background_tool_interrupted_parent_to_cancelled",
      orphanRow false false true false false) ]

def orphanInput : String × OrphanedBackgroundToolRow → OrphanInput
  | (name, row) =>
    { name, preState := row.call.state.toDefraDB, deadlineExpired := row.deadlineExpired
      parentLive := row.parentLive, parentInterrupted := row.parentInterrupted
      parentTerminal := row.parentTerminal, executionRegistered := row.executionRegistered
      processOutcome := row.process.toContract, ownerTaskDeleted := row.ownerTaskDeleted
      ownerSessionStopped := row.ownerSessionStopped }

def orphanOf (i : OrphanInput) : OrphanedBackgroundToolRow :=
  orphanRow i.deadlineExpired i.parentLive i.parentInterrupted i.parentTerminal
    i.executionRegistered
    ((ManagedExec.StopOutcome.all.find? fun o => o.toContract == i.processOutcome).getD .stopped)
    i.ownerTaskDeleted i.ownerSessionStopped

def orphanInputs : List OrphanInput := orphanRows.map orphanInput

def orphanCause (i : OrphanInput) : OrphanCause :=
  { recoveryCause := (orphanedBackgroundToolCause (orphanOf i)).map ToolRecoveryCause.toContract }

def orphanSettled (i : OrphanInput) : OrphanSettled :=
  let row := orphanOf i
  let recovered := orphanedBackgroundToolRecover row
  { terminalState := recovered.call.state.toDefraDB
    measureBefore := orphanedBackgroundToolMeasure row
    measureAfter := orphanedBackgroundToolMeasure recovered
    notificationReason :=
      (orphanedBackgroundToolCause row).bind ToolRecoveryCause.notificationReason
    cancelCause := sweptCancelCause (orphanedBackgroundToolCause row)
    toolFailureClass := sweptFailureClass (orphanedBackgroundToolCause row) }

/-- The orphan classifier decides every unregistered background row whose
parent resolves, including the still-running verdict no test host can
construct. -/
@[conformance_family]
def orphanClassified : Family OrphanInput OrphanCause where
  inputs := orphanInputs.filter fun i =>
    !i.executionRegistered && (orphanOf i).parentResolvable
  model := orphanCause

/-- The registered-worker session-stop classifier decides a registered row of
a stopped session from the process stop verdict alone. -/
@[conformance_family]
def orphanRegisteredSessionStop : Family OrphanInput OrphanCause where
  inputs := orphanInputs.filter fun i => i.executionRegistered && i.ownerSessionStopped
  model := orphanCause

/-- The orphan sweep over an accepted unregistered background row whose parent
resolves. A test host cannot hold a process that survives the stop request, so
the still-running verdict is decided by `orphanClassified` only. -/
@[conformance_family]
def orphanAccepted : Family OrphanInput OrphanSettled where
  inputs := orphanInputs.filter fun i =>
    !i.executionRegistered && (orphanOf i).parentResolvable &&
      i.processOutcome != ManagedExec.StopOutcome.stillRunning.toContract
  model := orphanSettled

@[conformance_family]
def orphanMissingParent : Family OrphanInput OrphanSettled where
  inputs := orphanInputs.filter fun i => !(orphanOf i).parentResolvable
  model := orphanSettled

/-- The orphan sweep over a row whose live worker is registered. A test host
cannot hold a process that survives the stop request, so the registered
still-running verdict is decided by `orphanRegisteredSessionStop` only. -/
@[conformance_family]
def orphanLiveWorker : Family OrphanInput OrphanSettled where
  inputs := orphanInputs.filter fun i =>
    i.executionRegistered && i.processOutcome != ManagedExec.StopOutcome.stillRunning.toContract
  model := orphanSettled

structure BackgroundCompletionInput where
  name : String
  preState : String
  parentResolvable : Bool
  sideEffectsDone : Bool
  deriving ToJson

def backgroundCompletionRow (state : ToolCallState) (parentResolvable sideEffectsDone : Bool) :
    BackgroundCompletionSideEffectRow :=
  { call := { r6NativeToolFixture with state }, parentResolvable, sideEffectsDone }

def backgroundCompletionRows : List (String × BackgroundCompletionSideEffectRow) :=
  [ ("terminal_background_tool_missing_completion_side_effects_to_converged",
      backgroundCompletionRow .failed true false) ]

@[conformance_family]
def backgroundCompletion : Family BackgroundCompletionInput Swept where
  inputs := backgroundCompletionRows.map fun (name, row) =>
    { name, preState := row.call.state.toDefraDB, parentResolvable := row.parentResolvable
      sideEffectsDone := row.sideEffectsDone }
  model := fun i =>
    let row := backgroundCompletionRow ((ToolCallState.fromDefraDB? i.preState).getD .failed)
      i.parentResolvable i.sideEffectsDone
    let recovered := backgroundCompletionSideEffectRecover row
    { terminalState := recovered.call.state.toDefraDB
      measureBefore := backgroundCompletionSideEffectMeasure row
      measureAfter := backgroundCompletionSideEffectMeasure recovered }

def assignmentName : AssignmentObservation → String
  | .absent => "absent"
  | .unknown => "unknown"
  | .active => "active"
  | .paused => "paused"
  | .blocked => "blocked"
  | .usageLimited => "usage_limited"
  | .budgetLimited => "budget_limited"
  | .complete => "complete"
  | .stopped => "stopped"

def assignments : List AssignmentObservation :=
  [.absent, .unknown, .active, .paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped]

def causedTerminals : List CausedTerminal := [.completed, .failed, .dead, .interrupted, .superseded]

structure SessionMessageInput where
  name : String
  preState : String
  recoveryCause : String
  assignment : String
  deriving ToJson

structure SessionMessageSettled where
  terminalState : String
  measureBefore : Nat
  measureAfter : Nat
  notificationReason : Option String
  deriving ToJson

def sessionMessageRow (cause : ToolRecoveryCause) : ToolCallRecoveryRow :=
  { call := { r6NativeToolFixture with operation := .sessionMessage }, cause }

def sessionMessageRows : List (String × ToolRecoveryCause × AssignmentObservation) :=
  [ ("session_message_caused_request_unbound_to_failed", .causedRequestUnbound, .complete)
  , ("session_message_request_completed_to_completed", .requestCompleted, .complete)
  , ("session_message_request_completed_no_goal_to_completed", .requestCompleted, .absent)
  , ("session_message_request_completed_unknown_goal_to_completed", .requestCompleted, .unknown)
  , ("session_message_request_completed_live_goal_to_completed", .requestCompleted, .active)
  , ("session_message_request_completed_blocked_goal_to_completed", .requestCompleted, .blocked)
  , ("session_message_request_completed_stopped_goal_to_completed", .requestCompleted, .stopped)
  , ("session_message_request_failed_to_failed", .requestFailed, .complete)
  , ("session_message_request_interrupted_to_cancelled", .requestInterrupted, .complete)
  , ("session_message_request_dead_to_failed", .requestDead, .complete)
  , ("session_message_request_superseded_to_failed", .requestSuperseded, .complete) ]

@[conformance_family]
def sessionMessage : Family SessionMessageInput SessionMessageSettled where
  inputs := sessionMessageRows.map fun (name, cause, assignment) =>
    { name, preState := (sessionMessageRow cause).call.state.toDefraDB
      recoveryCause := cause.toContract, assignment := assignmentName assignment }
  model := fun i =>
    let cause := (toolCauseOf i.recoveryCause).getD .causedRequestUnbound
    let row := sessionMessageRow cause
    let assignment := (assignments.find? fun a => assignmentName a == i.assignment).getD .complete
    { terminalState := (toolCallRecover row).call.state.toDefraDB
      measureBefore := toolCallRecoveryMeasure .sessionMessage row
      measureAfter := toolCallRecoveryMeasure .sessionMessage (toolCallRecover row)
      notificationReason :=
        match causedTerminals.find? fun t => t.recoveryCause == cause with
        | some terminal =>
            (sessionMessageSettlementReason terminal assignment).map SettlementReason.toReason
        | none => cause.notificationReason }

def observationName : TaskHookRequestObservation → String
  | .terminal => "terminal"
  | .active => "active"
  | .hidden => "hidden"
  | .unavailable => "unavailable"

def observations : List TaskHookRequestObservation := [.terminal, .active, .hidden, .unavailable]

structure TaskHookInput where
  name : String
  preState : String
  taskHookObservation : String
  executionRegistered : Bool
  deriving ToJson

def taskHookState (row : TaskHookRecordRow) : String :=
  if row.recorded then "running" else "released"

def taskHookRow (observation : TaskHookRequestObservation) (live : Bool) : TaskHookRecordRow :=
  { recorded := true, requestObservation := observation, liveExecution := live }

def taskHookRows : List (String × TaskHookRecordRow) :=
  [ ("task_hook_record_resolved_request_to_released", taskHookRow .terminal false)
  , ("task_hook_record_owned_request_deferred", taskHookRow .active false)
  , ("task_hook_record_live_execution_deferred", taskHookRow .terminal true)
  , ("task_hook_record_hidden_request_deferred", taskHookRow .hidden false)
  , ("task_hook_record_unavailable_identity_deferred", taskHookRow .unavailable false) ]

@[conformance_family]
def taskHook : Family TaskHookInput Swept where
  inputs := taskHookRows.map fun (name, row) =>
    { name, preState := taskHookState row
      taskHookObservation := observationName row.requestObservation
      executionRegistered := row.liveExecution }
  model := fun i =>
    let row := taskHookRow
      ((observations.find? fun o => observationName o == i.taskHookObservation).getD .unavailable)
      i.executionRegistered
    { terminalState := taskHookState (taskHookRecordRecover row)
      measureBefore := taskHookRecordMeasure row
      measureAfter := taskHookRecordMeasure (taskHookRecordRecover row) }

structure OwedWrite where
  subject : String
  doc : String
  deriving ToJson

structure OwedDeliveryInput where
  name : String
  preState : String
  authorized : Bool
  current : Bool
  attempt : String
  writes : List OwedWrite
  error : Option String
  refused : List OwedWrite
  deriving ToJson

def owedState (row : OwedDeliveryRow) : String :=
  match row.record.settled with
  | none => "running"
  | some .delivered => "delivered"
  | some .dropped => "dropped"

def owedRow (authorized current : Bool) (attempt : Hooks.OwedDelivery.Attempt)
    (refused : List Hooks.OwedDelivery.Write) : OwedDeliveryRow :=
  { record := Hooks.OwedDelivery.Record.fresh .staging (some "user") "agent"
      ⟨"control.session.halt", "goals", "s"⟩
    observed := ⟨authorized, current⟩
    attempt, refused }

def owedRows : List (String × OwedDeliveryRow) :=
  [ ("owed_delivery_applied_to_delivered", owedRow true true (.applied [⟨"s", "goal"⟩]) [])
  , ("owed_delivery_unowned_subject_to_dropped", owedRow false true (.applied [⟨"s", "goal"⟩]) [])
  , ("owed_delivery_superseded_to_dropped", owedRow true false (.applied [⟨"s", "goal"⟩]) [])
  , ("owed_delivery_failed_attempt_deferred", owedRow true true (.failed "refused") [])
  , ("owed_delivery_refused_write_deferred",
      owedRow true true (.applied [⟨"s", "goal"⟩]) [⟨"s", "goal"⟩]) ]

def owedWire (write : Hooks.OwedDelivery.Write) : OwedWrite :=
  { subject := write.subject, doc := write.doc }

def owedWriteOf (write : OwedWrite) : Hooks.OwedDelivery.Write := ⟨write.subject, write.doc⟩

def owedInput : String × OwedDeliveryRow → OwedDeliveryInput
  | (name, row) =>
    let (attempt, writes, error) := match row.attempt with
      | .applied writes => ("applied", writes.map owedWire, none)
      | .failed error => ("failed", [], some error)
    { name, preState := owedState row, authorized := row.observed.authorized
      current := row.observed.current, attempt, writes, error
      refused := row.refused.map owedWire }

def owedOf (i : OwedDeliveryInput) : OwedDeliveryRow :=
  owedRow i.authorized i.current
    (if i.attempt == "applied" then .applied (i.writes.map owedWriteOf) else .failed (i.error.getD ""))
    (i.refused.map owedWriteOf)

@[conformance_family]
def owedDelivery : Family OwedDeliveryInput Swept where
  inputs := owedRows.map owedInput
  model := fun i =>
    let row := owedOf i
    { terminalState := owedState (owedDeliveryRecover row)
      measureBefore := owedDeliveryMeasure row
      measureAfter := owedDeliveryMeasure (owedDeliveryRecover row) }

/-- The number of inputs each registered sweep's family replays. The orphan
sweep's rows are split across its five families; the plugin-job sweep's are
`Conformance.PluginJob.recoveries`. -/
def sweepFamilyInputs : List (String × Nat) :=
  [ (requestRecoverySweep.sweepId, request.inputs.length)
  , (toolCallRecoverySweep.sweepId, toolCall.inputs.length)
  , (orphanedBackgroundToolSweep.sweepId, orphanInputs.length)
  , (backgroundCompletionSideEffectSweep.sweepId, backgroundCompletion.inputs.length)
  , (terminalParentOwnedToolSweep.sweepId, terminalParent.inputs.length)
  , (sessionMessageRecoverySweep.sweepId, sessionMessage.inputs.length)
  , (inferenceCallRecoverySweep.sweepId, inference.inputs.length)
  , (taskHookRecoverySweep.sweepId, taskHook.inputs.length)
  , (pluginJobRecoverySweep.sweepId, Conformance.PluginJob.recoveries.inputs.length)
  , (owedDeliveryRecoverySweep.sweepId, owedDelivery.inputs.length) ]

/-- Every registered sweep is exercised by at least one family input, so a
replay of every family drives every sweep. -/
theorem sweep_families_cover_registered_sweeps :
    ∀ sweepId ∈ registeredRecoverySweepIds,
      ∃ entry ∈ sweepFamilyInputs, entry.1 = sweepId ∧ entry.2 > 0 := by
  native_decide

theorem orphan_families_cover_every_orphan_row :
    orphanInputs.all (fun i =>
      orphanClassified.inputs.any (·.name == i.name) || orphanRegisteredSessionStop.inputs.any (·.name == i.name) ||
        orphanMissingParent.inputs.any (·.name == i.name) || orphanLiveWorker.inputs.any (·.name == i.name)) =
      true := by
  native_decide

theorem sweeps_decrease_or_defer :
    request.inputs.all (fun i => sweptDecreasesOrDefers i.preState (request.model i)) &&
    inference.inputs.all (fun i => sweptDecreasesOrDefers i.preState (inference.model i)) &&
    toolCall.inputs.all (fun i => sweptDecreasesOrDefers i.preState (toolCall.model i).swept) &&
    terminalParent.inputs.all (fun i =>
      sweptDecreasesOrDefers i.preState (terminalParent.model i).swept) &&
    backgroundCompletion.inputs.all (fun i =>
      sweptDecreasesOrDefers i.preState (backgroundCompletion.model i)) &&
    taskHook.inputs.all (fun i => sweptDecreasesOrDefers i.preState (taskHook.model i)) &&
    owedDelivery.inputs.all (fun i => sweptDecreasesOrDefers i.preState (owedDelivery.model i)) &&
    orphanInputs.all (fun i =>
      let o := orphanSettled i
      sweptDecreasesOrDefers i.preState ⟨o.terminalState, o.measureBefore, o.measureAfter⟩) &&
    sessionMessage.inputs.all (fun i =>
      let o := sessionMessage.model i
      sweptDecreasesOrDefers i.preState ⟨o.terminalState, o.measureBefore, o.measureAfter⟩) =
      true := by
  native_decide

example :
    sessionMessage.inputs.map (fun i => (i.name, (sessionMessage.model i).notificationReason)) =
      [ ("session_message_caused_request_unbound_to_failed", some "caused_request_unbound")
      , ("session_message_request_completed_to_completed", none)
      , ("session_message_request_completed_no_goal_to_completed", some "assignment_absent")
      , ("session_message_request_completed_unknown_goal_to_completed", some "assignment_unknown")
      , ("session_message_request_completed_live_goal_to_completed", some "assignment_active")
      , ("session_message_request_completed_blocked_goal_to_completed", some "assignment_blocked")
      , ("session_message_request_completed_stopped_goal_to_completed", some "assignment_stopped")
      , ("session_message_request_failed_to_failed", some "request_failed")
      , ("session_message_request_interrupted_to_cancelled", some "request_interrupted")
      , ("session_message_request_dead_to_failed", some "request_dead")
      , ("session_message_request_superseded_to_failed", some "request_superseded") ] := by
  native_decide

/-- Every registered sweep is implemented, and a sweep is periodic exactly when
the periodic tick runs it, under the same Rust function. -/
example :
    registeredRecoverySweeps.all (fun sweep =>
      sweep.implementationStatus.toContract == "implemented" &&
        ((sweep.cadence.toContract == "periodic") ==
          (periodicRecoveryTickFunction sweep.sweepId).isSome) &&
        (periodicRecoveryTickFunction sweep.sweepId).all (· == sweep.rustFunction)) = true := by
  native_decide

end Conformance.RecoverySweeps
