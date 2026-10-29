import Proofs.Conformance.Contracts.Json
import Proofs.ToolExecution.Parallel

namespace Conformance.Contracts

def contractJsonBegin : String :=
  "---BEGIN GENTS LEAN CONTRACT JSON---"

def contractJsonEnd : String :=
  "---END GENTS LEAN CONTRACT JSON---"

def main : IO Unit := do
  IO.println contractJsonBegin
  IO.println snapshotJson
  IO.println contractJsonEnd

end Conformance.Contracts

def main : IO Unit :=
  Conformance.Contracts.main
