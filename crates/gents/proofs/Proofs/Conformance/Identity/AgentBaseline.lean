import Proofs.Store.Identity.GrantSetsConformance
import Proofs.Conformance.Family

namespace Conformance.AgentBaseline

open Lean Identity.GrantSets Identity.GrantSetsConformance

deriving instance ToJson for Step

structure StepObservation where
  action : String
  committed : Bool
  installed : List String
  deriving ToJson

def observe (actor : Holder) (installed : List String) : List Step → List StepObservation
  | [] => []
  | step :: rest =>
      let result := projectBaseline actor step.after installed
      let next := result.getD installed
      { action := step.name, committed := result.isSome, installed := next }
        :: observe actor next rest

def holderNamed (name : String) : Holder :=
  ((recipients.find? (·.name == name)).map fun r => holderOf (receives r)).getD actorless

structure LifecycleInput where
  actor : String
  actions : List Step
  deriving ToJson

structure LifecycleOutput where
  steps : List StepObservation
  deriving ToJson

@[conformance_family]
def lifecycle : Family LifecycleInput LifecycleOutput where
  inputs := baselineCases.map fun (_, actor, _, actions) => { actor, actions }
  model := fun i => { steps := observe (holderNamed i.actor) [] i.actions }

structure InstallInput where
  actor : String
  deriving ToJson

structure InstallOutput where
  installed : List String
  deriving ToJson

/-- A pack installation publishes its Agent as the enrolled user who installs
the pack, so an install reaches only that actor's publish step. -/
@[conformance_family]
def packInstall : Family InstallInput InstallOutput where
  inputs := [{ actor := Recipient.enrolledUser.name }]
  model := fun i =>
    { installed := ((observe (holderNamed i.actor) [] [.publish]).head?.map (·.installed)).getD [] }

example : baselineCases.all fun (_, actor, holder, _) => holderNamed actor == holder := by
  native_decide

end Conformance.AgentBaseline
