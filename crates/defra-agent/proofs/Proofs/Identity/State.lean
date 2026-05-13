/-!
# Identity — State

Records and well-formedness for the `AgentPrincipal` /
`AgentBehavior` / `AgentDeployment` split (#185).
-/

namespace Identity

abbrev DID := String
abbrev BehaviorId := String
abbrev DeploymentId := String

structure Principal where
  did         : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Behavior where
  id          : BehaviorId
  principal   : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Deployment where
  id        : DeploymentId
  principal : DID
  hostId    : String
  enabled   : Bool
  deriving DecidableEq, Repr

end Identity
