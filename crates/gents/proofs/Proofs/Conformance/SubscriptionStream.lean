import Proofs.Store.SubscriptionStream
import Proofs.Conformance.Family

namespace SubscriptionStream

deriving instance Lean.ToJson for Arrival, Received, End, Step, PermissionRead, EventGate

attribute [conformance_function] step takeRenewal gate

end SubscriptionStream
