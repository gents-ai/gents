import Proofs.PluginJob.Authority
import Lean

open Lean PluginJob

def main : IO Unit := do
  let cases := [false, true].flatMap fun hasUx =>
    [false, true].map fun home =>
      Json.mkObj [("hasUx", toJson hasUx), ("home", toJson home),
        ("allowed", toJson (documentUxAllowed hasUx home))]
  IO.println (toJson cases).compress
