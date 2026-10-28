import Proofs.ToolExecution.Footprint
import Lean

open Lean ToolExecution.Footprint

private def samples : List (String × Effects) :=
  [("unknown", .unknown), ("read_a", .known ["a"] []),
   ("read_b", .known ["b"] []), ("write_a", .known [] ["a"]),
   ("write_b", .known [] ["b"])]

def main : IO Unit := do
  let cases := [false, true].flatMap fun safeHooks =>
    samples.flatMap fun first =>
    samples.flatMap fun second =>
      samples.map fun third =>
        let calls := [first.2, second.2, third.2]
        Json.mkObj [
          ("safeHooks", toJson safeHooks),
          ("calls", toJson [first.1, second.1, third.1]),
          ("batchLength", toJson (nextBatch safeHooks calls).1.length)]
  IO.println (toJson cases).compress
