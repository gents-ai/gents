import Proofs.Session.ExecutionActivity
import Lean

open Lean Session.ExecutionActivity

def main : IO Unit := do
  let active : Identity := ⟨"request", "generation"⟩
  let other : Identity := ⟨"request", "other"⟩
  let cases := [false, true].flatMap fun currentSame =>
    [false, true].flatMap fun incomingSame =>
      [1, 2, 3].flatMap fun sequence =>
        [0, 1, 2].flatMap fun turn =>
        [0, 1, 2].map fun attempt =>
          let current : Observation := ⟨if currentSame then active else other, 2, 1, 1, 10, .generating⟩
          let incoming : Observation := ⟨if incomingSame then active else other, sequence, turn, attempt, 11, .providerRetry⟩
          let accepted := accept active (some current) incoming == some incoming
          Json.mkObj [
            ("currentSame", toJson currentSame), ("incomingSame", toJson incomingSame),
            ("sequence", toJson sequence), ("turn", toJson turn), ("attempt", toJson attempt), ("accepted", toJson accepted)]
  IO.println (toJson cases).compress
