import Proofs.Store.Identity.HeldAudience
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation
import Proofs.Conformance.TransactionReplay

namespace Identity.HeldAudience

deriving instance Lean.ToJson for Held, Change, Refusal, Answer, Told, UnitStage

attribute [conformance_function] change resend refusalOf told resendUnit

end Identity.HeldAudience
