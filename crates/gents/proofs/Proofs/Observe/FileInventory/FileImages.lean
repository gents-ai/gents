import Proofs.Observe.FileInventory.Source

namespace FileInventory.FileImages

/-- The origin session scope of a physical tool's capture. Retained images stay
bound to it; native document ACP alone decides who may read them. -/
structure Scope where
  requester : String
  node : String
  session : String
  deriving DecidableEq, Repr

/-- An inventory copy selects the destination session; the physical tool and retained
versions keep their original session. Native ACP must permit each source read. This
mapping changes no document relationship and confers no read authority. -/
def readScope (selected source : Scope) (bindingMatches : Bool) : Option Scope :=
  if bindingMatches && selected.requester != "" && selected.node != "" &&
      selected.session != "" && source.session != "" &&
      selected.requester == source.requester && selected.node == source.node
  then some source else none

theorem copied_inventory_reads_original_scope (selected source : Scope)
    (allowed : readScope selected source true = some source) :
    (readScope selected source true).map (·.session) = some source.session := by
  simp [allowed]

end FileInventory.FileImages
