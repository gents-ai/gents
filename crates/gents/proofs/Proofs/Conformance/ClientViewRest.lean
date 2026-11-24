import Proofs.Clients.ClientShell.ViewRest

/-- Every case of the transcript view's rest decision, executed from the Lean
owner for the desktop's `viewRests` binding. -/
def main : IO Unit := IO.println ClientShell.ViewRest.casesJson
