import Proofs.PluginJob.Authority
import Lean

open Lean PluginJob

def main : IO Unit := do
  let doors := [(PageDoor.packRecord, "pack"), (PageDoor.document, "document")]
  let cases := doors.flatMap fun (door, name) =>
    [false, true].flatMap fun first =>
      [false, true].map fun enabled =>
        Json.mkObj [("door", toJson name), ("first", toJson first), ("enabled", toJson enabled),
          ("loadable", toJson (pageLoadable door first enabled)),
          ("data_call", toJson (pageDataCallAllowed door first enabled))]
  IO.println (toJson cases).compress
