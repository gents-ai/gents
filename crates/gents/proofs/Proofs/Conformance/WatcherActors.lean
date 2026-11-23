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

/-- A runtime view holds as many Agents as a case has candidates; each can be
disabled, owned by another node, lose its identity resolution or name a backend
it cannot run, so the served view forms every candidate set. -/
@[conformance_family]
def scanned : Family ScanInput (List String) where
  inputs := actorCases.map scanInput
  model := fun i => scanActors i.candidates

end Conformance.WatcherActors
