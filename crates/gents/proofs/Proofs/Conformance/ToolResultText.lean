import Proofs.Shared.ToolResultText
import Proofs.Conformance.Family

namespace ToolResultText

deriving instance Lean.ToJson for Parts, Wire

attribute [conformance_function] wire

end ToolResultText
