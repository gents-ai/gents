import Proofs.Enrollment.AutoRenewal
import Proofs.Conformance.ContractTypes

open Conformance.Contracts

def main : IO Unit := do
  let cases := [1, 60, 3600, 86400, 604800].flatMap fun remaining =>
    [0, 1, 2, 5, 9].map fun attempts =>
      "{\"remaining\":" ++ toString remaining ++ ",\"attempts\":" ++ toString attempts ++
      ",\"delay\":" ++ toString (Enrollment.AutoRenewal.unansweredRetrySeconds remaining attempts) ++ "}"
  IO.println (jsonArray cases)
