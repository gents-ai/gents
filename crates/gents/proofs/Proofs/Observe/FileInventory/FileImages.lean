import Proofs.Observe.FileInventory.Source

namespace FileInventory.FileImages

/-- The origin session scope of a physical tool's capture. Retained images stay
bound to it; native document ACP alone decides who may read them. -/
structure Scope where
  requester : String
  node : String
  session : String
  deriving DecidableEq, Repr

end FileInventory.FileImages
