import Proofs.ToolCalls.Background.ProcessControl
import Proofs.ToolCalls.PluginJob.Authority
import Proofs.ToolCalls.Recovery.Sweeps.PluginJob
import Proofs.ToolCalls.Recovery.Sweeps.SessionMessage
import Proofs.Session.ManagedExec.Ownership
import Proofs.Conformance.Family

namespace Background.ProcessControl
deriving instance Lean.ToJson for LiveProcessEvidence
end Background.ProcessControl

namespace Background.ProcessControl
deriving instance Lean.ToJson for TerminalIntent
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

attribute [conformance_function]
  Background.ProcessControl.retireStopReceipt
  Background.ProcessControl.settleLiveCancel
  Background.ProcessControl.maySettleTerminal
  PluginJob.consentedDynamic
  PluginJob.documentUxAllowed
  Recovery.pluginJobStepInvocationAllowed
  Recovery.sessionMessageSettlementReason
  Recovery.CausedTerminal.settledState
