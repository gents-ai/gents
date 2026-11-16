import Proofs.Documents.Operations
import Proofs.Conformance.Family

namespace DocumentOperations

deriving instance Lean.ToJson for Actor, Step, Digest, DeleteOutcome

attribute [conformance_function] deleteOutcome

end DocumentOperations
