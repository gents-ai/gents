import Proofs.ConfigDocuments

namespace ApplyReconcile

abbrev Collection := ConfigDocuments.Collection

structure DocRef where
  collection : Collection
  id : String
  /-- Logical config IDs are unique only within this principal. -/
  agentDid : String
  deriving DecidableEq, Repr

end ApplyReconcile
