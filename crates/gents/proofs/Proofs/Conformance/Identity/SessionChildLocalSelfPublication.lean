import Proofs.Store.Identity.SessionChild
import Proofs.Conformance.Family

namespace Conformance.SessionChildLocalSelfPublication

open Lean Identity.SessionChild

structure LocalSelfPublicationInput where
  actor : String
  requester : String
  signer : String
  agent : String
  deriving ToJson

@[conformance_family]
def cases : Family LocalSelfPublicationInput Bool where
  inputs := localSelfPublicationCases.map fun (actor, requester, signer, agent) =>
    { actor, requester, signer, agent }
  model := fun i => localSelfPublicationAllowed i.actor i.requester i.signer i.agent

end Conformance.SessionChildLocalSelfPublication
