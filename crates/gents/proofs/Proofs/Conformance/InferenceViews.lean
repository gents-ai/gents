import Proofs.Inference.Views
import Proofs.Conformance.Family

namespace InferenceViews

deriving instance Lean.ToJson for Exposure

attribute [conformance_function] exposure

end InferenceViews
