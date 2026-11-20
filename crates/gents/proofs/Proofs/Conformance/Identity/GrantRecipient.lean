import Proofs.Store.Identity.GrantSetsConformance
import Proofs.Conformance.Functions.Store
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for Recipient

attribute [conformance_vocabulary] Recipient.name

end Identity.GrantSets

namespace Conformance.IdentityGrantRecipient

open Lean Identity.GrantSets Identity.GrantSetsConformance

structure Input where
  recipient : Recipient
  deriving ToJson

structure Grants where
  direct : List GrantSet
  projected : List GrantSet
  deriving ToJson

@[conformance_family]
def sets : Family Input Grants where
  inputs := recipients.map fun recipient => { recipient }
  model := fun i => { direct := directSets i.recipient, projected := projectedSets i.recipient }

end Conformance.IdentityGrantRecipient
