import Proofs.Inference.EagerStreaming
import Proofs.Conformance.Family

namespace EagerStreaming

deriving instance Lean.ToJson for Provider

attribute [conformance_function] enabled

end EagerStreaming
