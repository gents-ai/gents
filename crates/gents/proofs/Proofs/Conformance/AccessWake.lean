import Proofs.Store.AccessWake
import Proofs.Conformance.Family
import Proofs.Conformance.DocumentAcpChoice

namespace AccessWake

deriving instance Lean.ToJson for Event, Delivery

attribute [conformance_function] listens delivery

end AccessWake
