import Proofs.Shared.ToolExecution.Parallel
open ToolExecution.Parallel

def main : IO Unit := do
  let calls : List Call := [⟨0, .read⟩, ⟨1, .read⟩, ⟨2, .barrier⟩, ⟨3, .read⟩]
  let rows := (List.range 5).map fun offset =>
    s!"i = {offset + 1} -> {effectWidth 4 (calls.drop offset)}"
  IO.println "---- MODULE ScriptEffectBatchCases ----"
  IO.println ("WidthAt(i) == CASE " ++ String.intercalate " [] " rows)
  IO.println "===="
