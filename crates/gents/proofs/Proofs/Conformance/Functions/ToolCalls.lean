import Proofs.ToolCalls.Background.ProcessControl
import Proofs.ToolCalls.PluginJob.Authority
import Proofs.ToolCalls.Recovery.Sweeps.PluginJob
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

attribute [conformance_function]
  Background.ProcessControl.retireStopReceipt
  Background.ProcessControl.settleLiveCancel
  Background.ProcessControl.maySettleTerminal
  PluginJob.consentedDynamic
  PluginJob.documentUxAllowed
  Recovery.pluginJobStepInvocationAllowed
