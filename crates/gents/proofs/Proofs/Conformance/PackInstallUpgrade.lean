import Proofs.Configuration
import Proofs.Conformance.ContractTypes

open Configuration Conformance.Contracts

def main : IO Unit := do
  let cases := [none, some "old"].flatMap fun previous =>
    [false, true].map fun supplied =>
      "{\"previous\":" ++ jsonOptionalString previous ++
      ",\"supplied\":" ++ (if supplied then "true" else "false") ++
      ",\"result\":" ++ jsonString (resolvePackUpgradeField previous supplied "new") ++ "}"
  IO.println (jsonArray cases)
