import Proofs.Conformance.Contracts.Json.SelfConfig

def main : IO Unit :=
  IO.println (Lean.toJson (Conformance.Contracts.presetOutput SelfConfig.engineerGroup)).compress
