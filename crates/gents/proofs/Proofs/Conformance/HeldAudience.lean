import Proofs.Store.Identity.HeldAudience
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation

namespace Identity.HeldAudience

deriving instance Lean.ToJson for Held, Change, Refusal, Answer

attribute [conformance_function] change resend refusalOf

end Identity.HeldAudience
