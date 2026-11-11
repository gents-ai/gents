import Proofs.Pack.Operations
import Proofs.Conformance.Family

namespace Pack.Operations

deriving instance Lean.ToJson for
  Actor, Digest, DigestAdmission, Edited, EditedAdmission, VariableSource, RootReader, ReaderAction

attribute [conformance_function] digestAdmission editedAdmission variableSource readerAction

end Pack.Operations
