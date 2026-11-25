import Proofs.Inference.Operations
import Proofs.Inference.InferenceCall.Persistence
import Proofs.Inference.ActivityViews
import Proofs.Inference.SetupRecommendation
import Proofs.Conformance.Family

namespace InferenceOperations

deriving instance Lean.ToJson for Actor, Signer, ActingDid, AuthClass, BackendDecision, CurrentBackend,
  ChoiceDecision, ProfileRoute, DiscoveryAnswer, AccountOperation

attribute [conformance_function]
  actorKind
  backendUpdateDecision
  backendCreateDecision
  profileWriteDecision
  discoveryAnswer
  accountOperationAdmitted

end InferenceOperations

attribute [conformance_function]
  InferenceCall.Persistence.mayStart
  InferenceCall.Persistence.mayInstallTerminal

namespace InferenceSetup

deriving instance Lean.ToJson for Provider

attribute [conformance_function] recommendedMaxConcurrent

end InferenceSetup
