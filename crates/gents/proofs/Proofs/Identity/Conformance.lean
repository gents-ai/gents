import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Conformance.ContractCases

namespace Identity.Conformance

def amyDid : String :=
  "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"

def ruminationDid : String :=
  "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"

def ghostDid : String :=
  "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH"

structure PrincipalCase where
  did     : String
  enabled : Bool
  deriving Repr

structure BehaviorCase where
  id        : String
  principal : String
  enabled   : Bool
  deriving Repr

structure DeploymentCase where
  id        : String
  principal : String
  hostId    : String
  enabled   : Bool
  deriving Repr

structure PermissionGrantCase where
  principal  : String
  permission : String
  deriving Repr

structure IdentityStructuralCase where
  name        : String
  principals  : List PrincipalCase
  behaviors   : List BehaviorCase
  deployments : List DeploymentCase
  wellFormed  : Bool
  deriving Repr

def structuralCases : List IdentityStructuralCase :=
  [ { name        := "amy_general_and_amy_code_share_principal"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := amyDid, enabled := true }
        , { id := "amy-code",    principal := amyDid, enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := amyDid
          , hostId := "host-1.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "amy_rumination_separate_principal"
    , principals  :=
        [ { did := amyDid,        enabled := true }
        , { did := ruminationDid, enabled := true } ]
    , behaviors   :=
        [ { id := "amy-general",     principal := amyDid,        enabled := true }
        , { id := "amy-rumination",  principal := ruminationDid, enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := amyDid
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := ruminationDid
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "dangling_behavior_fk_violates"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "orphan", principal := ghostDid, enabled := true } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "duplicate_behavior_id_violates"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := amyDid, enabled := true }
        , { id := "amy-general", principal := amyDid, enabled := false } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "deployment_fk_violates"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   := []
    , deployments :=
        [ { id := "ghost-deploy"
          , principal := ghostDid
          , hostId := "host-3.local"
          , enabled := true } ]
    , wellFormed  := false
    }
  , { name        := "two_deployments_different_principals_ok"
    , principals  :=
        [ { did := amyDid,        enabled := true }
        , { did := ruminationDid, enabled := true } ]
    , behaviors   := []
    , deployments :=
        [ { id := "deploy-amy"
          , principal := amyDid
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := ruminationDid
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  ]

structure IdentityPermissionCase where
  name                     : String
  principals               : List PrincipalCase
  behaviors                : List BehaviorCase
  deployments              : List DeploymentCase
  grants                   : List PermissionGrantCase
  permission               : String
  rowOwner                 : String
  actorBehavior            : String
  peerBehavior             : String
  expectedActorPrincipal   : String
  expectedPeerPrincipal    : String
  expectedActorAllowed     : Bool
  expectedPeerAllowed      : Bool
  samePrincipal            : Bool
  expectedDecisionsEqual   : Bool
  hostDeployment           : String
  expectedActorHostable    : Bool
  expectedPeerHostable     : Bool
  deriving Repr

def behaviorCaseToBehavior (c : BehaviorCase) : Behavior :=
  { id := c.id, principal := c.principal, displayName := none, enabled := c.enabled }

def deploymentCaseToDeployment (c : DeploymentCase) : Deployment :=
  { id := c.id, principal := c.principal, hostId := c.hostId, enabled := c.enabled }

def grantStoreFromCases (grants : List PermissionGrantCase) : GrantStore String :=
  { granted := fun principal permission =>
      grants.any (fun grant =>
        grant.principal == principal && grant.permission == permission) }

def permissionDecideFromGrants (grants : List PermissionGrantCase) :
    Decide String :=
  canonicalDecide (grantStoreFromCases grants)

theorem permissionDecideFromGrants_respectsPrincipal
    (grants : List PermissionGrantCase) :
    RespectsPrincipal (permissionDecideFromGrants grants) :=
  canonicalDecide_respectsPrincipal (grantStoreFromCases grants)

def findBehavior? : List BehaviorCase → BehaviorId → Option BehaviorCase
  | [], _ => none
  | behavior :: rest, id =>
      if behavior.id == id then
        some behavior
      else
        findBehavior? rest id

def findDeployment? : List DeploymentCase → DeploymentId → Option DeploymentCase
  | [], _ => none
  | deployment :: rest, id =>
      if deployment.id == id then
        some deployment
      else
        findDeployment? rest id

def behaviorForId (behaviors : List BehaviorCase) (id : BehaviorId) :
    BehaviorCase :=
  match findBehavior? behaviors id with
  | some behavior => behavior
  | none => { id := id, principal := "", enabled := false }

def deploymentForId
    (deployments : List DeploymentCase) (id : DeploymentId) :
    DeploymentCase :=
  match findDeployment? deployments id with
  | some deployment => deployment
  | none => { id := id, principal := "", hostId := "", enabled := false }

def permissionDecision
    (grants : List PermissionGrantCase)
    (behavior : BehaviorCase)
    (permission : String) : Bool :=
  permissionDecideFromGrants grants (behaviorCaseToBehavior behavior) permission

def hostabilityDecision
    (deployment : DeploymentCase) (behavior : BehaviorCase) : Bool :=
  (deploymentCaseToDeployment deployment).canHostBehavior
    (behaviorCaseToBehavior behavior)

def mkIdentityPermissionCase
    (name : String)
    (principals : List PrincipalCase)
    (behaviors : List BehaviorCase)
    (deployments : List DeploymentCase)
    (grants : List PermissionGrantCase)
    (permission rowOwner actorBehavior peerBehavior hostDeployment : String) :
    IdentityPermissionCase :=
  let actor := behaviorForId behaviors actorBehavior
  let peer := behaviorForId behaviors peerBehavior
  let host := deploymentForId deployments hostDeployment
  let actorAllowed := permissionDecision grants actor permission
  let peerAllowed := permissionDecision grants peer permission
  { name := name
  , principals := principals
  , behaviors := behaviors
  , deployments := deployments
  , grants := grants
  , permission := permission
  , rowOwner := rowOwner
  , actorBehavior := actorBehavior
  , peerBehavior := peerBehavior
  , expectedActorPrincipal := actor.principal
  , expectedPeerPrincipal := peer.principal
  , expectedActorAllowed := actorAllowed
  , expectedPeerAllowed := peerAllowed
  , samePrincipal := actor.principal == peer.principal
  , expectedDecisionsEqual := actorAllowed == peerAllowed
  , hostDeployment := hostDeployment
  , expectedActorHostable := hostabilityDecision host actor
  , expectedPeerHostable := hostabilityDecision host peer
  }

def amyPrincipal : PrincipalCase :=
  { did := amyDid, enabled := true }

def ruminationPrincipal : PrincipalCase :=
  { did := ruminationDid, enabled := true }

def amyGeneralBehavior : BehaviorCase :=
  { id := "amy-general", principal := amyDid, enabled := true }

def amyCodeBehavior : BehaviorCase :=
  { id := "amy-code", principal := amyDid, enabled := true }

def amyRuminationBehavior : BehaviorCase :=
  { id := "amy-rumination", principal := ruminationDid, enabled := true }

def amyDeployment : DeploymentCase :=
  { id := "deploy-amy"
  , principal := amyDid
  , hostId := "host-1.local"
  , enabled := true
  }

def ruminationDeployment : DeploymentCase :=
  { id := "deploy-rumination"
  , principal := ruminationDid
  , hostId := "host-2.local"
  , enabled := true
  }

def amyRowReadPermission : String :=
  "row:" ++ amyDid ++ ":memory.read"

def ruminationRowReadPermission : String :=
  "row:" ++ ruminationDid ++ ":journal.read"

def grant (principal permission : String) : PermissionGrantCase :=
  { principal := principal, permission := permission }

def identityPermissionCases : List IdentityPermissionCase :=
  [ mkIdentityPermissionCase
      "same_principal_row_owner_grant_allows_shared_behaviors"
      [amyPrincipal]
      [amyGeneralBehavior, amyCodeBehavior]
      [amyDeployment]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      "amy-general"
      "amy-code"
      "deploy-amy"
  , mkIdentityPermissionCase
      "separate_principal_without_grant_blocks_peer"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyRuminationBehavior]
      [amyDeployment, ruminationDeployment]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      "amy-general"
      "amy-rumination"
      "deploy-amy"
  , mkIdentityPermissionCase
      "separate_principal_with_grant_allows_peer"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyRuminationBehavior]
      [amyDeployment, ruminationDeployment]
      [ grant amyDid amyRowReadPermission
      , grant ruminationDid amyRowReadPermission ]
      amyRowReadPermission
      amyDid
      "amy-general"
      "amy-rumination"
      "deploy-rumination"
  , mkIdentityPermissionCase
      "behavior_id_lookup_selects_declared_principal"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyCodeBehavior, amyRuminationBehavior]
      [amyDeployment, ruminationDeployment]
      [grant ruminationDid ruminationRowReadPermission]
      ruminationRowReadPermission
      ruminationDid
      "amy-code"
      "amy-rumination"
      "deploy-rumination"
  ]

