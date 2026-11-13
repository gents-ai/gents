import Proofs.ToolCalls.Background.ProcessControl
import Proofs.ToolCalls.PluginJob.Authority
import Proofs.ToolCalls.Recovery.Sweeps.PluginJob
import Proofs.ToolCalls.Recovery.Sweeps.SessionMessage
import Proofs.Session.ManagedExec.Ownership
import Proofs.ToolCalls.TaskHooks
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Family

namespace Background.ProcessControl
deriving instance Lean.ToJson for LiveProcessEvidence
end Background.ProcessControl

namespace Background.ProcessControl
deriving instance Lean.ToJson for TerminalIntent
deriving instance Lean.ToJson for StopAction
end Background.ProcessControl

namespace PluginJob
deriving instance Lean.ToJson for DynamicMode
end PluginJob

namespace ToolExecution
deriving instance Lean.ToJson for ToolCallState
end ToolExecution

namespace Recovery
deriving instance Lean.ToJson for CausedTerminal
deriving instance Lean.ToJson for AssignmentObservation
deriving instance Lean.ToJson for SettlementReason

/-- A reason's wire name is its text. -/
example : [SettlementReason.assignmentAbsent, .assignmentUnknown, .assignmentActive,
    .assignmentPaused, .assignmentBlocked, .assignmentUsageLimited, .assignmentBudgetLimited,
    .assignmentStopped, .requestFailed, .requestDead, .requestInterrupted,
    .requestSuperseded].all (fun reason => match Lean.toJson reason with
      | .str name => Conformance.wireName name == reason.toReason
      | _ => false) = true := by native_decide
end Recovery

namespace TaskHooks
deriving instance Lean.ToJson for TaskBinding, BindingRefusal, BoundRequest
end TaskHooks

attribute [conformance_function]
  TaskHooks.hooklessBoundRequest
  Background.ProcessControl.retireStopReceipt
  Background.ProcessControl.settleLiveCancel
  Background.ProcessControl.maySettleTerminal
  PluginJob.consentedDynamic
  PluginJob.documentUxAllowed
  Recovery.pluginJobStepInvocationAllowed
  Recovery.sessionMessageSettlementReason
  Recovery.CausedTerminal.settledState

namespace Background.ProcessControl
deriving instance Lean.ToJson for StopAction
end Background.ProcessControl

namespace Conformance.BackgroundStop

open Lean Background.ProcessControl

structure PendingObservation where
  durable : ToolExecution.ToolCallState
  deriving ToJson

structure StopObservation where
  action : StopAction
  rowState : ToolExecution.ToolCallState
  executionSignalled : Bool
  deriving ToJson

/-- The caller observed the row pending; by the time the stop runs the durable
row holds `durable`: still pending, or running because dispatch won the row.
A durable terminal row is not an input: there the native pre-dispatch
cancellation refuses with an error because the row's terminal source is
already closed, where this model reports the terminal state. -/
@[conformance_family]
def afterPendingObservation : Family PendingObservation StopObservation where
  inputs := [.pending, .running].map fun durable => { durable }
  model := fun i =>
    { action := stopAction i.durable
      rowState := stopSettledState i.durable
      executionSignalled := stopSignalsExecution (stopAction i.durable) }

end Conformance.BackgroundStop
