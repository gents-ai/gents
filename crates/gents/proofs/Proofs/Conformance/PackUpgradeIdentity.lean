import Proofs.Configuration
import Proofs.Conformance.ContractTypes

open Configuration Conformance.Contracts

def main : IO Unit := do
  let cases := [false, true].flatMap fun supplied =>
    [false, true].map fun matchesInstalled =>
      "{\"supplied\":" ++ toString supplied ++
      ",\"matches_installed\":" ++ toString matchesInstalled ++
      ",\"allowed\":" ++ toString (packUpgradeIdentityAllowed supplied matchesInstalled) ++ "}"
  IO.println (jsonArray cases)
