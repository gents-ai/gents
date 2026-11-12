import Proofs.ToolCalls.ToolDispatch
import Proofs.Conformance.Family

namespace ToolDispatch

deriving instance Lean.ToJson for Settlement

attribute [conformance_function] settleReady

end ToolDispatch
