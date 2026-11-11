import Proofs.RuntimeReconcile.HttpAccess
import Lean

open Lean RuntimeReconcile.HttpAccess

private def identityName : Identity → String
  | .anonymous => "anonymous"
  | .other => "other"
  | .operator => "operator"
  | .admin => "admin"

private def decisionName : Decision → String
  | .misdirected => "misdirected"
  | .unauthorized => "unauthorized"
  | .forbidden => "forbidden"
  | .bootstrap => "bootstrap"
  | .operator => "operator"

def main : IO Unit := do
  let cases := [false, true].flatMap fun authorityMatches =>
    [Surface.bootstrap, .operator, .snapshot].flatMap fun surface =>
      [Identity.anonymous, .other, .operator, .admin].map fun identity =>
        Json.mkObj [
          ("authorityMatches", toJson authorityMatches),
          ("surface", toJson (match surface with
            | .bootstrap => "bootstrap" | .operator => "operator" | .snapshot => "snapshot")),
          ("identity", toJson (identityName identity)),
          ("decision", toJson (decisionName (decide authorityMatches surface identity))),
          ("readsAs", toJson ((readsAs authorityMatches surface identity).map identityName))]
  IO.println (toJson cases).compress
