import Proofs.Identity.OwnerStaged
import Lean

open Lean Identity.OwnerStaged

private def callerFor (caller : String) (signer : String) : Option String :=
  match caller with
  | "signer" => some signer
  | "other" => some "admin"
  | _ => none

/-- Cases for the runtime's owner-staged route over real signed HTTP: the
pairing cases, route admission of a Node owner's command, and one HTTP apply
per requester and direction. -/
def main : IO Unit := do
  let commands := [true, false].flatMap fun authorityMatches =>
    ["signer", "other", "anonymous"].flatMap fun caller =>
      [true, false].map fun signatureValid =>
        Json.mkObj [
          ("authorityMatches", toJson authorityMatches),
          ("caller", toJson caller),
          ("signatureValid", toJson signatureValid),
          ("decision", toJson (overHttp authorityMatches (callerFor caller "user") "user"
            signatureValid unlinkedPairing "runtime" agentsOf runtimeNode routeReader).name)]
  IO.println (Json.mkObj [("pairing", toJson pairingCasesJson), ("commands", toJson commands),
    ("http_apply", toJson httpApplyCasesJson)]).compress
