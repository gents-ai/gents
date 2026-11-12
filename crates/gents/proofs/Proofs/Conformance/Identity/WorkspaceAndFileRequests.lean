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

structure CallbackActorInput where
  actor : String
  accessActor : String
  selectedAgent : String
  deriving ToJson

structure CallbackActorOutput where
  expected : Bool
  workspaceLoaded : Bool
  repositoryLoaded : Bool
  deriving ToJson

@[conformance_family]
def callbackActor : Family CallbackActorInput CallbackActorOutput where
  inputs := [("agent", "agent", "agent"), ("node", "node", "agent"), ("empty", "", "")].map
    fun (actor, accessActor, selectedAgent) => { actor, accessActor, selectedAgent }
  model := fun i =>
    { expected := callbackActorMatches i.accessActor i.selectedAgent
      workspaceLoaded := callbackActorMatches i.accessActor i.selectedAgent
      repositoryLoaded := callbackActorMatches i.accessActor i.selectedAgent }

end Conformance.WorkspaceAndFileRequests
