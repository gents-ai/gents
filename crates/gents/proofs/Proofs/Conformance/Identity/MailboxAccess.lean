import Proofs.Store.Identity.MailboxAccess
import Proofs.Conformance.Family

namespace Conformance.MailboxAccess

open Lean Identity.MailboxAccess

structure SweepActorInput where
  actor : String
  deriving ToJson

@[conformance_family]
def sweepActors : Family SweepActorInput Bool where
  inputs := ["agent", "other", "node"].map fun actor => { actor }
  model := fun i => sweepVisible i.actor "agent"

end Conformance.MailboxAccess
