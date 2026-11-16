import Proofs.Conformance.ClientRequestClosureCases

def main : IO Unit :=
  let family := Conformance.ClientRequestClosure.closed
  IO.println (Lean.toJson (family.inputs.map fun i =>
    Lean.Json.mkObj [("input", Lean.toJson i), ("expected", Lean.toJson (family.model i))])).compress
