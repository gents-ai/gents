import Proofs.ConfigDocuments

namespace ApplyReconcile

abbrev Collection := ConfigDocuments.Collection

structure DocRef where
  collection : Collection
  id : String
  nodeDid : String
  deriving DecidableEq, Repr

end ApplyReconcile
