import Proofs.ClientShell.ObservationOrdering

namespace Conformance.ClientLiveDeltaContracts

/-- Live cursor acceptance cases evaluated by the executable Lean owner. -/
def casesJson : String :=
  let cursors : List (Option Nat) := [none, some 0, some 1]
  let encodeCursor := fun (value : Option Nat) => value.elim "null" toString
  let rows := cursors.flatMap fun base => cursors.flatMap fun current =>
    [false, true].map fun terminal =>
      "{\"base\":" ++ encodeCursor base ++ ",\"current\":" ++ encodeCursor current ++
      ",\"terminal\":" ++ toString terminal ++ ",\"accepted\":" ++
      toString (ClientLiveDelta.accepts base current terminal) ++ "}"
  "[" ++ String.intercalate "," rows ++ "]"

end Conformance.ClientLiveDeltaContracts
