import Proofs.Tools.Identity.Configuration
import Proofs.Tools.Lsp.Types
import Proofs.Tools.MCPHealth.Coupling
import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Tools.ToolPolicy.PluginCalls
import Proofs.Tools.SelfConfig.ToolsSave
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Documents

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

namespace DocumentOperations

deriving instance Lean.ToJson for FieldUpdate, FieldAfter, WriteMode, WriteOutcome,
  OwnerCount, OwnerResolution, ImportOutcome, EndpointScheme, EndpointHost, EndpointStorage,
  Toggle, StoredToggle

attribute [conformance_function] fieldAfter writeOutcome ownerResolution importOutcome
  endpointStorage defaultsPath enabledAfterRegister sendNodeDidAfterRegister

end DocumentOperations

namespace SelfConfig.ToolsSave

deriving instance Lean.ToJson for Reach, SharedOutcome, Outcome, SettingChange

attribute [conformance_function] holdsOperatorGrant ownControlBinds sharedOutcome outcome
  dropRefused

end SelfConfig.ToolsSave
