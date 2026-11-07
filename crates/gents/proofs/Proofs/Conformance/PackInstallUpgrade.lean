import Proofs.Configuration
import Proofs.Conformance.ContractTypes

open Configuration Conformance.Contracts

def main : IO Unit := do
  let cases := [none, some "old"].flatMap fun previous =>
    [none, some "new"].map fun supplied =>
      "{\"previous\":" ++ jsonOptionalString previous ++
      ",\"supplied\":" ++ jsonOptionalString supplied ++
      ",\"result\":" ++ jsonOptionalString (resolvePackUpgradeVariable previous supplied) ++ "}"
  IO.println (jsonArray cases)
