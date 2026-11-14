import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceAndFileRequests

open Lean Identity Identity.WorkspaceAndFileRequests

deriving instance ToJson for Identity.WorkspaceAndFileRequests.WorkspaceAgentSelection

structure AgentSelectionInput where
  name : String
  configured : Option WorkspaceAgentSelection
  deriving ToJson

structure AgentSelectionOutput where
  accepted : Bool
  writer : Option String
  integrator : Option String
  deriving ToJson

@[conformance_family]
def agentSelection : Family AgentSelectionInput AgentSelectionOutput where
  inputs := [("missing", none), ("separate", some ⟨"writer", "integrator"⟩),
    ("same", some ⟨"writer", "writer"⟩), ("node", some ⟨"node", "integrator"⟩),
    ("blank", some ⟨"", "integrator"⟩)].map fun (name, configured) => { name, configured }
  model := fun i =>
    let selected := freezeWorkspaceAgents "node" i.configured
    { accepted := selected.isSome
      writer := selected.map WorkspaceAgentSelection.writer
      integrator := selected.map WorkspaceAgentSelection.integrator }

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

structure LivenessInput where
  visible : Bool
  terminal : Bool
  deriving ToJson

structure LivenessOutput where
  mayRelease : Bool
  deriving ToJson

@[conformance_family]
def liveness : Family LivenessInput LivenessOutput where
  inputs := [false, true].flatMap fun visible => [false, true].map fun terminal => { visible, terminal }
  model := fun i => { mayRelease := mayReleaseExclusive i.visible i.terminal }

structure ActorInput where
  actor : String
  deriving ToJson

structure QuickstartSelectionOutput where
  before : Bool
  afterRevocation : Bool
  deriving ToJson

@[conformance_family]
def quickstartSelection : Family ActorInput QuickstartSelectionOutput where
  inputs := ["user", "creator", "other"].map fun actor => { actor }
  model := fun i =>
    { before := Household.allowed 8 quickstartSelectedAgentFixture i.actor "agents" "target" "read"
      afterRevocation := Household.allowed 8 quickstartSelectedAgentRevoked i.actor "agents" "target" "read" }

structure QuickstartRepositoryOutput where
  before : Bool
  afterRevoke : Bool
  abortedDocumentVisible : Bool
  deriving ToJson

@[conformance_family]
def quickstartRepository : Family ActorInput QuickstartRepositoryOutput where
  inputs := ["user", "agent", "other"].map fun actor => { actor }
  model := fun i =>
    { before := quickstartRepositoryRead workspaceRepositoryFixture i.actor
      afterRevoke := quickstartRepositoryRead revokedWorkspaceRepository i.actor
      abortedDocumentVisible := (Identity.NodeFileRequests.publish false).document }

end Conformance.WorkspaceAndFileRequests
