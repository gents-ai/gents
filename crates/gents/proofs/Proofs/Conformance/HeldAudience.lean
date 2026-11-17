import Proofs.Store.Identity.HeldAudience
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation

namespace Identity.HeldAudience

deriving instance Lean.ToJson for Held, Change, Refusal, Answer, Told

attribute [conformance_function] change resend refusalOf told

end Identity.HeldAudience
