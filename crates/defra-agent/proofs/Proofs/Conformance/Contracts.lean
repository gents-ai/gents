import Proofs.Conformance.Contracts.Json

/-!
# Rust Conformance Contracts

This module is the Lean-owned extraction surface for Rust conformance tests.
Rust runs this file with `lake env lean --run` and consumes the JSON emitted by
`main`. State vocabularies, transition tables, classified Request/Process
transition cases, and finite witness rows are evaluated from the Lean
constructors, `toDefraDB` functions, and executable `step?` functions in the
imported submodules.
-/

namespace Conformance.Contracts

def contractJsonBegin : String :=
  "---BEGIN DEFRA LEAN CONTRACT JSON---"

def contractJsonEnd : String :=
  "---END DEFRA LEAN CONTRACT JSON---"

def main : IO Unit := do
  IO.println contractJsonBegin
  IO.println snapshotJson
  IO.println contractJsonEnd

end Conformance.Contracts

def main : IO Unit :=
  Conformance.Contracts.main
