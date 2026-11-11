import Proofs.Shared.AccountRotation.Policy
import Proofs.Conformance.Family

namespace AccountRotation

deriving instance Lean.ToJson for Family, Claim, Overage

attribute [conformance_function] Claim.binds Overage.serves

end AccountRotation
