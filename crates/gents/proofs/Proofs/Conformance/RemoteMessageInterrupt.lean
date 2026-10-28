import Proofs.Session.Management
import Lean

open Lean SessionQueue

def main : IO Unit := do
  let values : List (Option String) := [none, some "old", some "new"]
  let cases := values.flatMap fun before => values.map fun after =>
    Json.mkObj [("before", toJson before), ("after", toJson after),
      ("target", toJson (remoteInterruptTarget before after))]
  IO.println (toJson cases).compress
