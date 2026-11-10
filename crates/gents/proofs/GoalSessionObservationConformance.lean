import Proofs.Conformance.GoalSessionObservation

open Conformance.GoalSessionObservationContracts in
def main : IO Unit :=
  IO.println ("{\"cases\":" ++ casesJson ++ ",\"fold_cases\":" ++ foldCasesJson ++ "}")
