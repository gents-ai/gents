import Proofs.ClientShell.ObservationOrdering

open ClientObservationOrdering

/-- Expected guard outcomes are evaluated from the executable Lean owner. -/
def main : IO Unit := do
  let rows := (List.range 5).flatMap fun current =>
    (List.range 5).map fun captured =>
      "{\"current\":" ++ toString current ++
      ",\"captured\":" ++ toString captured ++
      ",\"accepted\":" ++ (if accepts current captured then "true" else "false") ++ "}"
  IO.println ("[" ++ String.intercalate "," rows ++ "]")
