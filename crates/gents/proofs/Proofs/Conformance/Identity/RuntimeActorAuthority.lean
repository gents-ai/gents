import Proofs.Store.Identity.RuntimeActorAuthority
import Proofs.Conformance.Family

namespace Conformance.RuntimeActorAuthority

open Lean Identity.RuntimeActorAuthority

/-- One selected-Agent runtime path (`RuntimePath.wire`) called as the hosting
runtime node key, the selected Agent, or a sibling Agent. -/
structure PrivateSessionInput where
  runtimePath : String
  actor : String
  deriving ToJson

/-- Whether the runtime authority gate admits the call, and whether the User's
private session or request is observed through that path. -/
structure PrivateSessionObserved where
  admitted : Bool
  observed : Bool
  deriving ToJson

private def actorOfWire : String → Option ActingPrincipal
  | "node_key" => some .nodeKey
  | "selected_agent" => some .selectedAgent
  | "sibling_agent" => some .otherActor
  | _ => none

def privateSessionModel (i : PrivateSessionInput) : PrivateSessionObserved :=
  match RuntimePath.selectedAgentPaths.find? (·.wire == i.runtimePath), actorOfWire i.actor with
  | some path, some actor =>
    { admitted := actorAdmitted path actor
    , observed := privateSessionObserved path actor false }
  | _, _ => { admitted := false, observed := false }

@[conformance_family]
def privateSessionAccess : Family PrivateSessionInput PrivateSessionObserved where
  inputs := RuntimePath.selectedAgentPaths.flatMap fun path =>
    ["node_key", "selected_agent", "sibling_agent"].map fun actor =>
      { runtimePath := path.wire, actor }
  model := privateSessionModel

end Conformance.RuntimeActorAuthority
