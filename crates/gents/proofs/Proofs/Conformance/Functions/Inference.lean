import Proofs.Inference.Operations
import Proofs.Conformance.Family

namespace InferenceOperations

deriving instance Lean.ToJson for Actor, AuthClass, BackendDecision, CurrentBackend,
  ChoiceDecision, ProfileRoute, DiscoveryAnswer, AccountOperation

attribute [conformance_function]
  backendUpdateDecision
  backendCreateDecision
  profileWriteDecision
  discoveryAnswer
  accountOperationAdmitted

end InferenceOperations
