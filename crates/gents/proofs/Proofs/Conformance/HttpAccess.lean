import Proofs.RuntimeReconcile.HttpAccess
import Lean

open Lean RuntimeReconcile.HttpAccess

private def identityName : Identity → String
  | .anonymous => "anonymous"
  | .other => "other"
  | .operator => "operator"

private def decisionName : Decision → String
  | .misdirected => "misdirected"
  | .unauthorized => "unauthorized"
  | .forbidden => "forbidden"
  | .bootstrap => "bootstrap"
  | .operator => "operator"

def main : IO Unit := do
  let cases := [false, true].flatMap fun authorityMatches =>
    [Surface.bootstrap, .operator].flatMap fun surface =>
      [Identity.anonymous, .other, .operator].map fun identity =>
        Json.mkObj [
          ("authorityMatches", toJson authorityMatches),
          ("surface", toJson (if surface == .bootstrap then "bootstrap" else "operator")),
          ("identity", toJson (identityName identity)),
          ("decision", toJson (decisionName (decide authorityMatches surface identity)))]
  IO.println (toJson cases).compress
