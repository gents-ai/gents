import Proofs.Tools.DocumentSigner
import Proofs.Conformance.Family

namespace DocumentSigner

deriving instance Lean.ToJson for Signer

/-- A document's writes, in order: a create, then any updates. -/
inductive History where
  | unsigned
  | byReader
  | byOther
  | readerThenReader
  | readerThenOther
  | otherThenReader
  | readerThenUnsigned
  deriving Repr, Lean.ToJson

def History.commits : History → List Commit
  | .unsigned => [.unsigned]
  | .byReader => [.reader]
  | .byOther => [.other]
  | .readerThenReader => [.reader, .reader]
  | .readerThenOther => [.reader, .other]
  | .otherThenReader => [.other, .reader]
  | .readerThenUnsigned => [.reader, .unsigned]

inductive Filter where
  | none
  | reader
  | other
  deriving Repr, Lean.ToJson

def Filter.signer : Filter → Option Signer
  | .none => Option.none
  | .reader => some .reader
  | .other => some .other

structure Read where
  returned : Bool
  signer : Option Signer
  deriving Lean.ToJson

@[conformance_function]
def read (history : History) (filter : Filter) : Read :=
  { returned := admits filter.signer history.commits
    signer := DocumentSigner.signer history.commits }

end DocumentSigner
