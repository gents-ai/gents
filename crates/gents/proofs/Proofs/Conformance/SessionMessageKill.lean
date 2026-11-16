import Proofs.ToolCalls.Recovery.Sweeps.SessionMessage
import Proofs.Conformance.Family

namespace Conformance.SessionMessageKill

open Lean Recovery

attribute [conformance_vocabulary] KillObservation.toContract
attribute [conformance_vocabulary] KillAction.toContract

deriving instance ToJson for KillObservation
deriving instance ToJson for KillAction
deriving instance ToJson for CausedFacts

structure KillInput where
  observation : KillObservation
  deriving ToJson

structure ActionOutput where
  action : KillAction
  deriving ToJson

structure DriveOutput where
  action : KillAction
  endsRowNow : Bool
  deriving ToJson

@[conformance_family]
def action : Family KillInput ActionOutput where
  inputs := killCases.map fun (observation, _) => { observation }
  model := fun i => { action := killAction i.observation }

@[conformance_family]
def drive : Family KillInput DriveOutput where
  inputs := killCases.map fun (observation, _) => { observation }
  model := fun i =>
    { action := killAction i.observation
      endsRowNow := (killAction i.observation).endsRowNow }

structure ObservationInput where
  caused : Option CausedFacts
  deriving ToJson

structure ObservationOutput where
  observation : KillObservation
  deriving ToJson

@[conformance_family]
def observed : Family ObservationInput ObservationOutput where
  inputs := killObservationCases.map fun (caused, _) => { caused }
  model := fun i => { observation := observeCaused i.caused }

end Conformance.SessionMessageKill
