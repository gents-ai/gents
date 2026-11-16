import Proofs.Clients.ClientShell.ObservationScope
import Proofs.Conformance.Family

/-! `ClientWindowScope.delivers` over the bound TLA `WindowIndependence`
checks: a registered window selecting one of two nodes or none. -/
namespace ClientWindowScope.Bound

inductive Node where
  | n1 | n2
  deriving Lean.ToJson

/-- A registered window's selection; `none` selects no node. -/
inductive Selection where
  | none | n1 | n2
  deriving Lean.ToJson

def Node.index : Node → Nat
  | .n1 => 0
  | .n2 => 1

def Selection.node : Selection → Option Node
  | .none => Option.none
  | .n1 => some .n1
  | .n2 => some .n2

/-- Whether a delivery scoped to `node` reaches a registered window that
selects `selected`. -/
def delivers (selected : Selection) (node : Node) : Bool :=
  ClientWindowScope.delivers (fun _ => some (selected.node.map Node.index)) 0 node.index

attribute [conformance_function] delivers

end ClientWindowScope.Bound
