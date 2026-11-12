import Lean
import Proofs.Tools.ToolPolicy.PluginReceipt
import Proofs.Conformance.Family

namespace Conformance.PluginReceipt
open Lean
open ToolPolicy.PluginReceipt

attribute [conformance_vocabulary] Terminal.spelling

deriving instance ToJson for Verdict, Receipt, Terminal, Decision

structure CommitInput where
  terminal : Terminal
  receipt : Receipt
  rowTerminal : Bool
  existing : Option Receipt
  deriving ToJson

structure CommitOutput where
  published : Receipt
  decision : Decision
  deriving ToJson

@[conformance_family]
def commits : Family CommitInput CommitOutput where
  inputs := [Terminal.completed, .failed, .timedOut, .cancelled].flatMap fun terminal =>
    [Verdict.success, .failed, .executionError].flatMap fun verdict =>
    let receipt : Receipt := ⟨"evidence", if verdict = .success then some "output" else none, verdict⟩
    let candidate := published terminal receipt
    [false, true].flatMap fun rowTerminal =>
    [none, some candidate, some { candidate with evidence := "different" }].map fun existing =>
      { terminal, receipt, rowTerminal, existing }
  model := fun i =>
    let candidate := published i.terminal i.receipt
    { published := candidate, decision := decide i.rowTerminal i.existing candidate }

end Conformance.PluginReceipt
