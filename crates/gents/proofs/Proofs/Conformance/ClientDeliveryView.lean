import Proofs.ClientShell.SubmissionReceipt

/-- The per-send delivery view and affordances, executed from the Lean owner
for the desktop's `chatDeliveryView` binding. -/
def main : IO Unit := IO.println ClientSubmissionReceipt.deliveryViewCasesJson
