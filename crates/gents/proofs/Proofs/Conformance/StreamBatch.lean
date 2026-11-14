import Proofs.Runtime.StreamBatch
import Proofs.Conformance.Family

namespace StreamBatch

deriving instance Lean.ToJson for Growth
deriving instance Lean.ToJson for BatchAge
deriving instance Lean.ToJson for Timer
deriving instance Lean.ToJson for Plan

attribute [conformance_function] plan

end StreamBatch
