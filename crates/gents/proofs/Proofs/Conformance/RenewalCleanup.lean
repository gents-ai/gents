import Proofs.Enrollment.AutoRenewal
import Proofs.Conformance.ContractTypes

open Conformance.Contracts

def main : IO Unit := do
  let cases := [false, true].flatMap fun owned =>
    [false, true].flatMap fun verified =>
      [0, 1, 2].map fun expires =>
        "{\"owned\":" ++ toString owned ++ ",\"verified\":" ++ toString verified ++
        ",\"expires\":" ++ toString expires ++ ",\"now\":1,\"allowed\":" ++
        toString (Enrollment.AutoRenewal.mayPruneRenewal owned verified expires 1) ++ "}"
  IO.println (jsonArray cases)
