import Proofs.ClientShell.InterruptControl
import Proofs.Conformance.ClientShell.Contracts.Types

namespace Conformance.ClientInterruptControl

open ClientShell.InterruptControl Conformance.ClientShellContracts

def phases : List Phase := [.idle, .submitting, .awaiting, .active, .blocked]

def turns : List (Option ClientTurnState) :=
  [none, some .waitingForClaim, some .running, some .completed,
    some .failed, some .superseded, some .interrupted]

def selections : List String :=
  phases.flatMap fun phase => [none, some 1].flatMap fun tracked =>
    [none, some 2].flatMap fun latest => turns.map fun turn =>
      "{\"phase\":" ++ Conformance.Contracts.jsonString (interruptPhaseName phase) ++
      ",\"tracked\":" ++ jsonNatOption tracked ++ ",\"latest\":" ++ jsonNatOption latest ++
      ",\"turn\":" ++ jsonStringOption (turn.map clientTurnStateName) ++
      ",\"visible\":" ++ boolJson (visible phase) ++
      ",\"target\":" ++ jsonNatOption (target tracked latest turn) ++ "}"

def stops : List String :=
  [false, true].flatMap fun inFlight => [none, some 1, some 2].flatMap fun request =>
    [none, some 1, some 2].flatMap fun latest => [false, true].flatMap fun observed =>
      [none, some 1, some 2].map fun requested =>
        "{\"inFlight\":" ++ boolJson inFlight ++ ",\"request\":" ++ jsonNatOption request ++
        ",\"latest\":" ++ jsonNatOption latest ++ ",\"observed\":" ++ boolJson observed ++
        ",\"requested\":" ++ jsonNatOption requested ++
        ",\"stopping\":" ++ boolJson (stopping inFlight request latest observed requested) ++ "}"

end Conformance.ClientInterruptControl

def main : IO Unit :=
  IO.println ("{\"selection\":[" ++ String.intercalate "," Conformance.ClientInterruptControl.selections ++
    "],\"stopping\":[" ++ String.intercalate "," Conformance.ClientInterruptControl.stops ++ "]}")
