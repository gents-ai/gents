import Proofs.Store.HomeResolution
import Proofs.Conformance.Family

namespace HomeResolution

deriving instance Lean.ToJson for Remembered, Resolution

attribute [conformance_function] resolve

end HomeResolution
