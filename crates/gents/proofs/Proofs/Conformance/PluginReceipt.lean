import Lean
import Proofs.ToolPolicy.PluginReceipt

namespace Conformance.PluginReceipt
open Lean
open ToolPolicy.PluginReceipt

private def receiptJson (receipt : Receipt) : Json := Json.mkObj [
  ("evidence", toJson receipt.evidence),
  ("output", toJson receipt.output),
  ("verdict", toJson receipt.verdict.spelling)]

def casesJson : Json := toJson <|
  [Terminal.completed, .failed, .timedOut, .cancelled].flatMap fun terminal =>
  [Verdict.success, .failed, .executionError].flatMap fun verdict =>
  let receipt : Receipt := ⟨"evidence", if verdict = .success then some "output" else none, verdict⟩
  let candidate := published terminal receipt
  [false, true].flatMap fun rowTerminal =>
  [none, some candidate, some { candidate with evidence := "different" }].map fun existing =>
    Json.mkObj [
      ("terminal", toJson terminal.spelling),
      ("receipt", receiptJson receipt),
      ("published", receiptJson candidate),
      ("row_terminal", toJson rowTerminal),
      ("existing", match existing with
        | none => Json.null
        | some prior => receiptJson prior),
      ("decision", toJson (decide rowTerminal existing candidate).spelling)]

end Conformance.PluginReceipt
