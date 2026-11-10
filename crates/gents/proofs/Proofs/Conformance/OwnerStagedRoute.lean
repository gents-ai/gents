import Proofs.Identity.OwnerStaged
import Lean

open Lean Identity.OwnerStaged

private def callerFor (caller : String) (signer : String) : Option String :=
  match caller with
  | "signer" => some signer
  | "other" => some "admin"
  | _ => none

def main : IO Unit := do
  let commands := [true, false].flatMap fun authorityMatches =>
    ["signer", "other", "anonymous"].flatMap fun caller =>
      [true, false].map fun signatureValid =>
        Json.mkObj [
          ("authorityMatches", toJson authorityMatches),
          ("caller", toJson caller),
          ("signatureValid", toJson signatureValid),
          ("decision", toJson (pairingOverHttp authorityMatches (callerFor caller "user") "user"
            signatureValid unlinkedPairing "runtime" agentsOf (some "node") routeReader).name)]
  IO.println (Json.mkObj [("pairing", toJson pairingCasesJson), ("commands", toJson commands)]).compress