theorem identityPermissionCases_count :
    identityPermissionCases.length = 4 := rfl

def stringListContains (values : List String) (value : String) : Bool :=
  values.any (fun candidate => candidate == value)

def identityPermissionCaseReferencesDeclared
    (c : IdentityPermissionCase) : Bool :=
  let behaviorIds := c.behaviors.map (fun behavior => behavior.id)
  let deploymentIds := c.deployments.map (fun deployment => deployment.id)
  stringListContains behaviorIds c.actorBehavior &&
    stringListContains behaviorIds c.peerBehavior &&
    stringListContains deploymentIds c.hostDeployment

theorem identityPermissionCases_reference_declared_ids :
    identityPermissionCases.all identityPermissionCaseReferencesDeclared = true := rfl

open Conformance.Contracts
open Conformance.ContractCases (boolString)

def principalCaseJson (c : PrincipalCase) : String :=
  "{"
    ++ "\"did\":" ++ jsonString c.did ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def behaviorCaseJson (c : BehaviorCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def deploymentCaseJson (c : DeploymentCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"host_id\":" ++ jsonString c.hostId ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def permissionGrantCaseJson (c : PermissionGrantCase) : String :=
  "{"
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"permission\":" ++ jsonString c.permission
    ++ "}"

def identityStructuralCaseJson (c : IdentityStructuralCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"deployments\":" ++ jsonArray (c.deployments.map deploymentCaseJson) ++ ","
    ++ "\"well_formed\":" ++ boolString c.wellFormed
    ++ "}"

def structuralCasesJson : String :=
  jsonArray (structuralCases.map identityStructuralCaseJson)

def identityPermissionCaseJson (c : IdentityPermissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"deployments\":" ++ jsonArray (c.deployments.map deploymentCaseJson) ++ ","
    ++ "\"grants\":" ++ jsonArray (c.grants.map permissionGrantCaseJson) ++ ","
    ++ "\"permission\":" ++ jsonString c.permission ++ ","
    ++ "\"row_owner\":" ++ jsonString c.rowOwner ++ ","
    ++ "\"actor_behavior\":" ++ jsonString c.actorBehavior ++ ","
    ++ "\"peer_behavior\":" ++ jsonString c.peerBehavior ++ ","
    ++ "\"expected_actor_principal\":"
      ++ jsonString c.expectedActorPrincipal ++ ","
    ++ "\"expected_peer_principal\":"
      ++ jsonString c.expectedPeerPrincipal ++ ","
    ++ "\"expected_actor_allowed\":"
      ++ boolString c.expectedActorAllowed ++ ","
    ++ "\"expected_peer_allowed\":"
      ++ boolString c.expectedPeerAllowed ++ ","
    ++ "\"same_principal\":" ++ boolString c.samePrincipal ++ ","
    ++ "\"expected_decisions_equal\":"
      ++ boolString c.expectedDecisionsEqual ++ ","
    ++ "\"host_deployment\":" ++ jsonString c.hostDeployment ++ ","
    ++ "\"expected_actor_hostable\":"
      ++ boolString c.expectedActorHostable ++ ","
    ++ "\"expected_peer_hostable\":"
      ++ boolString c.expectedPeerHostable
    ++ "}"

def identityPermissionCasesJson : String :=
  jsonArray (identityPermissionCases.map identityPermissionCaseJson)

structure IdentityContract where
  name      : String
  statement : String
  enforced  : Bool
  trackedBy : String
  deriving Repr

def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "The runtime's behavior_id -> agent_did resolution is " ++
        "single-valued: for any two AgentBehavior rows b1, b2 with " ++
        "b1.agent_did == b2.agent_did, the runtime supplies the same " ++
        "Identity::Authenticated(did) as the actor for any DefraDB ACP " ++
        "check, so any DID-keyed permission decision returns identical " ++
        "results."
    , enforced  := true
    , trackedBy := "#193"
    }
  ]

def identityContractJson (c : IdentityContract) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"statement\":" ++ jsonString c.statement ++ ","
    ++ "\"enforced\":" ++ boolString c.enforced ++ ","
    ++ "\"tracked_by\":" ++ jsonString c.trackedBy
    ++ "}"

def identityContractsJson : String :=
  jsonArray (identityContracts.map identityContractJson)

end Identity.Conformance
