import Proofs.Configuration
import Proofs.Conformance.ContractTypes

open Configuration Conformance.Contracts

def main : IO Unit := do
  let cases := [false, true].flatMap fun affected =>
    [none, some "new first"].map fun replacement =>
      "{\"affected\":" ++ (if affected then "true" else "false") ++
      ",\"replacement\":" ++ jsonOptionalString replacement ++
      ",\"result\":" ++ jsonOptionalString (resolveInstalledPackField "old first" affected replacement) ++ "}"
  IO.println (jsonArray cases)
