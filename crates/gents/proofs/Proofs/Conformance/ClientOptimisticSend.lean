import Proofs.Clients.ClientShell.OptimisticSend

/-- The optimistic send's views and generated traces, executed from the Lean
owner for the desktop's `chatDeliveryView` binding and the native
idempotent-send replay. -/
def main : IO Unit := IO.println ClientOptimisticSend.casesJson
