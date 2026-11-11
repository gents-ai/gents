import Proofs.Store.Identity.MailboxAccess

def main : IO Unit := IO.print (Identity.Household.policyYamlOf Identity.MailboxAccess.candidatePolicy)
