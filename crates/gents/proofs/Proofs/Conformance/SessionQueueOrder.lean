import Proofs.Conformance.SessionQueueOrderCases

def main : IO Unit :=
  IO.println ("[" ++ String.intercalate "," (Conformance.SessionQueueOrder.scenarios.map Conformance.SessionQueueOrder.caseJson) ++ "]")
