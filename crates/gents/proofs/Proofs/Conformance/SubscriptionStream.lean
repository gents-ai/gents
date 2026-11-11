import Proofs.Store.SubscriptionStream
import Proofs.Conformance.Family

namespace SubscriptionStream

deriving instance Lean.ToJson for Arrival, Received, End, Step

attribute [conformance_function] step takeRenewal

end SubscriptionStream
