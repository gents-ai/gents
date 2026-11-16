import Proofs.Clients.DesktopConnect
import Proofs.Conformance.Family

namespace Conformance.ConnectThisDesktop

open Lean DesktopConnect

deriving instance ToJson for NodeState
deriving instance ToJson for Step
deriving instance ToJson for Refusal

structure ConnectInput where
  node : NodeState
  identity : Bool
  routeSaved : Bool
  enrolled : Bool
  deriving ToJson

structure ConnectOutput where
  steps : List Step
  refusal : Option Refusal
  enrolled : Bool
  deriving ToJson

/-- Connect this desktop against a home `gents init` made, whose CLI User owns
the Node and answers within the window: each desktop state a served node can
be met in. -/
@[conformance_family]
def localNode : Family ConnectInput ConnectOutput where
  inputs := [
    { node := .notRunning, identity := false, routeSaved := false, enrolled := false },
    { node := .notRunning, identity := true, routeSaved := false, enrolled := false },
    { node := .serving, identity := false, routeSaved := false, enrolled := false },
    { node := .serving, identity := true, routeSaved := false, enrolled := false },
    { node := .serving, identity := true, routeSaved := true, enrolled := true }]
  model := fun i =>
    let o := connect ⟨i.node, i.identity, i.routeSaved, i.enrolled, true, .owner⟩
    { steps := o.steps, refusal := o.refusal, enrolled := o.enrolled }

end Conformance.ConnectThisDesktop
