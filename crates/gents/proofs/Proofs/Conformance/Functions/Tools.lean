import Proofs.Tools.Identity.Configuration
import Proofs.Tools.Lsp.Types
import Proofs.Tools.MCPHealth.Coupling
import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Tools.ToolPolicy.PluginCalls
import Proofs.Conformance.Family

namespace Lsp
deriving instance Lean.ToJson for LspAction
end Lsp

namespace Proofs.MCPHealth
deriving instance Lean.ToJson for HealthState
end Proofs.MCPHealth

namespace ToolExecution
deriving instance Lean.ToJson for Health
end ToolExecution

attribute [conformance_function]
  Identity.Configuration.ordinaryMutationAllowed
  Identity.Configuration.clientSignedWriteMayRetry
  Lsp.LspAction.mutates
  ToolPolicy.OperatorPaths.commandGuardAllows
  ToolPolicy.PluginCalls.available
  Proofs.MCPHealth.healthProjection
