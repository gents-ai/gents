import Proofs.Tools.PathNamespace
import Proofs.Conformance.Family

namespace PathNamespace

deriving instance Lean.ToJson for Component, Observed, Step

attribute [conformance_function] step

end PathNamespace
