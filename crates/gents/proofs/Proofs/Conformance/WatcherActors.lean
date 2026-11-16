import Proofs.Automation.EventDelivery.WatcherConformance
import Proofs.Conformance.Family

namespace Conformance.WatcherActors

open Lean EventDelivery.Watcher EventDelivery.WatcherConformance

deriving instance ToJson for ActorCandidate

structure ScanInput where
  name : String
  candidates : List ActorCandidate
  deriving ToJson

private def scanInput (c : String × List ActorCandidate) : ScanInput :=
  { name := c.1, candidates := c.2 }

/-- A runtime view with one configured Agent can be disabled and can lose its
identity resolution, so these are the candidate sets one served Agent forms. -/
def servedNames : List String := ["disabled-resolved", "disabled-unresolved"]

@[conformance_family]
def scanned : Family ScanInput (List String) where
  inputs := (actorCases.filter fun c => c.1 ∈ servedNames).map scanInput
  model := fun i => scanActors i.candidates

/-- model-production-gap: the served runtime fixture provisions one Agent, so a
view mixing an available, an unavailable, an unresolved and an unconfigured
Agent is not built by any production test yet. -/
@[conformance_family]
def scannedUnreached : Family ScanInput (List String) where
  inputs := (actorCases.filter fun c => c.1 ∉ servedNames).map scanInput
  model := fun i => scanActors i.candidates

end Conformance.WatcherActors
