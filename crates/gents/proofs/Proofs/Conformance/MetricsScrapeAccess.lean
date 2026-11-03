import Proofs.RuntimeReconcile.MetricsScrapeAccess
import Lean

open Lean RuntimeReconcile.MetricsScrapeAccess

private def bindName : Bind → String
  | .loopback => "loopback"
  | .interface => "interface"
  | .unspecified => "unspecified"

private def decisionName : Decision → String
  | .misdirected => "misdirected"
  | .unauthorized => "unauthorized"
  | .metrics => "metrics"

def main : IO Unit := do
  let binds := [Bind.loopback, .interface, .unspecified].flatMap fun bind =>
    [false, true].map fun tokenConfigured =>
      Json.mkObj [
        ("bind", toJson (bindName bind)),
        ("tokenConfigured", toJson tokenConfigured),
        ("allowed", toJson (bindAllowed bind tokenConfigured))]
  let requests := [false, true].flatMap fun authorityMatches =>
    [false, true].flatMap fun tokenConfigured =>
      [false, true].map fun tokenPresented =>
        Json.mkObj [
          ("authorityMatches", toJson authorityMatches),
          ("tokenConfigured", toJson tokenConfigured),
          ("tokenPresented", toJson tokenPresented),
          ("decision", toJson (decisionName (decide authorityMatches tokenConfigured tokenPresented)))]
  IO.println (Json.mkObj [("binds", toJson binds), ("requests", toJson requests)]).compress
