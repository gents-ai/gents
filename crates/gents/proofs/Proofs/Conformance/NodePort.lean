import Proofs.Clients.NodePort
import Proofs.Conformance.Family

namespace NodePort

deriving instance Lean.ToJson for Holder, Settlement

attribute [conformance_function] settle

end NodePort

namespace Conformance.NodePort

open Lean

structure CandidatesInput where
  taken : Nat
  deriving ToJson

/-- The ports tried after the default, the native-e2e default, a port near
the top of the range and the top itself. -/
@[conformance_family]
def candidatePorts : Family CandidatesInput (List Nat) where
  inputs := [⟨1⟩, ⟨9191⟩, ⟨21919⟩, ⟨65500⟩, ⟨65535⟩]
  model := fun i => _root_.NodePort.candidates i.taken

end Conformance.NodePort
