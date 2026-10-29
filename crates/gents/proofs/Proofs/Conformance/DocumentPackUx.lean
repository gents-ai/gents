import Proofs.PluginJob.Authority
import Lean

open Lean PluginJob ToolPolicy

def main : IO Unit := do
  let cases := [false, true].flatMap fun hasUx =>
    [false, true].flatMap fun home =>
      [false, true].map fun operator =>
        let via := if operator then InstallProvenance.operator else InstallProvenance.selfConfig
        Json.mkObj [("hasUx", toJson hasUx), ("home", toJson home),
          ("operator", toJson operator), ("allowed", toJson (documentUxAllowed hasUx home via))]
  IO.println (toJson cases).compress
