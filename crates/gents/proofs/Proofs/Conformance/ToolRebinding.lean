import Proofs.Runtime.RuntimeReconcile.ToolRebinding
import Proofs.Conformance.Family

namespace Conformance.ToolRebinding

open Lean RuntimeReconcile.ToolRebinding

deriving instance ToJson for Publication, Answer

attribute [conformance_function] answer

end Conformance.ToolRebinding
