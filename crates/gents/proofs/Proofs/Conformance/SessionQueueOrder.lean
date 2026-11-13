import Proofs.Conformance.SessionQueueOrderCases

def main : IO Unit :=
  let family := Conformance.SessionQueueOrder.arrival
  IO.println (Lean.toJson (family.inputs.map fun i =>
    Lean.Json.mkObj [("input", Lean.toJson i), ("expected", Lean.toJson (family.model i))])).compress
