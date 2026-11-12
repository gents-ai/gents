import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceAndFileRequests

open Lean Identity Identity.WorkspaceAndFileRequests

structure CompletionActorInput where
  actor : String
  held : String
  selected : String
  deriving ToJson

@[conformance_family]
def completionActor : Family CompletionActorInput Bool where
  inputs := [("agent", "agent", "agent"), ("node", "node", "agent"), ("empty", "", "")].map
    fun (actor, held, selected) => { actor, held, selected }
  model := fun i => completionActorMatches i.held i.selected

end Conformance.WorkspaceAndFileRequests
