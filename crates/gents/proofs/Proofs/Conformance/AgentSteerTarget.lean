import Proofs.Session.FoldCases
import Proofs.Session.Management
import Lean

open Lean SessionQueue

def main : IO Unit := do
  let active : QueueEntry := { FoldCases.user 1 with requestDocId := some 20 }
  let cases := [none, some 20, some 21].map fun target =>
    let candidate : QueueEntry := { FoldCases.user 2 (some 1) with
      delivery := .steer, source := .steering, steerTarget := target }
    Json.mkObj [
      ("headRequest", toJson (1 : Nat)),
      ("headDocument", toJson (20 : Nat)),
      ("targetDocument", toJson target),
      ("accepted", toJson (decide (steersInto active candidate)))]
  IO.println (toJson cases).compress
